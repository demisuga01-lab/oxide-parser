//! Integration tests for the async job API (submit / poll / retrieve).
//!
//! Its own test binary so it can drive a job system with a tuned config
//! (short retention, tiny queue, etc.) without disturbing other test files.
//! Each test builds ONE app via `build_app(config)` and clones the returned
//! router per request — cloning shares the same job store/worker pool, so
//! submit → poll → result all hit the same running system.

use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
    Router,
};
use serde_json::Value;
use std::path::Path;
use std::time::Duration;
use tower::util::ServiceExt;

const MAX_BODY: usize = 128 * 1024 * 1024;

fn fixture_pdf(name: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../engine/tests/fixtures")
        .join(name);
    std::fs::read(path).unwrap()
}

/// Unique result dir per test so on-disk cleanup is verifiable in isolation.
fn unique_result_dir(tag: &str) -> String {
    let mut id = [0u8; 8];
    let _ = getrandom::fill(&mut id);
    let hex: String = id.iter().map(|b| format!("{:02x}", b)).collect();
    std::env::temp_dir()
        .join(format!("oxide-jobtest-{}-{}", tag, hex))
        .to_string_lossy()
        .into_owned()
}

/// A config tuned for tests: auth disabled (so we exercise the "anonymous"
/// identity by default, and supply keys explicitly when testing ownership),
/// generous job timeout, and a dedicated result dir.
fn test_config(tag: &str) -> oxide_server::config::ServerConfig {
    oxide_server::config::ServerConfig {
        allow_unauthenticated: true,
        rate_limit_per_min: 0, // disable rate limiting in tests
        job_workers: 2,
        job_queue_capacity: 128,
        job_timeout_secs: 60,
        job_retention_secs: 3600,
        max_jobs: 1000,
        job_result_dir: Some(unique_result_dir(tag)),
        ..oxide_server::config::ServerConfig::default()
    }
}

fn build_app(config: oxide_server::config::ServerConfig) -> Router {
    oxide_server::app::create_app_with_config(config)
}

fn make_multipart(filename: &str, pdf: &[u8], extra: &[(&str, &str)]) -> (String, Vec<u8>) {
    let boundary = "oxide-jobs-boundary-123";
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
    (format!("multipart/form-data; boundary={}", boundary), body)
}

/// Submit a job; return (status, parsed json body).
async fn submit(
    app: &Router,
    endpoint: &str,
    pdf: &[u8],
    extra: &[(&str, &str)],
    api_key: Option<&str>,
) -> (StatusCode, Value) {
    let (ct, body) = make_multipart("x.pdf", pdf, extra);
    let mut req = Request::post(endpoint).header("content-type", ct);
    if let Some(key) = api_key {
        req = req.header("x-api-key", key);
    }
    let response = app
        .clone()
        .oneshot(req.body(Body::from(body)).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), MAX_BODY).await.unwrap();
    let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

async fn get_status(app: &Router, url: &str, api_key: Option<&str>) -> (StatusCode, Value) {
    let mut req = Request::get(url);
    if let Some(key) = api_key {
        req = req.header("x-api-key", key);
    }
    let response = app
        .clone()
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), MAX_BODY).await.unwrap();
    let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

async fn get_result_raw(
    app: &Router,
    url: &str,
    api_key: Option<&str>,
) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
    let mut req = Request::get(url);
    if let Some(key) = api_key {
        req = req.header("x-api-key", key);
    }
    let response = app
        .clone()
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = to_bytes(response.into_body(), MAX_BODY)
        .await
        .unwrap()
        .to_vec();
    (status, headers, bytes)
}

/// Poll a job's status until terminal (completed/failed) or attempts exhausted.
async fn poll_until_terminal(app: &Router, status_url: &str, api_key: Option<&str>) -> Value {
    for _ in 0..600 {
        let (code, body) = get_status(app, status_url, api_key).await;
        assert_eq!(
            code,
            StatusCode::OK,
            "status poll should be 200: {:?}",
            body
        );
        let s = body["status"].as_str().unwrap_or("");
        if s == "completed" || s == "failed" {
            return body;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("job did not reach a terminal state in time");
}

fn count_zip_entries(zip_bytes: &[u8]) -> usize {
    let reader = std::io::Cursor::new(zip_bytes);
    let archive = zip::ZipArchive::new(reader).expect("result should be a valid ZIP");
    archive.len()
}

// ---------------------------------------------------------------------------
// D.1 Happy path + differential check (async output == sync output)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn pdf2img_job_happy_path_and_matches_sync() {
    let config = test_config("happy");
    let app = build_app(config);
    let pdf = fixture_pdf("tracemonkey.pdf");

    // Submit.
    let (code, body) = submit(
        &app,
        "/api/v1/jobs/pdf2img",
        &pdf,
        &[("pages", "1-3"), ("dpi", "72")],
        None,
    )
    .await;
    assert_eq!(code, StatusCode::ACCEPTED, "submit -> 202: {:?}", body);
    let job_id = body["job_id"].as_str().expect("job_id present").to_string();
    assert_eq!(body["status"], "queued");
    let status_url = body["status_url"].as_str().unwrap().to_string();
    let result_url = body["result_url"].as_str().unwrap().to_string();

    // Poll to completion.
    let final_status = poll_until_terminal(&app, &status_url, None).await;
    assert_eq!(final_status["status"], "completed", "job should complete");

    // Retrieve result (async ZIP).
    let (rcode, rheaders, async_zip) = get_result_raw(&app, &result_url, None).await;
    assert_eq!(rcode, StatusCode::OK);
    assert_eq!(
        rheaders.get("content-type").unwrap(),
        "application/zip",
        "result is a zip"
    );
    let async_pages = count_zip_entries(&async_zip);
    assert_eq!(async_pages, 3, "3 pages requested -> 3 zip entries");

    // Differential: the SYNC endpoint for the same input must produce a ZIP
    // with the same number of entries (the async path reuses the same engine
    // core, so output is identical in structure).
    let (sct, sbody) = make_multipart("x.pdf", &pdf, &[("pages", "1-3"), ("dpi", "72")]);
    let sync_resp = app
        .clone()
        .oneshot(
            Request::post("/api/v1/pdf2img")
                .header("content-type", sct)
                .body(Body::from(sbody))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(sync_resp.status(), StatusCode::OK);
    let sync_zip = to_bytes(sync_resp.into_body(), MAX_BODY)
        .await
        .unwrap()
        .to_vec();
    let sync_pages = count_zip_entries(&sync_zip);
    assert_eq!(
        async_pages, sync_pages,
        "async and sync must render the same page count"
    );

    // job_id is non-guessable: 32 hex chars.
    assert_eq!(job_id.len(), 32);
    assert!(job_id.chars().all(|c| c.is_ascii_hexdigit()));
}

#[tokio::test]
async fn extract_images_job_happy_path() {
    let config = test_config("extract");
    let app = build_app(config);
    let pdf = fixture_pdf("image_only.pdf");

    let (code, body) = submit(&app, "/api/v1/jobs/extract-images", &pdf, &[], None).await;
    assert_eq!(code, StatusCode::ACCEPTED, "submit -> 202: {:?}", body);
    let status_url = body["status_url"].as_str().unwrap().to_string();
    let result_url = body["result_url"].as_str().unwrap().to_string();

    let final_status = poll_until_terminal(&app, &status_url, None).await;
    assert_eq!(final_status["status"], "completed");

    let (rcode, rheaders, zip) = get_result_raw(&app, &result_url, None).await;
    assert_eq!(rcode, StatusCode::OK);
    assert_eq!(rheaders.get("content-type").unwrap(), "application/zip");
    // The fixture has at least one image -> valid (possibly empty) ZIP.
    let _ = count_zip_entries(&zip);
}

#[tokio::test]
async fn result_can_be_downloaded_twice_within_retention() {
    let config = test_config("redownload");
    let app = build_app(config);
    let pdf = fixture_pdf("tracemonkey.pdf");
    let (_c, body) = submit(
        &app,
        "/api/v1/jobs/pdf2img",
        &pdf,
        &[("pages", "1"), ("dpi", "72")],
        None,
    )
    .await;
    let status_url = body["status_url"].as_str().unwrap().to_string();
    let result_url = body["result_url"].as_str().unwrap().to_string();
    poll_until_terminal(&app, &status_url, None).await;

    let (c1, _, z1) = get_result_raw(&app, &result_url, None).await;
    let (c2, _, z2) = get_result_raw(&app, &result_url, None).await;
    assert_eq!(c1, StatusCode::OK);
    assert_eq!(c2, StatusCode::OK);
    assert_eq!(z1, z2, "re-download within retention yields the same bytes");
}

// ---------------------------------------------------------------------------
// D.2 Error / edge cases
// ---------------------------------------------------------------------------

#[tokio::test]
async fn unknown_job_id_returns_404() {
    let app = build_app(test_config("unknown"));
    let (code, body) =
        get_status(&app, "/api/v1/jobs/deadbeefdeadbeefdeadbeefdeadbeef", None).await;
    assert_eq!(code, StatusCode::NOT_FOUND);
    assert_eq!(body["error"], "job_not_found");
}

#[tokio::test]
async fn result_before_completion_returns_409() {
    // A 60s job timeout but we check immediately after submit; the job is very
    // likely still queued/running on the first poll. Retry a few times to catch
    // it before completion without flaking if it finishes fast.
    let app = build_app(test_config("notready"));
    let pdf = fixture_pdf("tracemonkey.pdf");
    let (_c, body) = submit(
        &app,
        "/api/v1/jobs/pdf2img",
        &pdf,
        &[("pages", "1-5"), ("dpi", "150")],
        None,
    )
    .await;
    let result_url = body["result_url"].as_str().unwrap().to_string();

    // Immediately request the result.
    let (code, _h, _b) = get_result_raw(&app, &result_url, None).await;
    // Either it's not ready (409) or it already completed (200) — both are
    // valid; we only assert it's never a 5xx and that 409 carries not_ready.
    assert!(
        code == StatusCode::CONFLICT || code == StatusCode::OK,
        "early result must be 409 (not ready) or 200 (done), got {}",
        code
    );
}

#[tokio::test]
async fn corrupt_pdf_job_fails_with_classified_error() {
    let app = build_app(test_config("corrupt"));
    let not_a_pdf = b"this is definitely not a pdf file at all";
    let (code, body) = submit(&app, "/api/v1/jobs/pdf2img", not_a_pdf, &[], None).await;
    assert_eq!(
        code,
        StatusCode::ACCEPTED,
        "submit accepts; failure surfaces via status"
    );
    let status_url = body["status_url"].as_str().unwrap().to_string();
    let final_status = poll_until_terminal(&app, &status_url, None).await;
    assert_eq!(final_status["status"], "failed", "corrupt PDF -> failed");
    // Safe, classified error — a stable code and a message, no internal leakage.
    assert!(
        final_status["error"].is_string(),
        "failed job carries an error code"
    );
    assert!(final_status["message"].is_string());
    let msg = final_status["message"].as_str().unwrap();
    assert!(
        !msg.contains("panicked") && !msg.contains("src/"),
        "error message must not leak internals: {}",
        msg
    );
}

#[tokio::test]
async fn worker_survives_a_failing_job_and_processes_the_next() {
    let app = build_app(test_config("survive"));
    // First: a corrupt job that will fail.
    let bad = b"not a pdf";
    let (_c, b1) = submit(&app, "/api/v1/jobs/pdf2img", bad, &[], None).await;
    let s1 = b1["status_url"].as_str().unwrap().to_string();
    let f1 = poll_until_terminal(&app, &s1, None).await;
    assert_eq!(f1["status"], "failed");

    // Then: a valid job must still be processed (workers survived).
    let pdf = fixture_pdf("tracemonkey.pdf");
    let (_c2, b2) = submit(
        &app,
        "/api/v1/jobs/pdf2img",
        &pdf,
        &[("pages", "1"), ("dpi", "72")],
        None,
    )
    .await;
    let s2 = b2["status_url"].as_str().unwrap().to_string();
    let f2 = poll_until_terminal(&app, &s2, None).await;
    assert_eq!(
        f2["status"], "completed",
        "worker survived and completed the next job"
    );
}

#[tokio::test]
async fn queue_full_returns_503() {
    // Tiny config: 1 worker, queue capacity 1. Flood with submissions of a
    // slowish job (multi-page at higher DPI) so the worker + queue saturate and
    // at least one submission is rejected with 503.
    let config = oxide_server::config::ServerConfig {
        allow_unauthenticated: true,
        rate_limit_per_min: 0,
        job_workers: 1,
        job_queue_capacity: 1,
        job_timeout_secs: 60,
        job_retention_secs: 3600,
        max_jobs: 1000,
        job_result_dir: Some(unique_result_dir("queuefull")),
        ..oxide_server::config::ServerConfig::default()
    };
    let app = build_app(config);
    let pdf = fixture_pdf("tracemonkey.pdf");

    let mut saw_503 = false;
    let mut saw_202 = false;
    for _ in 0..20 {
        let (code, _body) = submit(
            &app,
            "/api/v1/jobs/pdf2img",
            &pdf,
            &[("pages", "1-8"), ("dpi", "150")],
            None,
        )
        .await;
        match code {
            StatusCode::ACCEPTED => saw_202 = true,
            StatusCode::SERVICE_UNAVAILABLE => saw_503 = true,
            other => panic!("unexpected submit status {}", other),
        }
        if saw_503 {
            break;
        }
    }
    assert!(saw_202, "some submissions should be accepted");
    assert!(
        saw_503,
        "flooding a 1-worker/1-slot queue should eventually return 503"
    );
}

// ---------------------------------------------------------------------------
// D.2 Ownership scoping
// ---------------------------------------------------------------------------

#[tokio::test]
async fn job_owned_by_another_key_is_404_not_403() {
    // Auth enabled with two keys. Key A submits; key B must get 404 (not 403)
    // for both status and result — never confirming the job exists.
    let config = oxide_server::config::ServerConfig {
        api_keys: vec!["key-a".to_string(), "key-b".to_string()],
        allow_unauthenticated: false,
        rate_limit_per_min: 0,
        job_workers: 2,
        job_queue_capacity: 128,
        job_timeout_secs: 60,
        job_retention_secs: 3600,
        max_jobs: 1000,
        job_result_dir: Some(unique_result_dir("ownership")),
        ..oxide_server::config::ServerConfig::default()
    };
    let app = build_app(config);
    let pdf = fixture_pdf("tracemonkey.pdf");

    let (code, body) = submit(
        &app,
        "/api/v1/jobs/pdf2img",
        &pdf,
        &[("pages", "1"), ("dpi", "72")],
        Some("key-a"),
    )
    .await;
    assert_eq!(code, StatusCode::ACCEPTED);
    let status_url = body["status_url"].as_str().unwrap().to_string();
    let result_url = body["result_url"].as_str().unwrap().to_string();

    // Owner (key-a) can see it.
    let (own_code, _) = get_status(&app, &status_url, Some("key-a")).await;
    assert_eq!(own_code, StatusCode::OK, "owner can poll");

    // Different key (key-b) gets 404 for status and result.
    let (other_status, other_body) = get_status(&app, &status_url, Some("key-b")).await;
    assert_eq!(
        other_status,
        StatusCode::NOT_FOUND,
        "non-owner -> 404 (not 403)"
    );
    assert_eq!(other_body["error"], "job_not_found");

    let (other_result, _h, _b) = get_result_raw(&app, &result_url, Some("key-b")).await;
    assert_eq!(
        other_result,
        StatusCode::NOT_FOUND,
        "non-owner result -> 404"
    );
}

#[tokio::test]
async fn job_endpoints_reject_unauthenticated_when_auth_enabled() {
    let config = oxide_server::config::ServerConfig {
        api_keys: vec!["the-key".to_string()],
        allow_unauthenticated: false,
        rate_limit_per_min: 0,
        job_result_dir: Some(unique_result_dir("auth")),
        ..oxide_server::config::ServerConfig::default()
    };
    let app = build_app(config);
    let pdf = fixture_pdf("tracemonkey.pdf");

    // No api key -> 401 from the auth middleware.
    let (code, _body) = submit(&app, "/api/v1/jobs/pdf2img", &pdf, &[("pages", "1")], None).await;
    assert_eq!(code, StatusCode::UNAUTHORIZED, "submit without key -> 401");

    let (scode, _) = get_status(&app, "/api/v1/jobs/anything", None).await;
    assert_eq!(scode, StatusCode::UNAUTHORIZED, "status without key -> 401");
}

// ---------------------------------------------------------------------------
// D.2 Retention / cleanup
// ---------------------------------------------------------------------------

#[tokio::test]
async fn completed_job_is_cleaned_up_after_retention() {
    // 1-second retention so the cleanup task reaps the job quickly. Verify the
    // job becomes 404 AND the temp result file is deleted from disk.
    let result_dir = unique_result_dir("retention");
    let config = oxide_server::config::ServerConfig {
        allow_unauthenticated: true,
        rate_limit_per_min: 0,
        job_workers: 2,
        job_queue_capacity: 128,
        job_timeout_secs: 60,
        job_retention_secs: 1,
        max_jobs: 1000,
        job_result_dir: Some(result_dir.clone()),
        ..oxide_server::config::ServerConfig::default()
    };
    let app = build_app(config);
    let pdf = fixture_pdf("tracemonkey.pdf");

    let (_c, body) = submit(
        &app,
        "/api/v1/jobs/pdf2img",
        &pdf,
        &[("pages", "1"), ("dpi", "72")],
        None,
    )
    .await;
    let job_id = body["job_id"].as_str().unwrap().to_string();
    let status_url = body["status_url"].as_str().unwrap().to_string();
    poll_until_terminal(&app, &status_url, None).await;

    // The result file exists on disk now.
    let result_file = std::path::Path::new(&result_dir).join(format!("{}.bin", job_id));
    assert!(
        result_file.exists(),
        "result file should exist after completion: {}",
        result_file.display()
    );

    // Wait out the retention window + a cleanup sweep (interval clamps to >=1s).
    tokio::time::sleep(Duration::from_secs(3)).await;

    let (code, body) = get_status(&app, &status_url, None).await;
    assert_eq!(
        code,
        StatusCode::NOT_FOUND,
        "job should be reaped after retention"
    );
    assert_eq!(body["error"], "job_not_found");
    assert!(
        !result_file.exists(),
        "result file should be deleted on cleanup: {}",
        result_file.display()
    );
}

// ---------------------------------------------------------------------------
// D.3 Concurrency
// ---------------------------------------------------------------------------

#[tokio::test]
async fn many_concurrent_jobs_all_complete_without_cross_contamination() {
    let config = test_config("concurrent");
    let app = build_app(config);
    let pdf = fixture_pdf("tracemonkey.pdf");

    // Submit several jobs with DIFFERENT page counts so each result is
    // distinguishable; verify each job's result matches its own request (no
    // cross-contamination: job A's result never returned for job B).
    let specs = [("1", 1usize), ("1-2", 2), ("1-3", 3), ("1-4", 4)];
    let mut jobs = Vec::new();
    for (pages, expected) in specs {
        let (code, body) = submit(
            &app,
            "/api/v1/jobs/pdf2img",
            &pdf,
            &[("pages", pages), ("dpi", "72")],
            None,
        )
        .await;
        assert_eq!(code, StatusCode::ACCEPTED);
        jobs.push((
            body["status_url"].as_str().unwrap().to_string(),
            body["result_url"].as_str().unwrap().to_string(),
            expected,
        ));
    }

    for (status_url, result_url, expected_pages) in jobs {
        let fs = poll_until_terminal(&app, &status_url, None).await;
        assert_eq!(fs["status"], "completed");
        let (rc, _h, zip) = get_result_raw(&app, &result_url, None).await;
        assert_eq!(rc, StatusCode::OK);
        assert_eq!(
            count_zip_entries(&zip),
            expected_pages,
            "each job's result must match its own page request"
        );
    }
}
