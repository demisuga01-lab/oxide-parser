//! Background worker pool, bounded queue, and retention cleanup.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::mpsc;

use crate::config::ServerConfig;
use crate::error::ServerError;
use crate::processing::ProcessedOutput;

use super::store::{InMemoryJobStore, JobError, JobKind, JobResult, JobStore, QueuedJob};

/// The running job subsystem: the store, the bounded queue sender, and the
/// temp directory holding result files. Handed to handlers via Axum state.
pub struct JobSystem {
    pub store: Arc<dyn JobStore>,
    sender: mpsc::Sender<QueuedJob>,
    config: Arc<ServerConfig>,
    result_dir: PathBuf,
}

/// Outcome of trying to enqueue a submitted job.
pub enum SubmitOutcome {
    /// Accepted: the job id to return to the client.
    Accepted(String),
    /// The queue is full (all workers busy and the buffer is saturated) — the
    /// handler should return 503.
    QueueFull,
    /// The store is at its `max_jobs` cap — also a 503/backpressure signal.
    StoreFull,
}

impl JobSystem {
    /// Build the system, spawn the worker pool and the cleanup task. Returns the
    /// system plus the guards owning the background tasks.
    pub fn start(config: Arc<ServerConfig>) -> (Self, super::JobsGuards) {
        let store: Arc<dyn JobStore> = Arc::new(InMemoryJobStore::new(config.max_jobs));

        // Result files live in a per-process subdir of the system temp dir,
        // namespaced so concurrent test servers don't collide. Tests override
        // via `config.job_result_dir` (or the OXIDE_JOB_RESULT_DIR env var) for
        // isolation/inspection.
        let result_dir = resolve_result_dir(config.job_result_dir.as_deref());
        if let Err(e) = std::fs::create_dir_all(&result_dir) {
            tracing::error!(dir = %result_dir.display(), error = %e,
                "failed to create job result dir; result downloads will fail");
        }

        // Bounded queue: capacity absorbs short bursts; beyond it, submission
        // fails fast (503) rather than accepting unbounded work.
        let (sender, receiver) = mpsc::channel::<QueuedJob>(config.job_queue_capacity);
        let receiver = Arc::new(tokio::sync::Mutex::new(receiver));

        let mut workers = Vec::with_capacity(config.job_workers);
        for worker_id in 0..config.job_workers {
            let rx = Arc::clone(&receiver);
            let store_for_worker = Arc::clone(&store);
            let config_for_worker = Arc::clone(&config);
            let dir_for_worker = result_dir.clone();
            workers.push(tokio::spawn(async move {
                worker_loop(
                    worker_id,
                    rx,
                    store_for_worker,
                    config_for_worker,
                    dir_for_worker,
                )
                .await;
            }));
        }

        let cleanup = spawn_cleanup_task(
            Arc::clone(&store),
            Duration::from_secs(config.job_retention_secs),
            // Sweep at a fraction of the retention window so expired jobs don't
            // linger much past their TTL, but never less often than ~5s or more
            // often than every second.
            cleanup_interval(config.job_retention_secs),
        );

        (
            Self {
                store,
                sender,
                config,
                result_dir,
            },
            super::JobsGuards { workers, cleanup },
        )
    }

    pub fn store(&self) -> &Arc<dyn JobStore> {
        &self.store
    }

    pub fn result_dir(&self) -> &PathBuf {
        &self.result_dir
    }

    /// Create a job for `owner` and enqueue it. Returns the outcome the handler
    /// maps to an HTTP response. The job is created in the store FIRST (so it is
    /// pollable immediately), then pushed to the queue; if the queue is full we
    /// mark the just-created job failed and report QueueFull so we don't leak a
    /// permanently-queued job that no worker will ever pick up.
    pub(crate) fn submit(&self, owner: String, kind: JobKind) -> SubmitOutcome {
        let label = kind.label();
        let (id, progress) = match self.store.create(owner, label) {
            Some(pair) => pair,
            None => return SubmitOutcome::StoreFull,
        };

        let queued = QueuedJob {
            id: id.clone(),
            kind,
            progress,
        };

        match self.sender.try_send(queued) {
            Ok(()) => SubmitOutcome::Accepted(id),
            Err(mpsc::error::TrySendError::Full(_)) => {
                // Roll back: the job will never run, so mark it failed (it is
                // then reaped on the normal retention schedule) and tell the
                // client to retry later.
                self.store.mark_failed(
                    &id,
                    JobError {
                        code: "queue_full",
                        message: "The job queue is full; retry shortly.".to_string(),
                        reference: None,
                    },
                    Instant::now(),
                );
                SubmitOutcome::QueueFull
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                self.store.mark_failed(
                    &id,
                    JobError {
                        code: "internal_error",
                        message: "The job system is shutting down.".to_string(),
                        reference: None,
                    },
                    Instant::now(),
                );
                SubmitOutcome::QueueFull
            }
        }
    }

    pub fn config(&self) -> &Arc<ServerConfig> {
        &self.config
    }
}

/// Map a retention TTL to a cleanup sweep interval: roughly a tenth of the TTL,
/// clamped to [1s, 60s]. A short test TTL therefore still gets swept promptly.
fn cleanup_interval(retention_secs: u64) -> Duration {
    let secs = (retention_secs / 10).clamp(1, 60);
    Duration::from_secs(secs)
}

fn resolve_result_dir(configured: Option<&str>) -> PathBuf {
    if let Some(dir) = configured {
        if !dir.is_empty() {
            return PathBuf::from(dir);
        }
    }
    if let Ok(dir) = std::env::var("OXIDE_JOB_RESULT_DIR") {
        if !dir.is_empty() {
            return PathBuf::from(dir);
        }
    }
    let pid = std::process::id();
    std::env::temp_dir().join(format!("oxide-jobs-{}", pid))
}

/// One worker: pull jobs off the shared queue and process them. Robust to a
/// single job's failure or panic — a bad job marks itself failed and the worker
/// continues to the next. The worker exits only when the channel is closed
/// (system shutdown).
async fn worker_loop(
    worker_id: usize,
    receiver: Arc<tokio::sync::Mutex<mpsc::Receiver<QueuedJob>>>,
    store: Arc<dyn JobStore>,
    config: Arc<ServerConfig>,
    result_dir: PathBuf,
) {
    loop {
        // Hold the receiver lock only long enough to take one job, so all
        // workers share the single queue fairly (one consumer at a time pulls,
        // then releases for the next).
        let job = {
            let mut rx = receiver.lock().await;
            rx.recv().await
        };

        let Some(job) = job else {
            tracing::debug!(worker_id, "job queue closed; worker exiting");
            break;
        };

        let id = job.id.clone();
        store.mark_running(&id);

        // Process the job in its OWN tokio task and await the handle, so a panic
        // ANYWHERE in the job — not just inside the `spawn_blocking` engine call,
        // but in the async orchestration around it — surfaces as a `JoinError`
        // rather than unwinding this worker loop. A single job's panic therefore
        // can never take down the worker or the server: it marks that job failed
        // (with a classified, non-leaking message) and the loop continues.
        let config_for_job = Arc::clone(&config);
        let outcome = match tokio::spawn(process_job(job, config_for_job)).await {
            Ok(result) => result,
            Err(join_err) => {
                tracing::error!(job_id = %id, worker_id, error = %join_err,
                    "job task panicked; marking failed and continuing");
                Err(ServerError::Internal(format!("job task aborted: {}", join_err)))
            }
        };

        let now = Instant::now();
        match outcome {
            Ok(output) => match persist_result(&result_dir, &id, output) {
                Ok(result) => store.mark_completed(&id, result, now),
                Err(err) => {
                    let classified = err.classify();
                    store.mark_failed(
                        &id,
                        JobError {
                            code: classified.error_code,
                            message: classified.message,
                            reference: classified.reference,
                        },
                        now,
                    );
                }
            },
            Err(err) => {
                let classified = err.classify();
                store.mark_failed(
                    &id,
                    JobError {
                        code: classified.error_code,
                        message: classified.message,
                        reference: classified.reference,
                    },
                    now,
                );
            }
        }
    }
}

/// Run the heavy work for one job, reusing the SAME engine entry points the sync
/// handlers use (guaranteeing byte-identical output) under the per-job deadline
/// and the resource limits from `config`.
async fn process_job(
    job: QueuedJob,
    config: Arc<ServerConfig>,
) -> Result<ProcessedOutput, ServerError> {
    let QueuedJob { kind, progress, .. } = job;
    match kind {
        JobKind::Pdf2Img(params) => {
            crate::routes::pdf2img::process_pdf2img(
                params,
                &config,
                config.job_timeout_secs,
                Some(progress),
            )
            .await
        }
        JobKind::ExtractImages(params) => {
            // The extract core is synchronous/CPU-bound; run it on the blocking
            // pool under the per-job deadline so a runaway extraction can't peg
            // a worker past the timeout. Output is identical to the sync path.
            let max_image_count = config.max_image_count;
            let max_output_bytes = config.max_output_bytes;
            crate::processing::run_with_deadline_secs(config.job_timeout_secs, move |_cancel| {
                crate::routes::extract_images::process_extract_images(
                    params,
                    max_image_count,
                    max_output_bytes,
                )
            })
            .await?
        }
    }
}

/// Write a completed job's bytes to a temp file keyed by job id and return the
/// result metadata. Bounded-memory: the bytes are already built (capped by
/// `max_output_bytes` during processing); we stream them to disk and drop them.
fn persist_result(
    result_dir: &std::path::Path,
    id: &str,
    output: ProcessedOutput,
) -> Result<JobResult, ServerError> {
    let path = result_dir.join(format!("{}.bin", id));
    let size_bytes = output.bytes.len() as u64;
    std::fs::write(&path, &output.bytes)
        .map_err(|e| ServerError::Internal(format!("failed to write job result file: {}", e)))?;
    Ok(JobResult {
        path,
        content_type: output.content_type,
        filename: output.filename,
        extra_headers: output.extra_headers,
        size_bytes,
    })
}

/// Spawn the retention cleanup task. Periodically reaps terminal jobs past the
/// retention TTL (dropping store state) and deletes their result files from
/// disk. Mirrors the rate-limiter cleanup pattern: a `Weak` would be ideal but
/// the store is a trait object behind `Arc`, so we hold an `Arc` and rely on the
/// returned handle being aborted on shutdown (the `JobsGuards` drop does this).
pub fn spawn_cleanup_task(
    store: Arc<dyn JobStore>,
    retention: Duration,
    interval: Duration,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        ticker.tick().await; // skip the immediate first tick
        loop {
            ticker.tick().await;
            let reaped = store.reap_expired(Instant::now(), retention);
            for path in reaped {
                if let Err(e) = std::fs::remove_file(&path) {
                    // A missing file is fine (already gone); log others at debug.
                    if e.kind() != std::io::ErrorKind::NotFound {
                        tracing::debug!(path = %path.display(), error = %e,
                            "failed to remove expired job result file");
                    }
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleanup_interval_clamps() {
        assert_eq!(cleanup_interval(3600), Duration::from_secs(60)); // 360 -> clamp 60
        assert_eq!(cleanup_interval(50), Duration::from_secs(5)); // 5
        assert_eq!(cleanup_interval(2), Duration::from_secs(1)); // 0 -> clamp 1
        assert_eq!(cleanup_interval(0), Duration::from_secs(1)); // 0 -> clamp 1
    }
}
