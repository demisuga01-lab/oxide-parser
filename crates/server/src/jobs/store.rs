//! Job state and the [`JobStore`] abstraction.
//!
//! The in-memory store ([`InMemoryJobStore`]) backs this round; the trait is the
//! seam for a future persistent/distributed backend.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::RwLock;
use std::time::{Duration, Instant};

use crate::processing::JobProgress;

/// Which heavy operation a job runs. Carries the already-parsed, validated
/// parameters captured at submit time so the worker can run the SAME core the
/// sync handler uses.
pub(crate) enum JobKind {
    Pdf2Img(crate::routes::pdf2img::Pdf2ImgParams),
    ExtractImages(crate::routes::extract_images::ExtractImagesParams),
}

impl JobKind {
    pub fn label(&self) -> &'static str {
        match self {
            JobKind::Pdf2Img(_) => "pdf2img",
            JobKind::ExtractImages(_) => "extract-images",
        }
    }
}

/// Lifecycle state of a job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobStatus {
    Queued,
    Running,
    Completed,
    Failed,
    /// Retention window elapsed; state/result reclaimed. Distinguished from
    /// "unknown id" only internally — externally both surface as 404.
    Expired,
}

impl JobStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            JobStatus::Queued => "queued",
            JobStatus::Running => "running",
            JobStatus::Completed => "completed",
            JobStatus::Failed => "failed",
            JobStatus::Expired => "expired",
        }
    }
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            JobStatus::Completed | JobStatus::Failed | JobStatus::Expired
        )
    }
}

/// A safe, client-facing error recorded on a failed job. Mirrors the
/// sanitization of the sync path: the worker classifies the
/// engine/server error into a stable code + non-leaking message, optionally
/// with a correlation reference for the internal path.
#[derive(Debug, Clone)]
pub struct JobError {
    pub code: &'static str,
    pub message: String,
    pub reference: Option<String>,
}

/// One job's full state.
pub struct Job {
    pub id: String,
    /// Submitting identity (API key, or "anonymous" when auth is disabled).
    /// Status/result retrieval is scoped to this value.
    pub owner: String,
    pub kind_label: &'static str,
    pub status: JobStatus,
    pub progress: Arc<JobProgress>,
    /// Set once completed: where the result bytes live on disk + the HTTP
    /// metadata (content-type, filename, extra headers) to replay on download.
    pub result: Option<JobResult>,
    pub error: Option<JobError>,
    /// When the job reached a terminal state — retention is measured from here.
    pub finished_at: Option<Instant>,
}

/// A completed job's result: a temp file on disk plus the response metadata.
#[derive(Clone)]
pub struct JobResult {
    pub path: PathBuf,
    pub content_type: &'static str,
    pub filename: &'static str,
    pub extra_headers: Vec<(&'static str, String)>,
    pub size_bytes: u64,
}

impl Job {
    fn new(id: String, owner: String, kind_label: &'static str) -> Self {
        Self {
            id,
            owner,
            kind_label,
            status: JobStatus::Queued,
            progress: Arc::new(JobProgress::default()),
            result: None,
            error: None,
            finished_at: None,
        }
    }
}

/// A lightweight snapshot of a job's externally-visible state, returned by the
/// store so handlers never hold the lock while building a response.
#[derive(Clone)]
pub struct JobSnapshot {
    pub id: String,
    pub owner: String,
    pub kind_label: &'static str,
    pub status: JobStatus,
    pub progress: (usize, usize),
    pub error: Option<JobError>,
    pub result: Option<JobResult>,
}

/// Storage abstraction for jobs. The in-memory implementation backs this round;
/// a persistent backend (DB/Redis) could implement the same trait later without
/// changing handlers or the worker.
///
/// Implementations must be safe to share across threads (`Send + Sync`): the
/// worker pool, the submit handlers, and the cleanup task all touch the store
/// concurrently.
pub trait JobStore: Send + Sync {
    /// Insert a freshly-created queued job owned by `owner`. Returns the new id,
    /// or `None` if the store is at its `max_jobs` cap (submission should then
    /// be rejected). The `progress` handle is returned to the caller so the
    /// worker can update it.
    fn create(&self, owner: String, kind_label: &'static str) -> Option<(String, Arc<JobProgress>)>;

    /// Snapshot a job's state if it exists.
    fn get(&self, id: &str) -> Option<JobSnapshot>;

    /// Mark a job running.
    fn mark_running(&self, id: &str);

    /// Mark a job completed with its result metadata.
    fn mark_completed(&self, id: &str, result: JobResult, now: Instant);

    /// Mark a job failed with a classified error.
    fn mark_failed(&self, id: &str, error: JobError, now: Instant);

    /// Remove jobs whose retention window has elapsed (relative to `now`) and
    /// return the result file paths of those removed, so the caller can delete
    /// them from disk. `retention` is the TTL; jobs with no `finished_at` (still
    /// queued/running) are never reaped here.
    fn reap_expired(&self, now: Instant, retention: Duration) -> Vec<PathBuf>;

    /// Total number of jobs currently retained (any status). For tests/metrics.
    fn len(&self) -> usize;

    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// In-memory job store: a single `RwLock<HashMap>`. Reads (status polling) take
/// the read lock; the handful of state transitions take the write lock briefly.
/// Bounded by `max_jobs` at `create` time so the map cannot grow without limit
/// even if submissions outpace retention cleanup.
pub struct InMemoryJobStore {
    jobs: RwLock<HashMap<String, Job>>,
    max_jobs: usize,
}

impl InMemoryJobStore {
    pub fn new(max_jobs: usize) -> Self {
        Self {
            jobs: RwLock::new(HashMap::new()),
            max_jobs: max_jobs.max(1),
        }
    }

    fn snapshot(job: &Job) -> JobSnapshot {
        JobSnapshot {
            id: job.id.clone(),
            owner: job.owner.clone(),
            kind_label: job.kind_label,
            status: job.status,
            progress: job.progress.snapshot(),
            error: job.error.clone(),
            result: job.result.clone(),
        }
    }
}

impl JobStore for InMemoryJobStore {
    fn create(
        &self,
        owner: String,
        kind_label: &'static str,
    ) -> Option<(String, Arc<JobProgress>)> {
        let mut map = self.jobs.write().unwrap_or_else(|e| e.into_inner());
        if map.len() >= self.max_jobs {
            // At capacity: refuse rather than grow unbounded. The cleanup task
            // reclaims terminal jobs on its own schedule; we do NOT evict a
            // live job to make room (that would lose a running result).
            return None;
        }
        let id = super::id::generate_job_id();
        let job = Job::new(id.clone(), owner, kind_label);
        let progress = Arc::clone(&job.progress);
        map.insert(id.clone(), job);
        Some((id, progress))
    }

    fn get(&self, id: &str) -> Option<JobSnapshot> {
        let map = self.jobs.read().unwrap_or_else(|e| e.into_inner());
        map.get(id).map(Self::snapshot)
    }

    fn mark_running(&self, id: &str) {
        let mut map = self.jobs.write().unwrap_or_else(|e| e.into_inner());
        if let Some(job) = map.get_mut(id) {
            job.status = JobStatus::Running;
        }
    }

    fn mark_completed(&self, id: &str, result: JobResult, now: Instant) {
        let mut map = self.jobs.write().unwrap_or_else(|e| e.into_inner());
        if let Some(job) = map.get_mut(id) {
            job.status = JobStatus::Completed;
            job.result = Some(result);
            job.finished_at = Some(now);
        }
    }

    fn mark_failed(&self, id: &str, error: JobError, now: Instant) {
        let mut map = self.jobs.write().unwrap_or_else(|e| e.into_inner());
        if let Some(job) = map.get_mut(id) {
            job.status = JobStatus::Failed;
            job.error = Some(error);
            job.finished_at = Some(now);
        }
    }

    fn reap_expired(&self, now: Instant, retention: Duration) -> Vec<PathBuf> {
        let mut map = self.jobs.write().unwrap_or_else(|e| e.into_inner());
        let mut reaped_paths = Vec::new();
        map.retain(|_, job| {
            match job.finished_at {
                Some(finished) if now.duration_since(finished) >= retention => {
                    if let Some(result) = &job.result {
                        reaped_paths.push(result.path.clone());
                    }
                    false // drop it
                }
                // Still within retention, or not yet terminal: keep.
                _ => true,
            }
        });
        reaped_paths
    }

    fn len(&self) -> usize {
        self.jobs.read().unwrap_or_else(|e| e.into_inner()).len()
    }
}

/// Payload carried on the bounded queue from a submit handler to a worker: the
/// job id (so the worker can update the store) and the work to run. The PDF
/// bytes live inside `kind`'s params.
pub struct QueuedJob {
    pub id: String,
    pub kind: JobKind,
    pub progress: Arc<JobProgress>,
}

// `Bytes` (inside the params) is cheaply cloneable / `Send`; assert the queued
// payload is `Send` so the channel can carry it across the worker boundary.
const _: () = {
    fn _assert_send<T: Send>() {}
    fn _check() {
        _assert_send::<QueuedJob>();
    }
};

#[cfg(test)]
mod tests {
    use super::*;

    fn make_store() -> InMemoryJobStore {
        InMemoryJobStore::new(100)
    }

    #[test]
    fn create_get_roundtrip() {
        let store = make_store();
        let (id, _progress) = store.create("owner-1".to_string(), "pdf2img").unwrap();
        let snap = store.get(&id).expect("job should exist");
        assert_eq!(snap.owner, "owner-1");
        assert_eq!(snap.status, JobStatus::Queued);
        assert_eq!(snap.kind_label, "pdf2img");
    }

    #[test]
    fn unknown_id_is_none() {
        let store = make_store();
        assert!(store.get("does-not-exist").is_none());
    }

    #[test]
    fn max_jobs_cap_refuses_creation() {
        let store = InMemoryJobStore::new(2);
        assert!(store.create("o".into(), "pdf2img").is_some());
        assert!(store.create("o".into(), "pdf2img").is_some());
        assert!(
            store.create("o".into(), "pdf2img").is_none(),
            "third create must be refused at the cap"
        );
    }

    #[test]
    fn lifecycle_transitions() {
        let store = make_store();
        let (id, _p) = store.create("o".into(), "pdf2img").unwrap();
        store.mark_running(&id);
        assert_eq!(store.get(&id).unwrap().status, JobStatus::Running);

        let result = JobResult {
            path: PathBuf::from("/tmp/nonexistent"),
            content_type: "application/zip",
            filename: "pages.zip",
            extra_headers: vec![],
            size_bytes: 0,
        };
        store.mark_completed(&id, result, Instant::now());
        assert_eq!(store.get(&id).unwrap().status, JobStatus::Completed);
        assert!(store.get(&id).unwrap().result.is_some());
    }

    #[test]
    fn reap_drops_only_expired_terminal_jobs() {
        let store = make_store();
        // A queued (non-terminal) job is never reaped.
        let (queued, _p) = store.create("o".into(), "pdf2img").unwrap();

        // A completed job, finished "2 hours ago". Build the "now" used for
        // reaping by *adding* to the long-ago instant rather than subtracting
        // from `Instant::now()` — on a machine whose monotonic clock has been
        // running for less than 2h, `Instant::now() - 7200s` underflows and
        // panics (the monotonic clock has no value that far in the past). Both
        // instants are anchored to the same base, so the elapsed gap is exactly
        // 7200s regardless of system uptime.
        let (done, _p2) = store.create("o".into(), "pdf2img").unwrap();
        let long_ago = Instant::now();
        let now = long_ago + Duration::from_secs(7200);
        store.mark_completed(
            &done,
            JobResult {
                path: PathBuf::from("/tmp/whatever.zip"),
                content_type: "application/zip",
                filename: "pages.zip",
                extra_headers: vec![],
                size_bytes: 0,
            },
            long_ago,
        );

        let reaped = store.reap_expired(now, Duration::from_secs(3600));
        assert_eq!(reaped.len(), 1, "only the expired completed job is reaped");
        assert!(store.get(&done).is_none(), "expired job removed");
        assert!(store.get(&queued).is_some(), "queued job retained");
    }
}
