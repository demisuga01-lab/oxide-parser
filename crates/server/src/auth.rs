use std::sync::Arc;

use axum::{
    body::Body,
    extract::State,
    http::{Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use subtle::ConstantTimeEq;

use crate::config::ServerConfig;

pub(crate) fn extract_api_key(req: &Request<Body>) -> Option<String> {
    extract_api_key_from_headers(req.headers())
}

/// Extract the API key from raw headers (X-API-Key, or Authorization: Bearer).
/// Used both by the middleware (via [`extract_api_key`]) and by the job handlers
/// to determine the submitting identity for ownership scoping.
pub(crate) fn extract_api_key_from_headers(headers: &axum::http::HeaderMap) -> Option<String> {
    if let Some(value) = headers.get("x-api-key") {
        if let Ok(key) = value.to_str() {
            return Some(key.to_string());
        }
    }

    if let Some(value) = headers.get("authorization") {
        if let Ok(header) = value.to_str() {
            if let Some(key) = header.strip_prefix("Bearer ") {
                return Some(key.to_string());
            }
        }
    }

    None
}

/// The identity that owns a submitted job: the API key when auth is enforced,
/// otherwise a single shared "anonymous" identity (dev mode). Scoping jobs to
/// this value prevents one caller reading another's results.
pub(crate) fn caller_identity(headers: &axum::http::HeaderMap) -> String {
    extract_api_key_from_headers(headers).unwrap_or_else(|| "anonymous".to_string())
}

/// Constant-time check of a provided key against the configured allowlist.
///
/// We compare the provided key against EVERY configured key (never breaking
/// early on a match) and use a constant-time byte comparison for each, so the
/// time taken does not reveal how many leading bytes matched a real key. This
/// avoids a timing side-channel that could let an attacker guess keys byte by
/// byte. Comparing against all keys also keeps the per-request cost independent
/// of which key matched.
pub(crate) fn is_valid_key(provided: &str, configured: &[String]) -> bool {
    let provided_bytes = provided.as_bytes();
    let mut matched = false;
    for key in configured {
        // `ct_eq` on equal-length byte slices is constant-time. Length
        // mismatches short-circuit (length is not secret), then we OR the
        // result in without branching on it.
        let key_bytes = key.as_bytes();
        if key_bytes.len() == provided_bytes.len() {
            let eq: bool = key_bytes.ct_eq(provided_bytes).into();
            matched |= eq;
        }
    }
    matched
}

pub async fn auth_middleware(
    State(config): State<Arc<ServerConfig>>,
    req: Request<Body>,
    next: Next,
) -> Response {
    // Fail-closed posture is enforced at startup (ServerConfig::validate). Here
    // we simply enforce: if keys are configured, every non-probe endpoint needs
    // a valid one. If no keys are configured the server only got this far via
    // the explicit dev opt-in, so requests pass through.
    if !config.auth_enforced() {
        return next.run(req).await;
    }

    let path = req.uri().path();
    if is_public_probe(path) {
        return next.run(req).await;
    }

    let provided_key = match extract_api_key(&req) {
        Some(key) => key,
        None => {
            return (
                StatusCode::UNAUTHORIZED,
                axum::Json(serde_json::json!({
                    "error": "missing_api_key",
                    "message": "Provide your API key via X-API-Key header or Authorization: Bearer <key>"
                })),
            )
                .into_response()
        }
    };

    if !is_valid_key(&provided_key, &config.api_keys) {
        return (
            StatusCode::UNAUTHORIZED,
            axum::Json(serde_json::json!({
                "error": "invalid_api_key",
                "message": "The provided API key is not valid"
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
    use axum::http::Method;

    use super::*;

    #[test]
    fn extract_api_key_from_x_api_key_header() {
        let req = Request::builder()
            .method(Method::GET)
            .uri("/api/v1/test")
            .header("x-api-key", "test-key-123")
            .body(Body::empty())
            .unwrap();

        assert_eq!(extract_api_key(&req), Some("test-key-123".to_string()));
    }

    #[test]
    fn extract_api_key_from_authorization_bearer() {
        let req = Request::builder()
            .method(Method::GET)
            .uri("/api/v1/test")
            .header("authorization", "Bearer my-secret-key")
            .body(Body::empty())
            .unwrap();

        assert_eq!(extract_api_key(&req), Some("my-secret-key".to_string()));
    }

    #[test]
    fn extract_api_key_returns_none_when_no_header() {
        let req = Request::builder()
            .method(Method::GET)
            .uri("/api/v1/test")
            .body(Body::empty())
            .unwrap();

        assert!(extract_api_key(&req).is_none());
    }

    #[test]
    fn is_valid_key_accepts_a_configured_key() {
        let keys = vec!["alpha".to_string(), "beta".to_string()];
        assert!(is_valid_key("alpha", &keys));
        assert!(is_valid_key("beta", &keys));
    }

    #[test]
    fn is_valid_key_rejects_unknown_key() {
        let keys = vec!["alpha".to_string(), "beta".to_string()];
        assert!(!is_valid_key("gamma", &keys));
        assert!(!is_valid_key("", &keys));
    }

    #[test]
    fn is_valid_key_rejects_prefix_of_a_real_key() {
        // A shorter key that is a prefix must not be accepted (length differs).
        let keys = vec!["supersecretkey".to_string()];
        assert!(!is_valid_key("super", &keys));
        assert!(!is_valid_key("supersecretkeyy", &keys));
    }

    #[test]
    fn is_valid_key_against_empty_list_is_always_false() {
        let keys: Vec<String> = Vec::new();
        assert!(!is_valid_key("anything", &keys));
    }
}
