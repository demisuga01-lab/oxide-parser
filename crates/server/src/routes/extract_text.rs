use axum::extract::Multipart;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use oxide_engine::{
    ContentEngine, PdfAnalyzer, TextExtractOptions, TextExtractor, TextFormatOptions, TextFormatter,
};

use crate::error::{ServerError, ServerResult};
use crate::params::{parse_bool_param, parse_page_range};

async fn extract_multipart_fields(
    mut multipart: Multipart,
) -> ServerResult<(
    Bytes,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
)> {
    let mut file_bytes: Option<Bytes> = None;
    let mut pages_str: Option<String> = None;
    let mut page_markers_str: Option<String> = None;
    let mut preserve_layout_str: Option<String> = None;
    let mut output_format_str: Option<String> = None;
    let mut password_str: Option<String> = None;

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| ServerError::InvalidParameter(format!("multipart error: {}", e)))?
    {
        let name = field.name().map(str::to_owned);
        match name.as_deref() {
            Some("file") => {
                file_bytes = Some(field.bytes().await.map_err(|e| {
                    ServerError::InvalidParameter(format!("failed to read file field: {}", e))
                })?);
            }
            Some("pages") => {
                pages_str = Some(field.text().await.map_err(|e| {
                    ServerError::InvalidParameter(format!("failed to read pages field: {}", e))
                })?);
            }
            Some("page_markers") => {
                page_markers_str = Some(field.text().await.map_err(|e| {
                    ServerError::InvalidParameter(format!(
                        "failed to read page_markers field: {}",
                        e
                    ))
                })?);
            }
            Some("preserve_layout") => {
                preserve_layout_str = Some(field.text().await.map_err(|e| {
                    ServerError::InvalidParameter(format!(
                        "failed to read preserve_layout field: {}",
                        e
                    ))
                })?);
            }
            Some("output_format") => {
                output_format_str = Some(field.text().await.map_err(|e| {
                    ServerError::InvalidParameter(format!(
                        "failed to read output_format field: {}",
                        e
                    ))
                })?);
            }
            Some("password") => {
                password_str = Some(field.text().await.map_err(|e| {
                    ServerError::InvalidParameter(format!("failed to read password field: {}", e))
                })?);
            }
            Some(unknown) => {
                let _ = field.bytes().await;
                tracing::debug!("extract-text: ignoring unknown field '{}'", unknown);
            }
            None => {
                let _ = field.bytes().await;
            }
        }
    }

    let file = file_bytes.ok_or(ServerError::MissingFile)?;
    let max_size = crate::config::get_config().max_file_size;
    if file.len() > max_size {
        return Err(ServerError::InvalidParameter(format!(
            "file too large: {} bytes (max {} bytes = {} MB)",
            file.len(),
            max_size,
            max_size / (1024 * 1024)
        )));
    }
    Ok((
        file,
        pages_str,
        page_markers_str,
        preserve_layout_str,
        output_format_str,
        password_str,
    ))
}

pub async fn handler(multipart: Multipart) -> ServerResult<Response> {
    let (
        file_bytes,
        pages_str,
        page_markers_str,
        preserve_layout_str,
        output_format_str,
        password_str,
    ) = extract_multipart_fields(multipart).await?;

    tracing::info!(
        pdf_size = file_bytes.len(),
        pages = %pages_str.as_deref().unwrap_or("all"),
        "processing extract-text request"
    );

    let output_format = match output_format_str.as_deref().map(str::trim) {
        None | Some("txt") => "txt",
        Some("json") => "json",
        Some(other) => {
            return Err(ServerError::InvalidParameter(format!(
                "output_format must be 'txt' or 'json', got '{}'",
                other
            )))
        }
    };

    let include_page_markers = parse_bool_param(page_markers_str.as_deref(), true)?;
    let preserve_layout = parse_bool_param(preserve_layout_str.as_deref(), false)?;

    let engine = ContentEngine::open_bytes_with_password(
        file_bytes.to_vec(),
        password_str.as_deref().unwrap_or("").as_bytes(),
    )
    .map_err(ServerError::from)?;
    let total_pages = engine.page_count().map_err(ServerError::from)?;

    let pages = parse_page_range(pages_str.as_deref(), total_pages)
        .map_err(ServerError::InvalidParameter)?;

    let analysis = PdfAnalyzer::quick_analysis(&engine).map_err(ServerError::from)?;

    if output_format == "json" {
        let extractor = TextExtractor::new();
        let mut page_results: Vec<serde_json::Value> = Vec::new();
        let mut total_chars = 0usize;

        let mut opts = TextExtractOptions::default();
        opts.format.include_page_markers = false;
        opts.format.preserve_layout = preserve_layout;

        for page_num in &pages {
            match extractor.extract_page(&engine, *page_num, &opts) {
                Ok((n, lines)) => {
                    let formatter = TextFormatter::new();
                    let fmt_opts = TextFormatOptions {
                        include_page_markers: false,
                        preserve_layout,
                        ..Default::default()
                    };
                    let text = formatter.format_page(&lines, n, &fmt_opts);
                    let char_count = text.chars().filter(|c| !c.is_whitespace()).count();
                    let line_count = lines.len();
                    total_chars += char_count;
                    page_results.push(serde_json::json!({
                        "page": n,
                        "text": text,
                        "line_count": line_count,
                        "char_count": char_count,
                    }));
                }
                Err(e) => {
                    tracing::warn!("extract-text JSON: page {} failed: {}", page_num, e);
                    page_results.push(serde_json::json!({
                        "page": page_num,
                        "text": "",
                        "line_count": 0,
                        "char_count": 0,
                        "error": e.to_string(),
                    }));
                }
            }
        }

        tracing::info!(
            page_count = total_pages,
            has_text = analysis.has_text_layer,
            "extract-text complete"
        );

        let response_body = serde_json::json!({
            "pages": page_results,
            "total_pages": total_pages,
            "total_chars": total_chars,
            "has_text_layer": analysis.has_text_layer,
            "is_likely_scanned": analysis.is_likely_scanned,
            "recommendation": analysis.recommendation,
        });

        Ok((StatusCode::OK, axum::Json(response_body)).into_response())
    } else {
        let opts = TextExtractOptions {
            pages: Some(pages),
            format: TextFormatOptions {
                include_page_markers,
                preserve_layout,
                ..Default::default()
            },
            ..Default::default()
        };

        let text = TextExtractor::new()
            .extract(&engine, &opts)
            .map_err(ServerError::from)?;

        if analysis.is_likely_scanned && analysis.total_char_count == 0 {
            return Err(ServerError::NoTextLayer);
        }

        tracing::info!(
            page_count = total_pages,
            has_text = analysis.has_text_layer,
            "extract-text complete"
        );

        Ok((
            StatusCode::OK,
            [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            text,
        )
            .into_response())
    }
}
