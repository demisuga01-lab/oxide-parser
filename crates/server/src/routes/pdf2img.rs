use std::io::{Cursor, Write};

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
};

struct Pdf2ImgParams {
    file: Bytes,
    pages_str: Option<String>,
    dpi: u32,
    format: ImageOutputFormat,
    quality: u8,
    password: Option<String>,
}

pub async fn handler(multipart: Multipart) -> ServerResult<Response> {
    let params = extract_pdf2img_fields(multipart).await?;
    let pdf_bytes = params.file.clone();
    let password = params.password.clone().unwrap_or_default();

    let probe = ContentEngine::open_bytes_with_password(pdf_bytes.to_vec(), password.as_bytes())
        .map_err(ServerError::from)?;
    let page_count = probe.page_count().map_err(ServerError::from)?;
    let page_nums = parse_page_range(params.pages_str.as_deref(), page_count)
        .map_err(ServerError::InvalidParameter)?;
    let max_pages = crate::config::get_config().max_pages;
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
    let pdf_vec = pdf_bytes.to_vec();
    let page_nums_for_render = page_nums.clone();
    let password_for_render = password.clone();

    let mut rendered: Vec<(usize, Vec<u8>)> = tokio::task::spawn_blocking(move || {
        page_nums_for_render
            .par_iter()
            .filter_map(|page_num| {
                // TODO(perf): implement thread-safe ContentEngine (Arc-based) to avoid
                // cloning PDF bytes for each page thread.
                let engine = match ContentEngine::open_bytes_with_password(
                    pdf_vec.clone(),
                    password_for_render.as_bytes(),
                ) {
                    Ok(engine) => engine,
                    Err(err) => {
                        tracing::warn!(page = *page_num, error = %err, "pdf2img: open failed");
                        return None;
                    }
                };
                let buf = match engine.render_page(*page_num, dpi) {
                    Ok(buf) => buf,
                    Err(err) => {
                        tracing::warn!(page = *page_num, error = %err, "pdf2img: render failed");
                        return None;
                    }
                };
                let raw = buf.to_raw_image();
                let encoded = match format {
                    ImageOutputFormat::Jpeg => ImageEncoder::encode_jpeg(&raw, quality),
                    ImageOutputFormat::Png | ImageOutputFormat::Original => {
                        ImageEncoder::encode_png_fast(&raw)
                    }
                };
                match encoded {
                    Ok(bytes) => Some((*page_num, bytes)),
                    Err(err) => {
                        tracing::warn!(page = *page_num, error = %err, "pdf2img: encode failed");
                        None
                    }
                }
            })
            .collect::<Vec<_>>()
    })
    .await
    .map_err(|err| ServerError::Internal(format!("render task failed: {}", err)))?;

    rendered.sort_by_key(|(page_num, _)| *page_num);
    let pages_rendered = rendered.len();
    let ext = params.format.file_extension();
    let zip_bytes = build_pdf2img_zip(&rendered, ext)
        .map_err(|err| ServerError::Internal(format!("ZIP build failed: {}", err)))?;

    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/zip"),
    );
    headers.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_static("attachment; filename=\"pages.zip\""),
    );
    insert_usize_header(&mut headers, "x-page-count", page_count)?;
    insert_usize_header(&mut headers, "x-pages-rendered", pages_rendered)?;
    insert_u32_header(&mut headers, "x-dpi", dpi)?;

    Ok((StatusCode::OK, headers, zip_bytes).into_response())
}

async fn extract_pdf2img_fields(mut multipart: Multipart) -> ServerResult<Pdf2ImgParams> {
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
        Some(other) => {
            return Err(ServerError::InvalidParameter(format!(
                "format must be 'png' or 'jpg', got '{}'",
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

fn build_pdf2img_zip(pages: &[(usize, Vec<u8>)], ext: &str) -> Result<Vec<u8>, String> {
    let mut buf = Vec::new();
    {
        let cursor = Cursor::new(&mut buf);
        let mut writer = ZipWriter::new(cursor);
        let opts = FileOptions::<()>::default()
            .compression_method(CompressionMethod::Deflated)
            .compression_level(Some(6));

        for (page_num, image_bytes) in pages {
            let filename = format!("page-{:03}.{}", page_num, ext);
            writer
                .start_file(&filename, opts)
                .map_err(|err| format!("ZIP start_file error: {}", err))?;
            writer
                .write_all(image_bytes)
                .map_err(|err| format!("ZIP write error: {}", err))?;
        }

        writer
            .finish()
            .map_err(|err| format!("ZIP finish error: {}", err))?;
    }
    Ok(buf)
}

fn insert_usize_header(
    headers: &mut HeaderMap,
    name: &'static str,
    value: usize,
) -> ServerResult<()> {
    let header_value = HeaderValue::from_str(&value.to_string()).map_err(|err| {
        ServerError::Internal(format!(
            "failed to build response header '{}': {}",
            name, err
        ))
    })?;
    headers.insert(name, header_value);
    Ok(())
}

fn insert_u32_header(headers: &mut HeaderMap, name: &'static str, value: u32) -> ServerResult<()> {
    let header_value = HeaderValue::from_str(&value.to_string()).map_err(|err| {
        ServerError::Internal(format!(
            "failed to build response header '{}': {}",
            name, err
        ))
    })?;
    headers.insert(name, header_value);
    Ok(())
}
