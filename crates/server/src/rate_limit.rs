use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::{
    body::Body,
    extract::State,
    http::{Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};

/// Length of a single rate-limit window. A key may make up to `limit` requests
/// within any rolling 60-second window.
const WINDOW: Duration = Duration::from_secs(60);

/// Backstop cap on the number of distinct keys tracked simultaneously. Even
/// under adversarial key/IP rotation (which inflates the map) memory stays
/// bounded: once the cap is hit we sweep expired entries and, if still full,
/// evict the oldest window. 100k entries is far above any legitimate client
/// population while bounding worst-case memory to a few MB.
const DEFAULT_MAX_KEYS: usize = 100_000;

/// Abstract time source so tests can advance time deterministically instead of
/// sleeping. Production uses [`SystemClock`]; tests use [`ManualClock`].
pub trait TimeSource: Send + Sync {
    fn now(&self) -> Instant;
}

/// Wall-clock time source backed by `Instant::now()`.
pub struct SystemClock;

impl TimeSource for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

/// Test clock: starts at construction time and only advances when `advance` is
/// called, so window expiry and cleanup can be exercised without real waiting.
pub struct ManualClock {
    base: Instant,
    offset_ms: AtomicU64,
}

impl ManualClock {
    pub fn new() -> Self {
        Self {
            base: Instant::now(),
            offset_ms: AtomicU64::new(0),
        }
    }

    pub fn advance(&self, by: Duration) {
        self.offset_ms
            .fetch_add(by.as_millis() as u64, Ordering::SeqCst);
    }
}

impl Default for ManualClock {
    fn default() -> Self {
        Self::new()
    }
}

impl TimeSource for ManualClock {
    fn now(&self) -> Instant {
        self.base + Duration::from_millis(self.offset_ms.load(Ordering::SeqCst))
    }
}

#[derive(Debug)]
struct Window {
    start: Instant,
    count: u32,
}

pub struct RateLimiter {
    windows: Mutex<HashMap<String, Window>>,
    limit: u32,
    max_keys: usize,
    clock: Box<dyn TimeSource>,
}

impl RateLimiter {
    pub fn new(requests_per_minute: u32) -> Self {
        Self::with_clock(requests_per_minute, Box::new(SystemClock))
    }

    /// Construct with an explicit time source (used by tests).
    pub fn with_clock(requests_per_minute: u32, clock: Box<dyn TimeSource>) -> Self {
        Self {
            windows: Mutex::new(HashMap::new()),
            limit: requests_per_minute,
            max_keys: DEFAULT_MAX_KEYS,
            clock,
        }
    }

    /// Override the distinct-key backstop cap (used by tests).
    pub fn with_max_keys(mut self, max_keys: usize) -> Self {
        self.max_keys = max_keys;
        self
    }

    pub fn limit(&self) -> u32 {
        self.limit
    }

    pub fn check(&self, key: &str) -> bool {
        if self.limit == 0 {
            return true;
        }

        let mut map = self.windows.lock().unwrap_or_else(|err| err.into_inner());
        let now = self.clock.now();

        // Backstop: if this is a NEW key and the map is at capacity, sweep
        // expired entries first, then evict the oldest window if still full.
        // This bounds memory under adversarial key rotation without disturbing
        // active windows of legitimate keys (the oldest-start entry is the one
        // closest to expiry anyway).
        if !map.contains_key(key) && map.len() >= self.max_keys {
            map.retain(|_, w| now.duration_since(w.start) < WINDOW);
            if map.len() >= self.max_keys {
                if let Some(oldest) = map
                    .iter()
                    .min_by_key(|(_, w)| w.start)
                    .map(|(k, _)| k.clone())
                {
                    map.remove(&oldest);
                }
            }
        }

        let window = map.entry(key.to_string()).or_insert(Window {
            start: now,
            count: 0,
        });

        if now.duration_since(window.start) >= WINDOW {
            window.start = now;
            window.count = 0;
        }

        if window.count >= self.limit {
            return false;
        }

        window.count += 1;
        true
    }

    /// Remove buckets whose window has fully elapsed. An elapsed window carries
    /// no live rate-limit state — the next `check` for that key would reset it
    /// to a fresh window anyway — so removing it is equivalent to leaving it and
    /// never prematurely resets an ACTIVE window (one still inside `WINDOW`).
    pub fn cleanup_expired(&self) {
        let mut map = self.windows.lock().unwrap_or_else(|err| err.into_inner());
        let now = self.clock.now();
        map.retain(|_, window| now.duration_since(window.start) < WINDOW);
    }

    /// Number of tracked keys. Exposed for tests/observability.
    pub fn tracked_keys(&self) -> usize {
        self.windows
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .len()
    }

    /// Spawn a background task that periodically sweeps expired buckets, bounding
    /// the limiter's memory over the server's lifetime. The task holds only a
    /// `Weak` reference so it does not keep the limiter alive on its own and
    /// stops once the limiter (and thus the app) is dropped. Returns the
    /// `JoinHandle` so callers may abort it explicitly if desired.
    pub fn spawn_cleanup(self: &Arc<Self>, interval: Duration) -> tokio::task::JoinHandle<()> {
        let weak = Arc::downgrade(self);
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            // Skip the immediate first tick so we don't sweep an empty map at t=0.
            ticker.tick().await;
            loop {
                ticker.tick().await;
                match weak.upgrade() {
                    Some(limiter) => limiter.cleanup_expired(),
                    None => break, // limiter dropped; nothing left to clean.
                }
            }
        })
    }
}

pub async fn rate_limit_middleware(
    State(limiter): State<Arc<RateLimiter>>,
    req: Request<Body>,
    next: Next,
) -> Response {
    if limiter.limit() == 0 || is_public_probe(req.uri().path()) {
        return next.run(req).await;
    }

    let key = crate::auth::extract_api_key(&req).unwrap_or_else(|| "anonymous".to_string());

    if !limiter.check(&key) {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            [("retry-after", "60")],
            axum::Json(serde_json::json!({
                "error": "rate_limit_exceeded",
                "message": "Too many requests. Retry after 60 seconds.",
                "limit": limiter.limit(),
            })),
        )
            .into_response();
    }

    next.run(req).await
}

fn is_public_probe(path: &str) -> bool {
    matches!(
        path,
        "/health" | "/readiness" | "/api/v1/health" | "/api/v1/readiness"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_limiter_allows_requests_within_limit() {
        let limiter = RateLimiter::new(5);
        for i in 0..5 {
            assert!(
                limiter.check("test-key"),
                "request {} should be allowed",
                i + 1
            );
        }
        assert!(
            !limiter.check("test-key"),
            "6th request should be rate-limited"
        );
    }

    #[test]
    fn rate_limiter_with_limit_zero_always_allows() {
        let limiter = RateLimiter::new(0);
        for _ in 0..1000 {
            assert!(limiter.check("key"));
        }
    }

    #[test]
    fn different_keys_have_independent_limits() {
        let limiter = RateLimiter::new(2);
        assert!(limiter.check("key-a"));
        assert!(limiter.check("key-a"));
        assert!(!limiter.check("key-a"));
        assert!(limiter.check("key-b"));
    }

    #[test]
    fn cleanup_expired_does_not_panic() {
        let limiter = RateLimiter::new(10);
        limiter.check("key-1");
        limiter.check("key-2");
        limiter.cleanup_expired();
    }

    #[test]
    fn rate_limiter_denies_after_window_limit() {
        let limiter = RateLimiter::new(3);
        assert!(limiter.check("key"));
        assert!(limiter.check("key"));
        assert!(limiter.check("key"));
        assert!(!limiter.check("key"));
    }

    #[test]
    fn rate_limiter_denied_checks_remain_denied() {
        let limiter = RateLimiter::new(2);
        assert!(limiter.check("k"));
        assert!(limiter.check("k"));
        assert!(!limiter.check("k"));
        assert!(!limiter.check("k"));
    }

    #[test]
    fn cleanup_expired_keeps_recent_windows() {
        let limiter = RateLimiter::new(100);
        for i in 0..50u16 {
            limiter.check(&format!("key-{}", i));
        }
        assert_eq!(limiter.tracked_keys(), 50);
        limiter.cleanup_expired();
        assert_eq!(limiter.tracked_keys(), 50);
    }

    #[test]
    fn cleanup_removes_expired_but_keeps_active_with_manual_clock() {
        let clock = Arc::new(ManualClock::new());
        // The limiter needs to own a clock; hand it a clone-backed wrapper.
        struct Shared(Arc<ManualClock>);
        impl TimeSource for Shared {
            fn now(&self) -> Instant {
                self.0.now()
            }
        }
        let limiter = RateLimiter::with_clock(100, Box::new(Shared(clock.clone())));

        // Two keys created at t=0.
        limiter.check("old");
        limiter.check("old");
        assert_eq!(limiter.tracked_keys(), 1);

        // Advance 30s (still within the 60s window), add a second key.
        clock.advance(Duration::from_secs(30));
        limiter.check("recent");
        assert_eq!(limiter.tracked_keys(), 2);

        // Advance another 40s: "old" started at t=0 (now 70s old → expired),
        // "recent" started at t=30s (now 40s old → still active).
        clock.advance(Duration::from_secs(40));
        limiter.cleanup_expired();
        assert_eq!(
            limiter.tracked_keys(),
            1,
            "expired 'old' should be removed, active 'recent' retained"
        );
    }

    #[test]
    fn window_resets_after_expiry_with_manual_clock() {
        let clock = Arc::new(ManualClock::new());
        struct Shared(Arc<ManualClock>);
        impl TimeSource for Shared {
            fn now(&self) -> Instant {
                self.0.now()
            }
        }
        let limiter = RateLimiter::with_clock(2, Box::new(Shared(clock.clone())));

        assert!(limiter.check("k"));
        assert!(limiter.check("k"));
        assert!(!limiter.check("k"), "limit of 2 reached");

        // Past the window: the limit resets.
        clock.advance(Duration::from_secs(61));
        assert!(limiter.check("k"), "new window should allow again");
    }

    #[test]
    fn max_keys_backstop_bounds_map_size() {
        let limiter = RateLimiter::new(100).with_max_keys(10);
        for i in 0..100u16 {
            limiter.check(&format!("key-{}", i));
        }
        assert!(
            limiter.tracked_keys() <= 10,
            "map should be bounded by max_keys, got {}",
            limiter.tracked_keys()
        );
    }
}
