use axum::{
    middleware,
    routing::{get, post},
    Router,
};
use tower_http::cors::CorsLayer;
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::trace::TraceLayer;

use crate::config::{get_config, ServerConfig};
use crate::routes;

// -- Oxide HTTP API v1 -----------------------------------------------------
//
// POST /api/v1/extract-text
//   Extract plain text from a PDF.
//   Fields: file, pages, page_markers, preserve_layout, output_format.
//
// POST /api/v1/extract-images
//   Extract embedded images from a PDF as a ZIP archive.
//   Fields: file, pages, format, quality, min_width, min_height,
//           include_masks, include_inline, output_format.
//
// POST /api/v1/analyze
//   Analyze whether a PDF has a real text layer.
//   Fields: file.
//
// POST /api/v1/pdf2img
//   Render PDF pages to PNG or JPEG images as a ZIP archive.
//   Fields: file, pages, dpi (24-600), format (png/jpg), quality.
//
// GET /api/v1/version
//   Returns server and engine version info.
//
// GET /health
//   Docker/k8s health check.
//
// GET /readiness
//   Kubernetes readiness probe.
// ---------------------------------------------------------------------------
pub fn create_app() -> Router {
    create_app_with_config(get_config().clone())
}

pub fn create_app_with_config(config: ServerConfig) -> Router {
    Router::new()
        .route("/health", get(routes::health::health))
        .route("/readiness", get(routes::health::readiness))
        .route("/api/v1/health", get(routes::health::health))
        .route("/api/v1/version", get(routes::health::version))
        .route("/api/v1/readiness", get(routes::health::readiness))
        .route("/api/v1/extract-text", post(routes::extract_text::handler))
        .route(
            "/api/v1/extract-images",
            post(routes::extract_images::handler),
        )
        .route("/api/v1/analyze", post(routes::analyze::handler))
        .route("/api/v1/pdf2img", post(routes::pdf2img::handler))
        // TODO(async-jobs): add background job support for very large PDFs if synchronous latency becomes unacceptable.
        .layer(TraceLayer::new_for_http())
        .layer(RequestBodyLimitLayer::new(config.max_file_size))
        .layer(CorsLayer::permissive())
        .layer(middleware::from_fn(
            crate::rate_limit::rate_limit_middleware,
        ))
        .layer(middleware::from_fn(crate::auth::auth_middleware))
}
