//! Asynchronous job model for the heavy endpoints (pdf2img / extract-images).
//!
//! ## Why a job model
//!
//! The synchronous endpoints hold the HTTP connection open for the whole of a
//! render/extract. For a large PDF that can be many seconds to minutes — fragile
//! across client/proxy/LB timeouts, and capped outright by the per-request
//! timeout from the resource-safety work. The async model runs
//! the heavy work in the background, untied to a single request: SUBMIT returns
//! a job id immediately (202), the client POLLS status, then RETRIEVES the
//! result when complete.
//!
//! ## Scope of THIS implementation (documented limitation)
//!
//! The store is IN-MEMORY and results live in a temp directory on disk, both
//! keyed by job id. This is SINGLE-PROCESS: job state is lost on restart and it
//! does not scale horizontally. That is the intended scope — it delivers the
//! async model with zero external dependencies (no DB/Redis/broker), consistent
//! with the project's lean, self-contained ethos. The `JobStore` trait is the
//! seam: a persistent/distributed backend can be slotted in later without
//! touching the handlers or worker.
//!
//! ## What is bounded (resource safety)
//!
//! - Queue length (`job_queue_capacity`): submissions past it are rejected 503.
//! - Worker count (`job_workers`): fixed pool, controlled concurrency.
//! - Per-job time (`job_timeout_secs`): larger than the sync cap, but still
//!   bounded; on expiry the job is marked failed.
//! - Retained jobs (`max_jobs`) and retention window (`job_retention_secs`): a
//!   cleanup task drops expired jobs and deletes their temp result files.
//!
//! ## Ownership / safety
//!
//! Job ids are non-guessable (128 bits of OS randomness, hex-encoded). Every
//! job records the submitting identity (API key, or "anonymous" when auth is
//! disabled); status/result are scoped to that identity. A request for a job
//! owned by someone else is treated as 404 (not 403) so the endpoint never
//! confirms the existence of another caller's job.

mod id;
mod store;
mod worker;

pub(crate) use store::{JobKind, JobStatus};
pub(crate) use worker::{JobSystem, SubmitOutcome};

use std::sync::Arc;

use crate::config::ServerConfig;

/// The job subsystem shared via Axum state: the store plus the bounded queue
/// sender. Cheap to clone (everything inside is `Arc`).
#[derive(Clone)]
pub struct JobsState {
    pub system: Arc<JobSystem>,
}

impl JobsState {
    /// Build the job system: spawn the worker pool and the retention-cleanup
    /// task, returning the state handed to handlers. The returned guards keep
    /// the background tasks alive for the server's lifetime.
    pub fn start(config: Arc<ServerConfig>) -> (Self, JobsGuards) {
        let (system, guards) = JobSystem::start(config);
        (
            Self {
                system: Arc::new(system),
            },
            guards,
        )
    }
}

/// Owns the background task handles. Dropping it aborts the workers and the
/// cleanup task (used by tests; `main` holds it for the process lifetime).
pub struct JobsGuards {
    pub workers: Vec<tokio::task::JoinHandle<()>>,
    pub cleanup: tokio::task::JoinHandle<()>,
}

impl Drop for JobsGuards {
    fn drop(&mut self) {
        for w in &self.workers {
            w.abort();
        }
        self.cleanup.abort();
    }
}
