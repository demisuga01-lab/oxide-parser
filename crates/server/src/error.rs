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
    Internal(String),
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
            UnsupportedFeature(msg) => ServerError::Internal(msg),
            Io(io_err) => ServerError::Internal(io_err.to_string()),
        }
    }
}

impl IntoResponse for ServerError {
    fn into_response(self) -> Response {
        let (status, error_code, message) = match &self {
            ServerError::MissingFile => (
                StatusCode::BAD_REQUEST,
                "missing_file",
                "Request must include a 'file' field with a PDF".to_string(),
            ),
            ServerError::InvalidParameter(msg) => {
                (StatusCode::BAD_REQUEST, "invalid_parameter", msg.clone())
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
                    msg.clone()
                },
            ),
            ServerError::NoTextLayer => (
                StatusCode::UNPROCESSABLE_ENTITY,
                "no_text_layer",
                "The PDF has no extractable text layer. Consider OCR for scanned documents."
                    .to_string(),
            ),
            ServerError::MalformedPdf(msg) => (
                StatusCode::UNPROCESSABLE_ENTITY,
                "malformed_pdf",
                msg.clone(),
            ),
            ServerError::Internal(msg) => {
                // TODO(errors): sanitize internal messages for non-debug production tiers.
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal_error",
                    msg.clone(),
                )
            }
        };
        let body = Json(json!({
            "error": error_code,
            "message": message,
        }));
        (status, body).into_response()
    }
}

pub type ServerResult<T> = std::result::Result<T, ServerError>;
