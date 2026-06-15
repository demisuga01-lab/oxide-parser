#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub port: u16,
    pub log_level: String,
    pub max_file_size: usize,
    pub max_dpi: u32,
    pub max_pages: usize,
    /// Comma-separated list of valid API keys. Empty means no keys configured;
    /// the server then REFUSES TO START unless `allow_unauthenticated` is set.
    pub api_keys: Vec<String>,
    /// Explicit dev-only opt-in (OXIDE_ALLOW_UNAUTHENTICATED=true) to run with
    /// no API keys. Fail-closed by default: without keys and without this flag,
    /// startup aborts rather than silently exposing every endpoint.
    pub allow_unauthenticated: bool,
    /// Allowlist of origins permitted for cross-origin (CORS) requests. Empty by
    /// default (most restrictive: no cross-origin access). Set
    /// OXIDE_CORS_ALLOWED_ORIGINS to a comma-separated list of full origins
    /// (e.g. `https://app.example.com`).
    pub cors_allowed_origins: Vec<String>,
    /// Dev-only opt-in (OXIDE_CORS_ALLOW_ANY=true) to allow ANY origin. Mirrors
    /// the auth dev opt-in; logs a warning on startup. Never enable in prod.
    pub cors_allow_any: bool,
    /// Maximum requests per minute per key. Zero disables rate limiting.
    pub rate_limit_per_min: u32,
    /// Wall-clock budget for the heavy processing phase of a single request,
    /// in seconds. When exceeded, a cooperative cancellation flag trips and the
    /// engine bails out of its hot loops, returning a timeout error rather than
    /// occupying a worker indefinitely. Zero disables the timeout.
    pub request_timeout_secs: u64,
    /// Cap on rendered pixels per page (width_px * height_px). A page whose
    /// MediaBox * DPI would exceed this is rejected BEFORE the pixel buffer is
    /// allocated, preventing a giant-MediaBox "pixel explosion" OOM.
    pub max_render_pixels: u64,
    /// Cap on total response/ZIP output bytes accumulated for a request. Once
    /// output crosses this while being built, the request errors instead of
    /// buffering an absurd payload (zip-bomb-like input/output asymmetry).
    pub max_output_bytes: u64,
    /// Cap on the number of images extracted in a single extract-images request.
    pub max_image_count: usize,
    /// Number of background worker tasks processing the async job queue.
    pub job_workers: usize,
    /// Bounded capacity of the async job queue. Submissions beyond this (with
    /// all workers busy) are rejected with 503 rather than accepted unbounded.
    pub job_queue_capacity: usize,
    /// Wall-clock budget for a single async JOB, in seconds. Larger than
    /// `request_timeout_secs` on purpose: a job is not holding a connection, so
    /// it can run longer. Zero disables the per-job timeout.
    pub job_timeout_secs: u64,
    /// How long a completed/failed job and its result are retained for the
    /// client to poll/download before the cleanup task removes them.
    pub job_retention_secs: u64,
    /// Backstop cap on the number of jobs retained in the store at once. Bounds
    /// memory/disk even if submissions outpace retention cleanup.
    pub max_jobs: usize,
    /// Directory for job result files. `None` => a per-process subdir of the
    /// system temp dir (or the `OXIDE_JOB_RESULT_DIR` env override). Tests set
    /// this to a unique dir so on-disk cleanup can be verified in isolation.
    pub job_result_dir: Option<String>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            port: 8080,
            log_level: "info".to_string(),
            max_file_size: 52_428_800,
            max_dpi: 600,
            max_pages: 200,
            api_keys: Vec::new(),
            allow_unauthenticated: false,
            cors_allowed_origins: Vec::new(),
            cors_allow_any: false,
            rate_limit_per_min: 60,
            // 30s comfortably covers a large multi-page high-DPI render while
            // stopping a single pathological page from pegging a worker forever.
            request_timeout_secs: 30,
            // 100 megapixels: an A4 page at 600 DPI is ~35 MP, a US-Arch-E
            // sheet (36x48in) at 300 DPI is ~155 MP — so this admits normal
            // high-DPI work and a generous margin while rejecting the
            // 200-inch-square MediaBox class of attack (billions of pixels).
            max_render_pixels: 100_000_000,
            // 2 GiB: a 200-page render ZIP or a large image extraction stays
            // well under this; it only trips on genuinely runaway output.
            max_output_bytes: 2 * 1024 * 1024 * 1024,
            // 10k images is far more than any real document carries on the
            // pages a single request would target.
            max_image_count: 10_000,
            // A small fixed worker pool: the heavy work is CPU-bound and itself
            // internally parallel (rayon over pages), so a few concurrent jobs
            // saturate the machine without oversubscribing.
            job_workers: 2,
            // Absorb short bursts beyond the worker count; reject (503) past it.
            job_queue_capacity: 128,
            // 300s: five minutes lets a genuinely large render/extract finish,
            // far beyond the 30s sync cap — the whole point of the async path.
            job_timeout_secs: 300,
            // Keep finished results for an hour so clients have a comfortable
            // window to poll and download before cleanup reclaims them.
            job_retention_secs: 3_600,
            // Bound total retained jobs regardless of retention timing.
            max_jobs: 1_000,
            job_result_dir: None,
        }
    }
}

impl ServerConfig {
    pub fn from_env() -> Self {
        let mut cfg = Self::default();

        if let Ok(value) = std::env::var("OXIDE_PORT") {
            if let Ok(port) = value.parse::<u16>() {
                cfg.port = port;
            }
        }

        if let Ok(value) = std::env::var("OXIDE_LOG_LEVEL") {
            cfg.log_level = value;
        }

        if let Ok(value) = std::env::var("OXIDE_MAX_FILE_SIZE") {
            if let Ok(max_file_size) = value.parse::<usize>() {
                cfg.max_file_size = max_file_size;
            }
        }

        if let Ok(value) = std::env::var("OXIDE_MAX_DPI") {
            if let Ok(max_dpi) = value.parse::<u32>() {
                cfg.max_dpi = max_dpi.min(600);
            }
        }

        if let Ok(value) = std::env::var("OXIDE_MAX_PAGES") {
            if let Ok(max_pages) = value.parse::<usize>() {
                cfg.max_pages = max_pages;
            }
        }

        if let Ok(value) = std::env::var("OXIDE_API_KEYS") {
            cfg.api_keys = value
                .split(',')
                .map(|key| key.trim().to_string())
                .filter(|key| !key.is_empty())
                .collect();
        }

        if let Ok(value) = std::env::var("OXIDE_ALLOW_UNAUTHENTICATED") {
            cfg.allow_unauthenticated = parse_bool_env(&value);
        }

        if let Ok(value) = std::env::var("OXIDE_CORS_ALLOWED_ORIGINS") {
            cfg.cors_allowed_origins = value
                .split(',')
                .map(|origin| origin.trim().to_string())
                .filter(|origin| !origin.is_empty())
                .collect();
        }

        if let Ok(value) = std::env::var("OXIDE_CORS_ALLOW_ANY") {
            cfg.cors_allow_any = parse_bool_env(&value);
        }

        if let Ok(value) = std::env::var("OXIDE_RATE_LIMIT_PER_MIN") {
            if let Ok(rate_limit_per_min) = value.parse::<u32>() {
                cfg.rate_limit_per_min = rate_limit_per_min;
            }
        }

        if let Ok(value) = std::env::var("OXIDE_REQUEST_TIMEOUT_SECS") {
            if let Ok(request_timeout_secs) = value.parse::<u64>() {
                cfg.request_timeout_secs = request_timeout_secs;
            }
        }

        if let Ok(value) = std::env::var("OXIDE_MAX_RENDER_PIXELS") {
            if let Ok(max_render_pixels) = value.parse::<u64>() {
                cfg.max_render_pixels = max_render_pixels;
            }
        }

        if let Ok(value) = std::env::var("OXIDE_MAX_OUTPUT_BYTES") {
            if let Ok(max_output_bytes) = value.parse::<u64>() {
                cfg.max_output_bytes = max_output_bytes;
            }
        }

        if let Ok(value) = std::env::var("OXIDE_MAX_IMAGE_COUNT") {
            if let Ok(max_image_count) = value.parse::<usize>() {
                cfg.max_image_count = max_image_count;
            }
        }

        if let Ok(value) = std::env::var("OXIDE_JOB_WORKERS") {
            if let Ok(job_workers) = value.parse::<usize>() {
                // At least one worker, else queued jobs would never drain.
                cfg.job_workers = job_workers.max(1);
            }
        }

        if let Ok(value) = std::env::var("OXIDE_JOB_QUEUE_CAPACITY") {
            if let Ok(job_queue_capacity) = value.parse::<usize>() {
                cfg.job_queue_capacity = job_queue_capacity.max(1);
            }
        }

        if let Ok(value) = std::env::var("OXIDE_JOB_TIMEOUT_SECS") {
            if let Ok(job_timeout_secs) = value.parse::<u64>() {
                cfg.job_timeout_secs = job_timeout_secs;
            }
        }

        if let Ok(value) = std::env::var("OXIDE_JOB_RETENTION_SECS") {
            if let Ok(job_retention_secs) = value.parse::<u64>() {
                cfg.job_retention_secs = job_retention_secs;
            }
        }

        if let Ok(value) = std::env::var("OXIDE_MAX_JOBS") {
            if let Ok(max_jobs) = value.parse::<usize>() {
                cfg.max_jobs = max_jobs.max(1);
            }
        }

        cfg
    }

    /// Fail-closed startup validation. Returns an error describing the
    /// misconfiguration if the server would otherwise come up in an unsafe
    /// state. The governing rule: an empty API-key list must NOT silently
    /// leave every endpoint open — it requires the explicit dev opt-in.
    pub fn validate(&self) -> Result<(), String> {
        if self.api_keys.is_empty() && !self.allow_unauthenticated {
            return Err(
                "OXIDE_API_KEYS is empty and OXIDE_ALLOW_UNAUTHENTICATED is not set; \
                 refusing to start an unauthenticated server. Set OXIDE_API_KEYS to a \
                 comma-separated list of keys, or set OXIDE_ALLOW_UNAUTHENTICATED=true \
                 to explicitly run without authentication (dev only)."
                    .to_string(),
            );
        }
        Ok(())
    }

    /// True when API-key authentication is actively enforced (keys are
    /// configured). When false, the server is running in the explicit
    /// dev-opt-in unauthenticated mode.
    pub fn auth_enforced(&self) -> bool {
        !self.api_keys.is_empty()
    }
}

/// Parse a boolean-ish env value. Accepts true/1/yes/on (case-insensitive) as
/// true; everything else is false. Keeps the dev opt-ins explicit.
fn parse_bool_env(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "true" | "1" | "yes" | "on"
    )
}

pub static CONFIG: std::sync::OnceLock<ServerConfig> = std::sync::OnceLock::new();

pub fn get_config() -> &'static ServerConfig {
    CONFIG.get_or_init(ServerConfig::default)
}
