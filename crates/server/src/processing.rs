//! Per-request resource safety: cooperative processing timeout and limit checks.
//!
//! ## Why cooperative cancellation, not a tower timeout
//!
//! The heavy work (render / extract) runs CPU-bound inside
//! `tokio::task::spawn_blocking` + rayon. A `tower::TimeoutLayer` only times
//! out the *async future* — it returns an error to the client but the blocking
//! thread keeps running, still pegging a CPU core. That leaks workers under
//! attack, which is exactly the DoS we're defending against.
//!
//! Instead we hand the engine a [`CancelToken`] and arm a timer task that trips
//! it at the deadline. The engine polls the flag in its hot loops (operator
//! dispatch, tiling tiles, per-page parallel render) and bails out, freeing the
//! thread. This actually STOPS the work rather than abandoning the wait.

use std::future::Future;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use oxide_engine::CancelToken;

use crate::config::ServerConfig;
use crate::error::ServerError;

/// Live progress counters for a running job. The worker hands a shared handle
/// to the processing core, which bumps `done` as each page/image completes; the
/// status endpoint reads these to report `pages_done`/`pages_total`. Atomics so
/// the parallel render loop can update without locking.
#[derive(Debug, Default)]
pub struct JobProgress {
    pub done: AtomicUsize,
    pub total: AtomicUsize,
}

impl JobProgress {
    pub fn set_total(&self, total: usize) {
        self.total.store(total, Ordering::Relaxed);
    }
    pub fn inc(&self) {
        self.done.fetch_add(1, Ordering::Relaxed);
    }
    pub fn snapshot(&self) -> (usize, usize) {
        (
            self.done.load(Ordering::Relaxed),
            self.total.load(Ordering::Relaxed),
        )
    }
}

/// A fully-processed result ready to be returned to a client (sync path) or
/// written to a job's result file (async path). Both the synchronous handlers
/// and the background job worker produce this via the SAME core processing
/// functions, guaranteeing byte-identical output regardless of path.
pub struct ProcessedOutput {
    pub bytes: Vec<u8>,
    pub content_type: &'static str,
    pub filename: &'static str,
    /// Extra response headers (e.g. x-page-count). Stored alongside a job's
    /// result so the result endpoint can replay them.
    pub extra_headers: Vec<(&'static str, String)>,
}

/// Run CPU-bound `work` on the blocking pool under a cooperative deadline of
/// `timeout_secs` seconds (0 disables). Shared core for both the per-request
/// timeout and the larger per-job timeout.
///
/// `work` receives a [`CancelToken`] it must thread into the engine call so the
/// engine's hot loops can observe cancellation. A timer task trips the token at
/// the deadline; the timer is aborted as soon as `work` finishes, so no timer
/// leaks on the normal (fast) path.
pub async fn run_with_deadline_secs<F, T>(timeout_secs: u64, work: F) -> Result<T, ServerError>
where
    F: FnOnce(CancelToken) -> T + Send + 'static,
    T: Send + 'static,
{
    let token = CancelToken::new();

    let timer_handle = if timeout_secs > 0 {
        let timer_token = token.clone();
        let dur = Duration::from_secs(timeout_secs);
        Some(tokio::spawn(async move {
            tokio::time::sleep(dur).await;
            timer_token.cancel();
        }))
    } else {
        None
    };

    let work_token = token.clone();
    let join = tokio::task::spawn_blocking(move || work(work_token)).await;

    if let Some(handle) = timer_handle {
        handle.abort();
    }

    match join {
        Ok(value) => Ok(value),
        Err(join_err) => {
            if token.is_cancelled() {
                Err(ServerError::Timeout)
            } else {
                Err(ServerError::Internal(format!(
                    "processing task failed: {}",
                    join_err
                )))
            }
        }
    }
}

/// Run CPU-bound `work` on the blocking pool under a cooperative deadline.
///
/// `work` receives a [`CancelToken`] it must thread into the engine call so the
/// engine's hot loops can observe cancellation. A timer task trips the token at
/// `config.request_timeout_secs`; the timer is aborted as soon as `work`
/// finishes, so no timer leaks on the normal (fast) path.
///
/// Returns `Err(ServerError::Timeout)` if the deadline tripped, otherwise the
/// result of `work`.
pub async fn run_with_timeout<F, T>(config: &ServerConfig, work: F) -> Result<T, ServerError>
where
    F: FnOnce(CancelToken) -> T + Send + 'static,
    T: Send + 'static,
{
    run_with_deadline_secs(config.request_timeout_secs, work).await
}

/// Await `fut`, returning [`ServerError::Timeout`] if it doesn't resolve within
/// the configured budget. Used to bound the whole async handler (including
/// multipart body reads) as a coarse backstop on top of the cooperative engine
/// cancellation, so even non-engine stalls can't hang a request forever.
pub async fn with_deadline<F, T>(config: &ServerConfig, fut: F) -> Result<T, ServerError>
where
    F: Future<Output = Result<T, ServerError>>,
{
    if config.request_timeout_secs == 0 {
        return fut.await;
    }
    // Give the outer async deadline a small grace margin over the inner
    // cooperative one so the engine's clean Timeout error wins the race and the
    // client gets the specific message rather than this backstop.
    let dur = Duration::from_secs(config.request_timeout_secs) + Duration::from_secs(5);
    match tokio::time::timeout(dur, fut).await {
        Ok(result) => result,
        Err(_elapsed) => Err(ServerError::Timeout),
    }
}

/// Reject a render whose pixel count would exceed the configured cap, BEFORE
/// any pixel buffer is allocated. `width_px`/`height_px` come from the page
/// viewport (MediaBox * DPI), so a giant MediaBox is caught here rather than at
/// allocation time.
pub fn check_render_pixels(
    config: &ServerConfig,
    page_number: usize,
    width_px: u32,
    height_px: u32,
) -> Result<(), ServerError> {
    let pixels = width_px as u64 * height_px as u64;
    if pixels > config.max_render_pixels {
        return Err(ServerError::ResourceLimit(format!(
            "page {} would render {} pixels ({}x{}), exceeding the limit of {} pixels; \
             lower the DPI or the page is abusively large",
            page_number, pixels, width_px, height_px, config.max_render_pixels
        )));
    }
    Ok(())
}

/// Enforce the running output-size cap as a ZIP (or other payload) is built.
/// Call after each chunk is appended; errors the moment the accumulated size
/// crosses the cap so the whole oversized payload is never buffered.
pub fn check_output_size(config: &ServerConfig, accumulated: usize) -> Result<(), ServerError> {
    check_output_size_limit(config.max_output_bytes, accumulated)
}

/// Variant taking an explicit byte cap, so the same check works from a job
/// worker (which has the config's value but not the global config handle).
pub fn check_output_size_limit(
    max_output_bytes: u64,
    accumulated: usize,
) -> Result<(), ServerError> {
    if accumulated as u64 > max_output_bytes {
        return Err(ServerError::ResourceLimit(format!(
            "response output exceeded the limit of {} bytes",
            max_output_bytes
        )));
    }
    Ok(())
}

/// Reject an extract-images request that found more images than the cap.
pub fn check_image_count(config: &ServerConfig, count: usize) -> Result<(), ServerError> {
    check_image_count_limit(config.max_image_count, count)
}

/// Variant taking an explicit cap, callable from a job worker without the
/// global config handle.
pub fn check_image_count_limit(max_image_count: usize, count: usize) -> Result<(), ServerError> {
    if count > max_image_count {
        return Err(ServerError::ResourceLimit(format!(
            "request would extract {} images, exceeding the limit of {}",
            count, max_image_count
        )));
    }
    Ok(())
}
