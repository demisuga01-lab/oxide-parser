use std::io::{Cursor, Write};
use std::sync::Arc;

use axum::{
    body::Bytes,
    extract::Multipart,
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};
use oxide_engine::{ContentEngine, ImageEncoder, ImageOutputFormat};
use rayon::prelude::*;
use zip::{write::FileOptions, CompressionMethod, ZipWriter};

use crate::{
    error::{ServerError, ServerResult},
    params::parse_page_range,
    processing::ProcessedOutput,
};

pub(crate) struct Pdf2ImgParams {
    pub file: Bytes,
    pub pages_str: Option<String>,
    pub dpi: u32,
    pub format: ImageOutputFormat,
    pub quality: u8,
    pub password: Option<String>,
}

pub async fn handler(multipart: Multipart) -> ServerResult<Response> {
    let params = extract_pdf2img_fields(multipart).await?;
    let config = crate::config::get_config();
    let output = process_pdf2img(params, config, config.request_timeout_secs, None).await?;
    Ok(output_to_response(output))
}

/// Turn a [`ProcessedOutput`] into an HTTP response (sync path and the job
/// result endpoint share this).
pub(crate) fn output_to_response(output: ProcessedOutput) -> Response {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(output.content_type),
    );
    if let Ok(disposition) =
        HeaderValue::from_str(&format!("attachment; filename=\"{}\"", output.filename))
    {
        headers.insert(header::CONTENT_DISPOSITION, disposition);
    }
    for (name, value) in &output.extra_headers {
        if let Ok(v) = HeaderValue::from_str(value) {
            if let Ok(n) = axum::http::HeaderName::from_bytes(name.as_bytes()) {
                headers.insert(n, v);
            }
        }
    }
    (StatusCode::OK, headers, output.bytes).into_response()
}

/// Core pdf2img processing shared by the synchronous handler and the async job
/// worker. `timeout_secs` is the cooperative deadline (the sync request timeout
/// for the sync path, the larger job timeout for jobs). Output is byte-for-byte
/// identical regardless of caller.
pub(crate) async fn process_pdf2img(
    params: Pdf2ImgParams,
    config: &crate::config::ServerConfig,
    timeout_secs: u64,
    progress: Option<Arc<crate::processing::JobProgress>>,
) -> ServerResult<ProcessedOutput> {
    let pdf_bytes = params.file.clone();
    let password = params.password.clone().unwrap_or_default();

    let probe = ContentEngine::open_bytes_with_password(pdf_bytes.to_vec(), password.as_bytes())
        .map_err(ServerError::from)?;
    let page_count = probe.page_count().map_err(ServerError::from)?;
    let page_nums = parse_page_range(params.pages_str.as_deref(), page_count)
        .map_err(ServerError::InvalidParameter)?;
    let max_pages = config.max_pages;
    if page_nums.len() > max_pages {
        return Err(ServerError::InvalidParameter(format!(
            "too many pages requested: {} (max {})",
            page_nums.len(),
            max_pages
        )));
    }

    if page_nums.len() > 50 {
        tracing::warn!(
            page_count = page_nums.len(),
            dpi = params.dpi,
            "pdf2img: large render request"
        );
    }

    let dpi = params.dpi;
    let format = params.format.clone();
    let quality = params.quality;
    let page_nums_for_render = page_nums.clone();

    // Pixel-explosion guard: reject any page whose MediaBox * DPI would exceed
    // the render-pixel cap BEFORE we allocate a single page buffer. A giant
    // MediaBox at a legal DPI can otherwise demand gigabytes per page. We check
    // every requested page's viewport up front so the rejection costs nothing
    // beyond reading page geometry.
    for page_num in &page_nums {
        let viewport = probe.page_viewport(*page_num, dpi).map_err(ServerError::from)?;
        crate::processing::check_render_pixels(
            config,
            *page_num,
            viewport.width_px,
            viewport.height_px,
        )?;
    }

    // Share ONE parsed engine across all render threads via Arc instead of
    // re-opening (and thus re-parsing + holding another full copy of) the PDF
    // for every page. The engine is `Send + Sync` (its only interior
    // mutability, the object-stream cache, is an `RwLock`), so each rayon
    // thread holds a cheap `Arc` clone — a pointer bump, not a deep copy. This
    // turns per-page O(file_size) memory into O(1) regardless of page count.
    // We reuse the `probe` engine that was already parsed for the page count.
    let engine = Arc::new(probe);

    let render_engine = Arc::clone(&engine);
    let max_output_bytes = config.max_output_bytes;
    if let Some(p) = &progress {
        p.set_total(page_nums_for_render.len());
    }
    let progress_for_render = progress.clone();
    // Render under a cooperative deadline: every rayon worker shares the one
    // CancelToken, so when the timer trips they all observe it and bail,
    // freeing the threads (a tower timeout alone would leak the runaway work).
    let render_result: Result<Vec<(usize, Vec<u8>)>, ServerError> =
        crate::processing::run_with_deadline_secs(timeout_secs, move |cancel| {
            let pages: Vec<(usize, Vec<u8>)> = page_nums_for_render
                .par_iter()
                .filter_map(|page_num| {
                    let buf = match render_engine.render_page_cancellable(*page_num, dpi, &cancel) {
                        Ok(buf) => buf,
                        Err(err) => {
                            tracing::warn!(page = *page_num, error = %err, "pdf2img: render failed");
                            return None;
                        }
                    };
                    let raw = buf.to_raw_image();
                    let encoded = match format {
                        ImageOutputFormat::Jpeg => ImageEncoder::encode_jpeg(&raw, quality),
                        ImageOutputFormat::Webp => ImageEncoder::encode_webp(&raw, quality),
                        ImageOutputFormat::Png | ImageOutputFormat::Original => {
                            ImageEncoder::encode_png_fast(&raw)
                        }
                    };
                    match encoded {
                        Ok(bytes) => {
                            // Bump the per-job progress counter as each page
                            // finishes (no-op for the sync path, which passes
                            // None). Relaxed atomic — readers want a live-ish
                            // count, not a synchronized one.
                            if let Some(p) = &progress_for_render {
                                p.inc();
                            }
                            Some((*page_num, bytes))
                        }
                        Err(err) => {
                            tracing::warn!(page = *page_num, error = %err, "pdf2img: encode failed");
                            None
                        }
                    }
                })
                .collect();
            // If the deadline tripped, workers bailed mid-flight and `pages` is
            // partial/empty — report a clean timeout instead of a truncated ZIP.
            if cancel.is_cancelled() {
                Err(ServerError::Timeout)
            } else {
                Ok(pages)
            }
        })
        .await?;

    let mut rendered = render_result?;
    rendered.sort_by_key(|(page_num, _)| *page_num);
    let pages_rendered = rendered.len();
    let ext = params.format.file_extension();
    let zip_bytes = build_pdf2img_zip(&rendered, ext, max_output_bytes)?;

    Ok(ProcessedOutput {
        bytes: zip_bytes,
        content_type: "application/zip",
        filename: "pages.zip",
        extra_headers: vec![
            ("x-page-count", page_count.to_string()),
            ("x-pages-rendered", pages_rendered.to_string()),
            ("x-dpi", dpi.to_string()),
        ],
    })
}

pub(crate) async fn extract_pdf2img_fields(mut multipart: Multipart) -> ServerResult<Pdf2ImgParams> {
    let mut file_bytes: Option<Bytes> = None;
    let mut pages_str: Option<String> = None;
    let mut dpi_str: Option<String> = None;
    let mut format_str: Option<String> = None;
    let mut quality_str: Option<String> = None;
    let mut password_str: Option<String> = None;

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|err| ServerError::InvalidParameter(format!("multipart error: {}", err)))?
    {
        let name = field.name().map(str::to_owned);
        match name.as_deref() {
            Some("file") => {
                file_bytes = Some(
                    field
                        .bytes()
                        .await
                        .map_err(|err| ServerError::InvalidParameter(format!("{}", err)))?,
                );
            }
            Some("pages") => pages_str = Some(read_text_field(field).await?),
            Some("dpi") => dpi_str = Some(read_text_field(field).await?),
            Some("format") => format_str = Some(read_text_field(field).await?),
            Some("quality") => quality_str = Some(read_text_field(field).await?),
            Some("password") => password_str = Some(read_text_field(field).await?),
            _ => {
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

    let dpi = parse_optional_u32(dpi_str.as_deref(), "dpi", 150)?;
    let max_dpi = crate::config::get_config().max_dpi;
    if dpi < 24 || dpi > max_dpi {
        return Err(ServerError::InvalidParameter(format!(
            "dpi must be between 24 and {}, got {}",
            max_dpi, dpi
        )));
    }

    let format = match format_str.as_deref().map(str::trim) {
        None | Some("") | Some("png") => ImageOutputFormat::Png,
        Some("jpg") | Some("jpeg") => ImageOutputFormat::Jpeg,
        Some("webp") => ImageOutputFormat::Webp,
        Some(other) => {
            return Err(ServerError::InvalidParameter(format!(
                "format must be 'png', 'jpg', or 'webp', got '{}'",
                other
            )))
        }
    };

    let quality = parse_optional_u8(quality_str.as_deref(), "quality", 85)?;
    if !(1..=100).contains(&quality) {
        return Err(ServerError::InvalidParameter(format!(
            "quality must be 1-100, got '{}'",
            quality
        )));
    }

    Ok(Pdf2ImgParams {
        file,
        pages_str,
        dpi,
        format,
        quality,
        password: password_str,
    })
}

async fn read_text_field(field: axum::extract::multipart::Field<'_>) -> ServerResult<String> {
    field
        .text()
        .await
        .map_err(|err| ServerError::InvalidParameter(format!("{}", err)))
}

fn parse_optional_u8(value: Option<&str>, field: &str, default: u8) -> ServerResult<u8> {
    match value.map(str::trim) {
        None | Some("") => Ok(default),
        Some(value) => value.parse::<u8>().map_err(|_| {
            ServerError::InvalidParameter(format!("{} must be 1-100, got '{}'", field, value))
        }),
    }
}

fn parse_optional_u32(value: Option<&str>, field: &str, default: u32) -> ServerResult<u32> {
    match value.map(str::trim) {
        None | Some("") => Ok(default),
        Some(value) => value.parse::<u32>().map_err(|_| {
            ServerError::InvalidParameter(format!(
                "{} must be an integer 24-600, got '{}'",
                field, value
            ))
        }),
    }
}

fn build_pdf2img_zip(
    pages: &[(usize, Vec<u8>)],
    ext: &str,
    max_output_bytes: u64,
) -> ServerResult<Vec<u8>> {
    let mut buf = Vec::new();
    {
        let cursor = Cursor::new(&mut buf);
        let mut writer = ZipWriter::new(cursor);
        let opts = FileOptions::<()>::default()
            .compression_method(CompressionMethod::Deflated)
            .compression_level(Some(6));

        let mut accumulated = 0usize;
        for (page_num, image_bytes) in pages {
            let filename = format!("page-{:03}.{}", page_num, ext);
            writer
                .start_file(&filename, opts)
                .map_err(|err| ServerError::Internal(format!("ZIP start_file error: {}", err)))?;
            writer
                .write_all(image_bytes)
                .map_err(|err| ServerError::Internal(format!("ZIP write error: {}", err)))?;
            // Bound output as it accumulates so a request that would build an
            // absurd ZIP errors at the cap rather than buffering it whole. We
            // sum pre-compression entry sizes, an upper bound on the final ZIP
            // (deflate only shrinks), so the check is conservative.
            accumulated = accumulated.saturating_add(image_bytes.len());
            crate::processing::check_output_size_limit(max_output_bytes, accumulated)?;
        }

        writer
            .finish()
            .map_err(|err| ServerError::Internal(format!("ZIP finish error: {}", err)))?;
    }
    Ok(buf)
}
