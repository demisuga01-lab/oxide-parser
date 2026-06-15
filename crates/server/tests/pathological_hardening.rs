//! Resource-safety hardening tests: per-request timeout (cooperative
//! cancellation) and resource limits, driven by deliberately pathological but
//! WELL-FORMED PDFs. Asserts the server degrades gracefully — clean specific
//! error, bounded resource use, and still healthy afterward — rather than
//! hanging or OOMing.
//!
//! This file is its own test binary, so the process-global `CONFIG` OnceLock
//! can be set once here to a tuned configuration (short timeout, tight caps)
//! without affecting other test files.

use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
};
use std::time::{Duration, Instant};
use tower::util::ServiceExt;

#[path = "pathological/mod.rs"]
mod pathological;

/// Install a tuned config for this test binary. Idempotent across tests in this
/// file (OnceLock::set only takes the first). A 2s timeout and tight caps make
/// the pathological behavior observable quickly.
fn install_test_config() {
    let cfg = oxide_server::config::ServerConfig {
        request_timeout_secs: 2,
        max_render_pixels: 100_000_000, // 100 MP, same as prod default
        max_output_bytes: 8 * 1024 * 1024, // 8 MiB so output explosion is reachable
        max_image_count: 10_000,
        max_pages: 200,
        ..oxide_server::config::ServerConfig::default()
    };
    let _ = oxide_server::config::CONFIG.set(cfg);
}

fn make_multipart(filename: &str, pdf: &[u8], extra: &[(&str, &str)]) -> (String, Vec<u8>) {
    let boundary = "oxide-pathological-boundary";
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

async fn post_pdf2img(pdf: &[u8], extra: &[(&str, &str)]) -> (StatusCode, Vec<u8>) {
    let (ct, body) = make_multipart("x.pdf", pdf, extra);
    let app = oxide_server::app::create_app();
    let response = app
        .oneshot(
            Request::post("/api/v1/pdf2img")
                .header("content-type", ct)
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 64 * 1024 * 1024)
        .await
        .unwrap()
        .to_vec();
    (status, bytes)
}

/// A normal small page renders successfully and is unaffected by the limits.
#[tokio::test]
async fn normal_request_succeeds_under_limits() {
    install_test_config();
    let pdf = pathological::many_pages_pdf(1);
    let (status, bytes) = post_pdf2img(&pdf, &[("dpi", "72")]).await;
    assert_eq!(status, StatusCode::OK, "normal render should succeed");
    assert!(bytes.starts_with(b"PK"), "should be a ZIP");
}

/// A page engineered to render slowly must time out with a clean 503 within a
/// bounded time (not hang), proving the cooperative cancellation actually stops
/// the CPU-bound work.
#[tokio::test]
async fn slow_render_times_out_cleanly() {
    install_test_config();
    // Many full-page fills at high DPI => far more than 2s of rasterization.
    let pdf = pathological::huge_operator_count_pdf(40_000);
    let start = Instant::now();
    let (status, _bytes) = post_pdf2img(&pdf, &[("dpi", "300")]).await;
    let elapsed = start.elapsed();

    assert_eq!(
        status,
        StatusCode::SERVICE_UNAVAILABLE,
        "pathological render should return a clean timeout (503)"
    );
    // Timeout is 2s; allow generous slack for the cooperative check interval
    // plus the outer async backstop margin. The key assertion is that it
    // terminates at all rather than running to completion (which would be far
    // longer) or hanging forever.
    assert!(
        elapsed < Duration::from_secs(20),
        "request should terminate promptly, took {:?}",
        elapsed
    );
}

/// After several pathological timeout requests, a normal request must still
/// succeed — proving the worker threads were actually FREED (cooperative
/// cancellation stopped them) rather than leaked still pegging CPU.
#[tokio::test]
async fn server_stays_responsive_after_pathological_requests() {
    install_test_config();
    let slow = pathological::huge_operator_count_pdf(40_000);
    for _ in 0..3 {
        let (status, _) = post_pdf2img(&slow, &[("dpi", "300")]).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    }
    // Now a normal request: if threads had leaked, the blocking pool could be
    // saturated and this would stall. It should complete promptly.
    let normal = pathological::many_pages_pdf(1);
    let start = Instant::now();
    let (status, bytes) = post_pdf2img(&normal, &[("dpi", "72")]).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "server must remain responsive after pathological requests"
    );
    assert!(bytes.starts_with(b"PK"));
    assert!(
        start.elapsed() < Duration::from_secs(10),
        "follow-up normal request should be prompt"
    );
}

/// A giant MediaBox would explode the pixel count; the pre-allocation pixel cap
/// must reject it with a clean 413 rather than allocating gigabytes.
#[tokio::test]
async fn giant_mediabox_rejected_before_allocation() {
    install_test_config();
    let pdf = pathological::giant_mediabox_pdf();
    // 6000pt square at 150 DPI = 12500x12500 = ~156 MP, over the 100 MP cap.
    let (status, body) = post_pdf2img(&pdf, &[("dpi", "150")]).await;
    assert_eq!(
        status,
        StatusCode::PAYLOAD_TOO_LARGE,
        "pixel explosion should be rejected with 413"
    );
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["error"], "resource_limit");
}

/// The same giant-MediaBox page at a tiny DPI stays under the pixel cap and
/// renders fine — proving the cap keys on actual pixels, not page size alone.
#[tokio::test]
async fn giant_mediabox_ok_at_tiny_dpi() {
    install_test_config();
    let pdf = pathological::giant_mediabox_pdf();
    // 6000pt at 24 DPI = 2000x2000 = ~4 MP, well under the 100 MP cap.
    let (status, bytes) = post_pdf2img(&pdf, &[("dpi", "24")]).await;
    assert_eq!(status, StatusCode::OK, "should render under the pixel cap");
    assert!(bytes.starts_with(b"PK"));
}

/// Deeply nested Form XObjects past the depth-8 guard must not overflow the
/// stack; the page should still render and return a valid ZIP.
#[tokio::test]
async fn deeply_nested_forms_degrade_gracefully() {
    install_test_config();
    let pdf = pathological::deeply_nested_forms_pdf(64);
    let (status, bytes) = post_pdf2img(&pdf, &[("dpi", "72")]).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "depth guard should hold and the page still render"
    );
    assert!(bytes.starts_with(b"PK"));
}

/// A self-referential Form (A->A cycle) must not infinitely recurse; the page
/// renders and returns cleanly.
#[tokio::test]
async fn self_referential_form_terminates() {
    install_test_config();
    let pdf = pathological::self_referential_form_pdf();
    let (status, bytes) = post_pdf2img(&pdf, &[("dpi", "72")]).await;
    assert_eq!(status, StatusCode::OK);
    assert!(bytes.starts_with(b"PK"));
}

/// A pathological tiling pattern (tiny step, huge fill) must hit the tile cap
/// and/or timeout and terminate, not hang. Either a clean render (cap skipped
/// the pattern) or a 503 timeout is acceptable; a hang is not.
#[tokio::test]
async fn pathological_tiling_pattern_terminates() {
    install_test_config();
    let pdf = pathological::pathological_tiling_pattern_pdf();
    let start = Instant::now();
    let (status, _bytes) = post_pdf2img(&pdf, &[("dpi", "72")]).await;
    assert!(
        status == StatusCode::OK || status == StatusCode::SERVICE_UNAVAILABLE,
        "tiling pattern should render (cap) or time out, got {}",
        status
    );
    assert!(
        start.elapsed() < Duration::from_secs(20),
        "must terminate promptly"
    );
}

/// Too many requested pages is rejected by the page cap (400-class), not run.
#[tokio::test]
async fn too_many_pages_rejected() {
    install_test_config();
    // 250 pages > the 200 max_pages default.
    let pdf = pathological::many_pages_pdf(250);
    let (status, _bytes) = post_pdf2img(&pdf, &[("dpi", "72")]).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "exceeding the page cap should be a clean 4xx"
    );
}
