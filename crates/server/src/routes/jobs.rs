//! HTTP handlers for the async job API:
//!
//! - `POST /api/v1/jobs/pdf2img`        — submit a render job
//! - `POST /api/v1/jobs/extract-images` — submit an extract job
//! - `GET  /api/v1/jobs/{id}`           — poll status / progress
//! - `GET  /api/v1/jobs/{id}/result`    — download the result once completed
//!
//! All four are auth-gated by the same middleware as the sync endpoints. Status
//! and result are additionally scoped to the submitting identity: a job owned by
//! a different key surfaces as 404 (never 403), so the endpoint does not confirm
//! the existence of another caller's job.

use axum::{
    extract::{Multipart, Path, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};

use crate::auth::caller_identity;
use crate::error::ServerResult;
use crate::jobs::{JobKind, JobStatus, JobsState, SubmitOutcome};

/// POST /api/v1/jobs/pdf2img — enqueue a render job.
pub async fn submit_pdf2img(
    State(jobs): State<JobsState>,
    headers: HeaderMap,
    multipart: Multipart,
) -> ServerResult<Response> {
    let params = crate::routes::pdf2img::extract_pdf2img_fields(multipart).await?;
    let owner = caller_identity(&headers);
    submit_response(&jobs, owner, JobKind::Pdf2Img(params), "pdf2img")
}

/// POST /api/v1/jobs/extract-images — enqueue an extract-images (ZIP) job.
///
/// JSON metadata mode is intentionally NOT offered async: it's lightweight (no
/// image bytes decoded) and belongs on the synchronous endpoint. We force ZIP
/// mode here regardless of the Accept header.
pub async fn submit_extract_images(
    State(jobs): State<JobsState>,
    headers: HeaderMap,
    multipart: Multipart,
) -> ServerResult<Response> {
    // accept_json = false: the heavy ZIP path is the only async variant.
    let params =
        crate::routes::extract_images::extract_extract_images_fields(multipart, false).await?;
    let owner = caller_identity(&headers);
    submit_response(
        &jobs,
        owner,
        JobKind::ExtractImages(params),
        "extract-images",
    )
}

fn submit_response(
    jobs: &JobsState,
    owner: String,
    kind: JobKind,
    label: &str,
) -> ServerResult<Response> {
    match jobs.system.submit(owner, kind) {
        SubmitOutcome::Accepted(id) => {
            let body = Json(serde_json::json!({
                "job_id": id,
                "status": "queued",
                "kind": label,
                "status_url": format!("/api/v1/jobs/{}", id),
                "result_url": format!("/api/v1/jobs/{}/result", id),
            }));
            Ok((StatusCode::ACCEPTED, body).into_response())
        }
        // Backpressure: the queue or the store is full. 503 with Retry-After is
        // the correct signal — the work was not accepted; the client should
        // retry shortly. (Distinct from 413/resource-limit, which means the
        // input itself is too large and retrying won't help.)
        SubmitOutcome::QueueFull => {
            Ok(service_unavailable("The job queue is full; retry shortly."))
        }
        SubmitOutcome::StoreFull => Ok(service_unavailable(
            "The server is retaining the maximum number of jobs; retry shortly.",
        )),
    }
}

fn service_unavailable(message: &str) -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        [("retry-after", "10")],
        Json(serde_json::json!({
            "error": "queue_full",
            "message": message,
        })),
    )
        .into_response()
}

/// GET /api/v1/jobs/{id} — poll a job's status and progress.
pub async fn status(
    State(jobs): State<JobsState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let owner = caller_identity(&headers);
    let Some(snap) = jobs.system.store.get(&id) else {
        return not_found();
    };
    // Ownership scoping: a job that exists but belongs to someone else is 404,
    // not 403 — never confirm another caller's job exists.
    if snap.owner != owner {
        return not_found();
    }

    let (done, total) = snap.progress;
    let mut body = serde_json::json!({
        "job_id": snap.id,
        "kind": snap.kind_label,
        "status": snap.status.as_str(),
    });
    // Progress is meaningful once we know the total (set when work begins).
    if total > 0 {
        body["progress"] = serde_json::json!({
            "done": done,
            "total": total,
        });
    }
    match snap.status {
        JobStatus::Completed => {
            body["result_url"] = serde_json::json!(format!("/api/v1/jobs/{}/result", snap.id));
        }
        JobStatus::Failed => {
            if let Some(err) = &snap.error {
                body["error"] = serde_json::json!(err.code);
                body["message"] = serde_json::json!(err.message);
                if let Some(reference) = &err.reference {
                    body["reference"] = serde_json::json!(reference);
                }
            }
        }
        _ => {}
    }
    (StatusCode::OK, Json(body)).into_response()
}

/// GET /api/v1/jobs/{id}/result — download a completed job's output.
pub async fn result(
    State(jobs): State<JobsState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let owner = caller_identity(&headers);
    let Some(snap) = jobs.system.store.get(&id) else {
        return not_found();
    };
    if snap.owner != owner {
        return not_found();
    }

    match snap.status {
        JobStatus::Completed => {
            let Some(result) = snap.result else {
                // Completed but result metadata missing — treat as gone.
                return not_found();
            };
            // Read the bounded result file (its size was capped by
            // max_output_bytes during processing) and replay the stored
            // response metadata. We keep the file until TTL expiry so the
            // client may re-download within the retention window.
            match std::fs::read(&result.path) {
                Ok(bytes) => {
                    let mut response_headers = HeaderMap::new();
                    response_headers.insert(
                        header::CONTENT_TYPE,
                        HeaderValue::from_static(result.content_type),
                    );
                    if let Ok(disposition) = HeaderValue::from_str(&format!(
                        "attachment; filename=\"{}\"",
                        result.filename
                    )) {
                        response_headers.insert(header::CONTENT_DISPOSITION, disposition);
                    }
                    for (name, value) in &result.extra_headers {
                        if let (Ok(n), Ok(v)) = (
                            axum::http::HeaderName::from_bytes(name.as_bytes()),
                            HeaderValue::from_str(value),
                        ) {
                            response_headers.insert(n, v);
                        }
                    }
                    (StatusCode::OK, response_headers, bytes).into_response()
                }
                Err(_) => {
                    // File gone (reaped/cleaned) though state said completed:
                    // surface as expired/not-found rather than a 500.
                    not_found()
                }
            }
        }
        JobStatus::Failed => {
            // Replay the classified failure with its proper status.
            if let Some(err) = &snap.error {
                let status = match err.code {
                    "queue_full" | "timeout" => StatusCode::SERVICE_UNAVAILABLE,
                    "resource_limit" => StatusCode::PAYLOAD_TOO_LARGE,
                    "internal_error" => StatusCode::INTERNAL_SERVER_ERROR,
                    "invalid_parameter" | "missing_file" => StatusCode::BAD_REQUEST,
                    _ => StatusCode::UNPROCESSABLE_ENTITY,
                };
                let mut body = serde_json::json!({
                    "error": err.code,
                    "message": err.message,
                });
                if let Some(reference) = &err.reference {
                    body["reference"] = serde_json::json!(reference);
                }
                (status, Json(body)).into_response()
            } else {
                not_found()
            }
        }
        JobStatus::Queued | JobStatus::Running => {
            // Not ready yet: 409 Conflict with the current status, so a polling
            // client knows to keep waiting rather than treating it as an error.
            (
                StatusCode::CONFLICT,
                Json(serde_json::json!({
                    "error": "not_ready",
                    "message": "The job has not completed yet; poll the status endpoint.",
                    "status": snap.status.as_str(),
                })),
            )
                .into_response()
        }
        JobStatus::Expired => not_found(),
    }
}

/// A uniform 404 for unknown ids, expired jobs, and jobs owned by another
/// caller — so the three cases are externally indistinguishable.
fn not_found() -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(serde_json::json!({
            "error": "job_not_found",
            "message": "No such job, or it has expired.",
        })),
    )
        .into_response()
}
