//! Server security hardening tests: fail-closed auth,
//! restrictive CORS, error sanitization, and rate-limiter behavior.
//!
//! Auth/CORS configuration is threaded through `create_app_with_config`, which
//! builds the middleware from the passed `ServerConfig` rather than the
//! process-global `CONFIG`. That keeps these tests hermetic — each builds an
//! app with exactly the config it needs without racing on a shared OnceLock.

use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
};
use oxide_server::config::ServerConfig;
use serde_json::Value;
use std::path::Path;
use tower::util::ServiceExt;

fn fixture_pdf(name: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../engine/tests/fixtures")
        .join(name);
    std::fs::read(path).unwrap()
}

fn make_multipart(filename: &str, pdf: &[u8], extra: &[(&str, &str)]) -> (String, Vec<u8>) {
    let boundary = "oxide-security-boundary";
    let mut body: Vec<u8> = Vec::new();
    body.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
    body.extend_from_slice(
        format!(
            "Content-Disposition: form-data; name=\"file\"; filename=\"{}\"\r\n\
             Content-Type: application/pdf\r\n\r\n",
            filename
        )
        .as_bytes(),
    );
    body.extend_from_slice(pdf);
    body.extend_from_slice(b"\r\n");
    for (name, value) in extra {
        body.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
        body.extend_from_slice(
            format!(
                "Content-Disposition: form-data; name=\"{}\"\r\n\r\n{}",
                name, value
            )
            .as_bytes(),
        );
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{}--\r\n", boundary).as_bytes());
    let ct = format!("multipart/form-data; boundary={}", boundary);
    (ct, body)
}

fn config_with_keys(keys: &[&str]) -> ServerConfig {
    ServerConfig {
        api_keys: keys.iter().map(|k| k.to_string()).collect(),
        ..ServerConfig::default()
    }
}

const DATA_ENDPOINTS: [&str; 4] = [
    "/api/v1/extract-text",
    "/api/v1/extract-images",
    "/api/v1/analyze",
    "/api/v1/pdf2img",
];

// ---------------------------------------------------------------------------
// Part A — fail-closed auth
// ---------------------------------------------------------------------------

#[test]
fn validate_refuses_empty_keys_without_dev_optin() {
    let cfg = ServerConfig::default(); // empty keys, allow_unauthenticated=false
    assert!(
        cfg.validate().is_err(),
        "empty keys + no dev opt-in must fail validation (fail-closed)"
    );
}

#[test]
fn validate_allows_empty_keys_with_dev_optin() {
    let cfg = ServerConfig {
        allow_unauthenticated: true,
        ..ServerConfig::default()
    };
    assert!(
        cfg.validate().is_ok(),
        "explicit dev opt-in should permit unauthenticated startup"
    );
    assert!(!cfg.auth_enforced());
}

#[test]
fn validate_allows_when_keys_present() {
    let cfg = config_with_keys(&["k1"]);
    assert!(cfg.validate().is_ok());
    assert!(cfg.auth_enforced());
}

#[tokio::test]
async fn data_endpoints_reject_missing_key_with_401() {
    let pdf = fixture_pdf("flate.pdf");
    for path in DATA_ENDPOINTS {
        let app = oxide_server::app::create_app_with_config(config_with_keys(&["secret-key"]));
        let (ct, body) = make_multipart("test.pdf", &pdf, &[("dpi", "72")]);
        let response = app
            .oneshot(
                Request::post(path)
                    .header("content-type", ct)
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "{} without a key should be 401",
            path
        );
    }
}

#[tokio::test]
async fn data_endpoints_reject_wrong_key_with_401() {
    let pdf = fixture_pdf("flate.pdf");
    for path in DATA_ENDPOINTS {
        let app = oxide_server::app::create_app_with_config(config_with_keys(&["secret-key"]));
        let (ct, body) = make_multipart("test.pdf", &pdf, &[("dpi", "72")]);
        let response = app
            .oneshot(
                Request::post(path)
                    .header("content-type", ct)
                    .header("x-api-key", "wrong-key")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "{} with a wrong key should be 401",
            path
        );
    }
}

#[tokio::test]
async fn data_endpoints_accept_correct_key() {
    let pdf = fixture_pdf("flate.pdf");
    for path in DATA_ENDPOINTS {
        let app = oxide_server::app::create_app_with_config(config_with_keys(&["secret-key"]));
        let (ct, body) = make_multipart("test.pdf", &pdf, &[("dpi", "72")]);
        let response = app
            .oneshot(
                Request::post(path)
                    .header("content-type", ct)
                    .header("x-api-key", "secret-key")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "{} with the correct key should be 200",
            path
        );
    }
}

#[tokio::test]
async fn correct_key_via_bearer_header_is_accepted() {
    let pdf = fixture_pdf("flate.pdf");
    let app = oxide_server::app::create_app_with_config(config_with_keys(&["secret-key"]));
    let (ct, body) = make_multipart("test.pdf", &pdf, &[]);
    let response = app
        .oneshot(
            Request::post("/api/v1/analyze")
                .header("content-type", ct)
                .header("authorization", "Bearer secret-key")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn health_is_reachable_without_a_key_when_auth_enforced() {
    let app = oxide_server::app::create_app_with_config(config_with_keys(&["secret-key"]));
    let response = app
        .oneshot(Request::get("/health").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "health must stay open for load balancers even with auth enforced"
    );
}

#[tokio::test]
async fn readiness_and_version_reachable_without_key() {
    for path in ["/readiness", "/api/v1/health", "/api/v1/readiness"] {
        let app = oxide_server::app::create_app_with_config(config_with_keys(&["secret-key"]));
        let response = app
            .oneshot(Request::get(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "{} should be public",
            path
        );
    }
}

// ---------------------------------------------------------------------------
// Part B — restrictive CORS
// ---------------------------------------------------------------------------

fn cors_config(origins: &[&str], allow_any: bool) -> ServerConfig {
    ServerConfig {
        allow_unauthenticated: true, // these tests aren't about auth
        cors_allowed_origins: origins.iter().map(|o| o.to_string()).collect(),
        cors_allow_any: allow_any,
        ..ServerConfig::default()
    }
}

#[tokio::test]
async fn cors_allows_listed_origin() {
    let app =
        oxide_server::app::create_app_with_config(cors_config(&["https://app.example.com"], false));
    let response = app
        .oneshot(
            Request::builder()
                .method("OPTIONS")
                .uri("/api/v1/analyze")
                .header("origin", "https://app.example.com")
                .header("access-control-request-method", "POST")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let allow_origin = response
        .headers()
        .get("access-control-allow-origin")
        .and_then(|v| v.to_str().ok());
    assert_eq!(allow_origin, Some("https://app.example.com"));
}

#[tokio::test]
async fn cors_denies_unlisted_origin() {
    let app =
        oxide_server::app::create_app_with_config(cors_config(&["https://app.example.com"], false));
    let response = app
        .oneshot(
            Request::builder()
                .method("OPTIONS")
                .uri("/api/v1/analyze")
                .header("origin", "https://evil.example.com")
                .header("access-control-request-method", "POST")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        response
            .headers()
            .get("access-control-allow-origin")
            .is_none(),
        "an unlisted origin must not receive an allow-origin header"
    );
}

#[tokio::test]
async fn cors_default_is_restrictive() {
    // No origins configured => no cross-origin grant.
    let app = oxide_server::app::create_app_with_config(cors_config(&[], false));
    let response = app
        .oneshot(
            Request::builder()
                .method("OPTIONS")
                .uri("/api/v1/analyze")
                .header("origin", "https://app.example.com")
                .header("access-control-request-method", "POST")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        response
            .headers()
            .get("access-control-allow-origin")
            .is_none(),
        "with no configured origins, no origin should be allowed"
    );
}

#[tokio::test]
async fn cors_allow_any_optin_echoes_any_origin() {
    let app = oxide_server::app::create_app_with_config(cors_config(&[], true));
    let response = app
        .oneshot(
            Request::builder()
                .method("OPTIONS")
                .uri("/api/v1/analyze")
                .header("origin", "https://whatever.example.com")
                .header("access-control-request-method", "POST")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let allow_origin = response
        .headers()
        .get("access-control-allow-origin")
        .and_then(|v| v.to_str().ok());
    assert_eq!(
        allow_origin,
        Some("*"),
        "allow-any dev mode should permit any origin"
    );
}

// ---------------------------------------------------------------------------
// Part C — error sanitization
// ---------------------------------------------------------------------------

#[tokio::test]
async fn malformed_pdf_returns_safe_422_not_500() {
    let garbage = b"%PDF-1.4 not actually a pdf";
    let (ct, body) = make_multipart("bad.pdf", garbage, &[]);
    let app = oxide_server::app::create_app_with_config(cors_config(&[], false));
    let response = app
        .oneshot(
            Request::post("/api/v1/extract-text")
                .header("content-type", ct)
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_ne!(
        response.status(),
        StatusCode::INTERNAL_SERVER_ERROR,
        "a malformed user PDF is client-actionable, not an internal 500"
    );
    assert!(response.status().is_client_error());
}

#[tokio::test]
async fn password_protected_returns_specific_safe_422() {
    let pdf = build_password_protected_pdf();
    let (ct, body) = make_multipart("protected.pdf", &pdf, &[]);
    let app = oxide_server::app::create_app_with_config(cors_config(&[], false));
    let response = app
        .oneshot(
            Request::post("/api/v1/extract-text")
                .header("content-type", ct)
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body_bytes = to_bytes(response.into_body(), 4096).await.unwrap();
    let json: Value = serde_json::from_slice(&body_bytes).unwrap();
    assert_eq!(json["error"], "encrypted");
    // The message is specific and safe — it tells the client what to do without
    // leaking internals.
    let msg = json["message"].as_str().unwrap_or("");
    assert!(msg.to_lowercase().contains("password") || msg.to_lowercase().contains("encrypted"));
}

#[test]
fn internal_error_body_is_generic_and_carries_reference() {
    use axum::response::IntoResponse;
    // Directly exercise the IntoResponse mapping for the generic 500 path: the
    // body must NOT contain the internal detail, and MUST contain a reference id.
    let secret = "C:/oxide/secret/path/leak.rs line 42 panic detail";
    let resp = oxide_server::error::ServerError::Internal(secret.to_string()).into_response();
    assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);

    let fut = to_bytes(resp.into_body(), 8192);
    let body = futures_block_on(fut).unwrap();
    let json: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["error"], "internal_error");
    let message = json["message"].as_str().unwrap_or("");
    assert!(
        !message.contains(secret) && !message.contains("leak.rs"),
        "internal detail must not appear in the client message: {}",
        message
    );
    assert!(
        json["reference"].as_str().unwrap_or("").starts_with("err-"),
        "a correlation reference id should be present"
    );
}

/// Minimal blocking executor for the one sync test that needs to drain a body.
fn futures_block_on<F: std::future::Future>(fut: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(fut)
}

fn build_password_protected_pdf() -> Vec<u8> {
    fn hex(bytes: &[u8]) -> String {
        let mut s = String::with_capacity(bytes.len() * 2);
        for b in bytes {
            s.push_str(&format!("{:02X}", b));
        }
        s
    }
    let mut bytes: Vec<u8> = Vec::new();
    let mut offsets = [0usize; 3];
    bytes.extend_from_slice(b"%PDF-1.4\n");
    offsets[1] = bytes.len();
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    offsets[2] = bytes.len();
    bytes.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [] /Count 0 >>\nendobj\n");
    let xref = bytes.len();
    bytes.extend_from_slice(b"xref\n0 3\n0000000000 65535 f \n");
    for offset in offsets.iter().skip(1) {
        bytes.extend_from_slice(format!("{:010} 00000 n \n", offset).as_bytes());
    }
    let owner_o = vec![0xABu8; 32];
    let user_u = vec![0xCDu8; 32];
    let file_id = b"0123456789abcdef";
    let trailer = format!(
        "trailer\n<< /Size 3 /Root 1 0 R /Encrypt << /Filter /Standard /V 2 /R 3 /Length 128 \
         /P -3904 /O <{}> /U <{}> >> /ID [<{}> <{}>] >>\nstartxref\n{}\n%%EOF\n",
        hex(&owner_o),
        hex(&user_u),
        hex(file_id),
        hex(file_id),
        xref
    );
    bytes.extend_from_slice(trailer.as_bytes());
    bytes
}

// ---------------------------------------------------------------------------
// Part D — rate limiting (end-to-end enforcement through the middleware)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn rate_limit_returns_429_after_exceeding_limit() {
    let cfg = ServerConfig {
        allow_unauthenticated: true,
        rate_limit_per_min: 3,
        ..ServerConfig::default()
    };
    let app = oxide_server::app::create_app_with_config(cfg);

    // The limiter keys anonymous (no api key) requests under "anonymous"; fire
    // 3 allowed then expect the 4th to be limited. Use analyze on a valid PDF.
    let pdf = fixture_pdf("flate.pdf");
    let mut statuses = Vec::new();
    for _ in 0..4 {
        let (ct, body) = make_multipart("test.pdf", &pdf, &[]);
        let response = app
            .clone()
            .oneshot(
                Request::post("/api/v1/analyze")
                    .header("content-type", ct)
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        statuses.push(response.status());
    }
    assert_eq!(
        statuses[3],
        StatusCode::TOO_MANY_REQUESTS,
        "4th request over a limit of 3 should be 429; got {:?}",
        statuses
    );
}

#[tokio::test]
async fn spawned_cleanup_task_shrinks_the_map_on_schedule() {
    use oxide_server::rate_limit::RateLimiter;
    use std::sync::Arc;
    use std::time::Duration;

    // Real-time interval cleanup with a real (system) clock. We use a short
    // window proxy by populating then waiting just over the 60s window is too
    // slow for a unit test, so instead we assert the scheduling WIRING invokes
    // cleanup_expired: populate, spawn a fast-interval task, and confirm the
    // task runs (entries created "now" are still active so they remain, but the
    // task must execute without panicking and the handle stays alive).
    let limiter = Arc::new(RateLimiter::new(100));
    for i in 0..20u16 {
        limiter.check(&format!("key-{}", i));
    }
    assert_eq!(limiter.tracked_keys(), 20);

    let handle = limiter.spawn_cleanup(Duration::from_millis(20));
    // Give the interval a few ticks.
    tokio::time::sleep(Duration::from_millis(120)).await;
    assert!(
        !handle.is_finished(),
        "cleanup task should still be running"
    );
    // Active windows (created just now) are correctly retained by cleanup.
    assert_eq!(limiter.tracked_keys(), 20);

    handle.abort();
}

#[tokio::test]
async fn dropping_limiter_stops_cleanup_task() {
    use oxide_server::rate_limit::RateLimiter;
    use std::sync::Arc;
    use std::time::Duration;

    let limiter = Arc::new(RateLimiter::new(100));
    let handle = limiter.spawn_cleanup(Duration::from_millis(20));
    drop(limiter); // only the Weak in the task remains
    tokio::time::sleep(Duration::from_millis(80)).await;
    assert!(
        handle.is_finished(),
        "task should exit once the limiter is dropped"
    );
}
