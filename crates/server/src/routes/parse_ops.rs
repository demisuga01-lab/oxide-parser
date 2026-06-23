//! Parser endpoints: the canonical document-model surface over HTTP.
//!
//! - `POST /api/v1/parse`          → parse to the canonical [`Document`] model,
//!   serialized as Markdown / JSON / HTML.
//! - `POST /api/v1/chunk`          → RAG-ready semantic chunks (JSON).
//! - `POST /api/v1/extract-fields` → structured key-value fields (JSON).
//! - `POST /api/v1/info`           → document metadata (pdfinfo-style, JSON).
//!
//! These mirror the CLI (`oxide parse`/`chunk`/`extract-fields`/`info`) and the
//! C-ABI / WASM parser bindings, and emit the **same** canonical schema so a
//! consumer gets identical structure regardless of surface. Each handler runs
//! the CPU-bound engine work on the blocking pool under the configured
//! cooperative deadline ([`run_with_timeout`]) and re-checks the file-size cap,
//! so untrusted input stays crash/hang/OOM-safe behind the existing
//! auth / rate-limit / body-limit middleware.
//!
//! OCR is **not** engaged server-side (no Tesseract dependency in this crate):
//! a scanned PDF parses to the digital-born result with scanned pages degraded
//! to placeholders. Clients can detect this via `POST /api/v1/analyze`
//! (`is_likely_scanned`) and run OCR out of band.

use axum::extract::Multipart;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use bytes::Bytes;
use oxide_engine::{
    ChunkOptions, ContentEngine, DocType, ExtractOptions, ParseOptions, SerializeOptions,
};

use crate::error::{ServerError, ServerResult};
use crate::params::parse_page_range;
use crate::processing::run_with_timeout;

/// Fields shared by the parser endpoints. Not every endpoint reads every field
/// (e.g. `format` is parse-only, `doc_type` is extract-fields-only); unknown
/// fields are ignored, matching the other handlers.
#[derive(Default)]
struct ParserFields {
    file: Option<Bytes>,
    pages: Option<String>,
    format: Option<String>,
    password: Option<String>,
    doc_type: Option<String>,
    target_tokens: Option<String>,
    overlap: Option<String>,
    keep_furniture: Option<String>,
}

async fn read_fields(mut multipart: Multipart) -> ServerResult<ParserFields> {
    let mut f = ParserFields::default();
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| ServerError::InvalidParameter(format!("multipart error: {}", e)))?
    {
        let name = field.name().map(str::to_owned);
        macro_rules! text {
            ($label:literal) => {
                Some(field.text().await.map_err(|e| {
                    ServerError::InvalidParameter(format!("failed to read {} field: {}", $label, e))
                })?)
            };
        }
        match name.as_deref() {
            Some("file") => {
                f.file = Some(field.bytes().await.map_err(|e| {
                    ServerError::InvalidParameter(format!("failed to read file field: {}", e))
                })?);
            }
            Some("pages") => f.pages = text!("pages"),
            Some("format") => f.format = text!("format"),
            Some("password") => f.password = text!("password"),
            Some("doc_type") | Some("type") => f.doc_type = text!("doc_type"),
            Some("target_tokens") => f.target_tokens = text!("target_tokens"),
            Some("overlap") => f.overlap = text!("overlap"),
            Some("keep_furniture") => f.keep_furniture = text!("keep_furniture"),
            Some(unknown) => {
                let _ = field.bytes().await;
                tracing::debug!("parser endpoint: ignoring unknown field '{}'", unknown);
            }
            None => {
                let _ = field.bytes().await;
            }
        }
    }
    Ok(f)
}

/// Validate the file field against the size cap (the same recheck every handler
/// does on top of the tower body limit) and return the bytes.
fn take_file(f: &mut ParserFields) -> ServerResult<Bytes> {
    let file = f.file.take().ok_or(ServerError::MissingFile)?;
    let max_size = crate::config::get_config().max_file_size;
    if file.len() > max_size {
        return Err(ServerError::InvalidParameter(format!(
            "file too large: {} bytes (max {} bytes = {} MB)",
            file.len(),
            max_size,
            max_size / (1024 * 1024)
        )));
    }
    Ok(file)
}

/// Open the document and resolve the requested 1-based page selection against
/// the document length, enforcing the page-count cap.
fn open_and_pages(
    file: &Bytes,
    password: Option<&str>,
    pages: Option<&str>,
) -> ServerResult<(ContentEngine, Vec<usize>)> {
    let engine =
        ContentEngine::open_bytes_with_password(file.to_vec(), password.unwrap_or("").as_bytes())
            .map_err(ServerError::from)?;
    let total_pages = engine.page_count().map_err(ServerError::from)?;
    let selected = parse_page_range(pages, total_pages).map_err(ServerError::InvalidParameter)?;

    let max_pages = crate::config::get_config().max_pages;
    if selected.len() > max_pages {
        return Err(ServerError::ResourceLimit(format!(
            "request selects {} pages, exceeding the limit of {}",
            selected.len(),
            max_pages
        )));
    }
    Ok((engine, selected))
}

/// `POST /api/v1/parse` — canonical document model as Markdown / JSON / HTML.
pub async fn parse(multipart: Multipart) -> ServerResult<Response> {
    let mut fields = read_fields(multipart).await?;
    let file = take_file(&mut fields)?;

    let format = match fields.format.as_deref().map(str::trim) {
        None | Some("") | Some("markdown") | Some("md") => "markdown",
        Some("json") => "json",
        Some("html") => "html",
        Some(other) => {
            return Err(ServerError::InvalidParameter(format!(
                "format must be 'markdown', 'json', or 'html', got '{}'",
                other
            )))
        }
    };

    let (engine, pages) =
        open_and_pages(&file, fields.password.as_deref(), fields.pages.as_deref())?;
    tracing::info!(
        pdf_size = file.len(),
        pages = pages.len(),
        format,
        "processing parse request"
    );

    let config = crate::config::get_config();
    let format_owned = format.to_string();
    let (content_type, body) = run_with_timeout(config, move |_cancel| {
        let opts = ParseOptions {
            pages,
            ..Default::default()
        };
        let doc = engine.parse_document(&opts)?;
        let out = match format_owned.as_str() {
            "json" => ("application/json", doc.to_json()),
            "html" => (
                "text/html; charset=utf-8",
                doc.to_html(&SerializeOptions::default()),
            ),
            _ => (
                "text/markdown; charset=utf-8",
                doc.to_markdown(&SerializeOptions::default()),
            ),
        };
        Ok::<(&'static str, String), ServerError>(out)
    })
    .await??;

    Ok((
        StatusCode::OK,
        [(axum::http::header::CONTENT_TYPE, content_type)],
        body,
    )
        .into_response())
}

/// `POST /api/v1/chunk` — RAG-ready semantic chunks as JSON.
pub async fn chunk(multipart: Multipart) -> ServerResult<Response> {
    let mut fields = read_fields(multipart).await?;
    let file = take_file(&mut fields)?;

    let target_tokens = parse_usize(fields.target_tokens.as_deref(), "target_tokens")?;
    let overlap = parse_usize(fields.overlap.as_deref(), "overlap")?;
    let keep_furniture = crate::params::parse_bool_param(fields.keep_furniture.as_deref(), false)?;

    let (engine, pages) =
        open_and_pages(&file, fields.password.as_deref(), fields.pages.as_deref())?;
    tracing::info!(
        pdf_size = file.len(),
        pages = pages.len(),
        "processing chunk request"
    );

    let config = crate::config::get_config();
    let body = run_with_timeout(config, move |_cancel| {
        let opts = ParseOptions {
            pages,
            ..Default::default()
        };
        let doc = engine.parse_document(&opts)?;
        let mut chunk_opts = ChunkOptions {
            include_furniture: keep_furniture,
            ..Default::default()
        };
        if let Some(t) = target_tokens {
            chunk_opts.target_tokens = t;
        }
        if let Some(o) = overlap {
            chunk_opts.overlap_tokens = o;
        }
        Ok::<String, ServerError>(doc.chunk(&chunk_opts).to_json())
    })
    .await??;

    Ok((
        StatusCode::OK,
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        body,
    )
        .into_response())
}

/// `POST /api/v1/extract-fields` — structured key-value fields as JSON.
pub async fn extract_fields(multipart: Multipart) -> ServerResult<Response> {
    let mut fields = read_fields(multipart).await?;
    let file = take_file(&mut fields)?;

    // doc_type: null/empty/"auto"/unknown → auto-detect (matches CLI/C/WASM).
    let doc_type = fields
        .doc_type
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty() && *s != "auto")
        .and_then(DocType::parse);

    let (engine, pages) =
        open_and_pages(&file, fields.password.as_deref(), fields.pages.as_deref())?;
    tracing::info!(
        pdf_size = file.len(),
        pages = pages.len(),
        "processing extract-fields request"
    );

    let config = crate::config::get_config();
    let body = run_with_timeout(config, move |_cancel| {
        let opts = ExtractOptions {
            doc_type,
            pages,
            ..Default::default()
        };
        let result = engine.extract_fields(&opts)?;
        Ok::<String, ServerError>(result.to_json())
    })
    .await??;

    Ok((
        StatusCode::OK,
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        body,
    )
        .into_response())
}

/// `POST /api/v1/info` — document metadata (pdfinfo-style) as JSON.
pub async fn info(multipart: Multipart) -> ServerResult<Response> {
    let mut fields = read_fields(multipart).await?;
    let file = take_file(&mut fields)?;

    let engine = ContentEngine::open_bytes_with_password(
        file.to_vec(),
        fields.password.as_deref().unwrap_or("").as_bytes(),
    )
    .map_err(ServerError::from)?;
    tracing::info!(pdf_size = file.len(), "processing info request");

    let config = crate::config::get_config();
    let value = run_with_timeout(config, move |_cancel| {
        let info = engine.document_info()?;
        serde_json::to_value(&info)
            .map_err(|e| ServerError::Internal(format!("serialize document info: {}", e)))
    })
    .await??;

    Ok((StatusCode::OK, Json(value)).into_response())
}

fn parse_usize(s: Option<&str>, label: &str) -> ServerResult<Option<usize>> {
    match s.map(str::trim).filter(|s| !s.is_empty()) {
        None => Ok(None),
        Some(v) => v.parse::<usize>().map(Some).map_err(|_| {
            ServerError::InvalidParameter(format!(
                "{} must be a non-negative integer, got '{}'",
                label, v
            ))
        }),
    }
}
