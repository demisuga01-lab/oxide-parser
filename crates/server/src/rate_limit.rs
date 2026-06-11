use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use axum::{
    body::Body,
    http::{Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};

#[derive(Debug)]
struct Window {
    start: Instant,
    count: u32,
}

pub struct RateLimiter {
    windows: Mutex<HashMap<String, Window>>,
    limit: u32,
}

impl RateLimiter {
    pub fn new(requests_per_minute: u32) -> Self {
        Self {
            windows: Mutex::new(HashMap::new()),
            limit: requests_per_minute,
        }
    }

    pub fn check(&self, key: &str) -> bool {
        if self.limit == 0 {
            return true;
        }

        let mut map = self.windows.lock().unwrap_or_else(|err| err.into_inner());
        let now = Instant::now();
        let window = map.entry(key.to_string()).or_insert(Window {
            start: now,
            count: 0,
        });

        if now.duration_since(window.start) >= Duration::from_secs(60) {
            window.start = now;
            window.count = 0;
        }

        if window.count >= self.limit {
            return false;
        }

        window.count += 1;
        true
    }

    pub fn cleanup_expired(&self) {
        let mut map = self.windows.lock().unwrap_or_else(|err| err.into_inner());
        let now = Instant::now();
        map.retain(|_, window| now.duration_since(window.start) < Duration::from_secs(120));
    }
}

pub static RATE_LIMITER: OnceLock<RateLimiter> = OnceLock::new();

pub fn get_rate_limiter() -> &'static RateLimiter {
    RATE_LIMITER.get_or_init(|| {
        let limit = crate::config::get_config().rate_limit_per_min;
        RateLimiter::new(limit)
    })
}

pub async fn rate_limit_middleware(req: Request<Body>, next: Next) -> Response {
    let config = crate::config::get_config();

    if config.rate_limit_per_min == 0 || is_public_probe(req.uri().path()) {
        return next.run(req).await;
    }

    let key = crate::auth::extract_api_key(&req).unwrap_or_else(|| "anonymous".to_string());

    if !get_rate_limiter().check(&key) {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            [("retry-after", "60")],
            axum::Json(serde_json::json!({
                "error": "rate_limit_exceeded",
                "message": "Too many requests. Retry after 60 seconds.",
                "limit": config.rate_limit_per_min,
            })),
        )
            .into_response();
    }

    // TODO(ops): spawn a background task to call cleanup_expired() periodically.
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

        {
            let map = limiter.windows.lock().unwrap();
            assert_eq!(map.len(), 50);
        }

        limiter.cleanup_expired();

        {
            let map = limiter.windows.lock().unwrap();
            assert_eq!(map.len(), 50);
        }
    }
}
