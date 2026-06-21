use std::sync::atomic::{AtomicU64, Ordering};

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;

#[derive(Debug)]
pub enum ServerError {
    MissingFile,
    InvalidParameter(String),
    EncryptedDocument,
    EncryptedPdf(String),
    NoTextLayer,
    MalformedPdf(String),
    /// A document feature the engine does not yet support. The specific feature
    /// name is SAFE and USEFUL to surface to the client (it tells them what
    /// about their input we can't handle), so this maps to a 422, not a 500.
    Unsupported(String),
    /// Processing exceeded the per-request time budget (cooperative timeout).
    Timeout,
    /// The request asked for more work/output than a resource limit allows
    /// (pixel explosion, output-size explosion, too many images, etc.).
    ResourceLimit(String),
    /// A truly unexpected internal failure (a bug, an unhandled engine error,
    /// I/O failure, etc.). The inner string is for SERVER-SIDE LOGS ONLY and is
    /// never sent to the client — the 500 path returns a generic message plus a
    /// correlation id so operators can find the logged detail.
    Internal(String),
}

/// Monotonic counter feeding the correlation-id suffix. Combined with the
/// process start it yields a short id that is unique within a process run,
/// enough to tie a client-visible 500 to its server-side log line without
/// pulling in a uuid dependency.
static CORRELATION_COUNTER: AtomicU64 = AtomicU64::new(0);

fn next_correlation_id() -> String {
    let n = CORRELATION_COUNTER.fetch_add(1, Ordering::Relaxed);
    // Pad so ids sort/scan nicely in logs; "err-" prefix marks the namespace.
    format!("err-{:012x}", n)
}

impl From<oxide_engine::OxideError> for ServerError {
    fn from(e: oxide_engine::OxideError) -> Self {
        use oxide_engine::OxideError::*;
        match e {
            EncryptedDocument => ServerError::EncryptedDocument,
            EncryptedPdf(msg) => ServerError::EncryptedPdf(msg),
            MalformedPdf(msg) => ServerError::MalformedPdf(msg),
            ParseError(msg) => ServerError::MalformedPdf(msg),
            MissingObject { number, generation } => {
                ServerError::MalformedPdf(format!("missing object {} {}", number, generation))
            }
            Cancelled(_) => ServerError::Timeout,
            // Unsupported features are client-actionable: report the specific
            // feature with a 422 rather than burying it in a generic 500.
            UnsupportedFeature(msg) => ServerError::Unsupported(msg),
            // A page that exceeds the render-pixel cap (e.g. a hostile giant
            // MediaBox) is a resource-limit condition, surfaced as such rather
            // than a generic 500. The server's own check_render_pixels normally
            // catches this first; this maps the engine-level guard for the cases
            // a render path reaches the engine directly.
            ResourceLimit(msg) => ServerError::ResourceLimit(msg),
            // I/O is an unexpected internal condition; detail goes to logs only.
            Io(io_err) => ServerError::Internal(io_err.to_string()),
        }
    }
}

impl IntoResponse for ServerError {
    fn into_response(self) -> Response {
        let classified = self.classify();
        let mut body = json!({
            "error": classified.error_code,
            "message": classified.message,
        });
        if let Some(reference) = &classified.reference {
            body["reference"] = json!(reference);
        }
        (classified.status, Json(body)).into_response()
    }
}

/// The safe, client-facing parts of an error: a status code, a stable error
/// code, a message that leaks no internals, and (for the generic internal path)
/// a correlation reference id whose full detail was logged server-side.
///
/// This is the single classification point reused by both the synchronous
/// `IntoResponse` path and the async job worker, so a failed job records
/// exactly the same sanitized error a sync request would have returned.
#[derive(Debug, Clone)]
pub struct ClassifiedError {
    pub status: StatusCode,
    pub error_code: &'static str,
    pub message: String,
    pub reference: Option<String>,
}

impl ServerError {
    /// Classify this error into its safe, client-facing form. For the generic
    /// internal path this also LOGS the full detail server-side keyed by the
    /// correlation id (the same side effect the sync path had), so calling
    /// `classify()` once per error preserves debuggability without leakage.
    pub fn classify(self) -> ClassifiedError {
        // SAFE 4xx variants carry an intentionally informative, non-leaking
        // message. The Internal variant is the only one that must be sanitized.
        let (status, error_code, message) = match self {
            ServerError::MissingFile => (
                StatusCode::BAD_REQUEST,
                "missing_file",
                "Request must include a 'file' field with a PDF".to_string(),
            ),
            ServerError::InvalidParameter(msg) => {
                (StatusCode::BAD_REQUEST, "invalid_parameter", msg)
            }
            ServerError::EncryptedDocument => (
                StatusCode::UNPROCESSABLE_ENTITY,
                "encrypted",
                "The PDF is encrypted and cannot be processed".to_string(),
            ),
            ServerError::EncryptedPdf(msg) => (
                StatusCode::UNPROCESSABLE_ENTITY,
                "encrypted",
                if msg.is_empty() {
                    "The PDF is password-protected; provide the correct password".to_string()
                } else {
                    msg
                },
            ),
            ServerError::NoTextLayer => (
                StatusCode::UNPROCESSABLE_ENTITY,
                "no_text_layer",
                "The PDF has no extractable text layer. Consider OCR for scanned documents."
                    .to_string(),
            ),
            ServerError::MalformedPdf(msg) => {
                (StatusCode::UNPROCESSABLE_ENTITY, "malformed_pdf", msg)
            }
            ServerError::Unsupported(msg) => (
                StatusCode::UNPROCESSABLE_ENTITY,
                "unsupported_feature",
                if msg.is_empty() {
                    "The PDF uses a feature that is not yet supported".to_string()
                } else {
                    format!("Unsupported PDF feature: {}", msg)
                },
            ),
            ServerError::Timeout => (
                StatusCode::SERVICE_UNAVAILABLE,
                "timeout",
                "Request exceeded the processing time limit".to_string(),
            ),
            ServerError::ResourceLimit(msg) => {
                (StatusCode::PAYLOAD_TOO_LARGE, "resource_limit", msg)
            }
            ServerError::Internal(detail) => {
                // GENERIC internal path: never echo `detail`. Log it keyed by a
                // correlation id, return only the id to the client.
                let id = next_correlation_id();
                tracing::error!(
                    correlation_id = %id,
                    detail = %detail,
                    "internal server error (detail logged server-side only)"
                );
                return ClassifiedError {
                    status: StatusCode::INTERNAL_SERVER_ERROR,
                    error_code: "internal_error",
                    message: "An internal error occurred while processing the request.".to_string(),
                    reference: Some(id),
                };
            }
        };
        ClassifiedError {
            status,
            error_code,
            message,
            reference: None,
        }
    }
}

pub type ServerResult<T> = std::result::Result<T, ServerError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn correlation_ids_are_unique_and_namespaced() {
        let a = next_correlation_id();
        let b = next_correlation_id();
        assert_ne!(a, b);
        assert!(a.starts_with("err-"));
        assert!(b.starts_with("err-"));
    }

    #[test]
    fn unsupported_feature_maps_to_422_not_500() {
        let err: ServerError =
            oxide_engine::OxideError::UnsupportedFeature("JBIG2".to_string()).into();
        let resp = err.into_response();
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[test]
    fn internal_error_maps_to_500() {
        let resp = ServerError::Internal("secret path /etc/oxide/key".to_string()).into_response();
        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }
}
