use std::io::{Cursor, Write};

use axum::{
    extract::Multipart,
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use bytes::Bytes;
use oxide_engine::{
    ContentEngine, ImageDecoder, ImageEncoder, ImageLocateOptions, ImageLocator, ImageOutputFormat,
    ImageReference, SmaskLoader,
};
use zip::{write::FileOptions, CompressionMethod, ZipWriter};

use crate::{
    error::{ServerError, ServerResult},
    params::{parse_bool_param, parse_page_range},
};

struct ExtractImagesParams {
    file: Bytes,
    pages_str: Option<String>,
    image_format: ImageOutputFormat,
    quality: u8,
    min_width: u32,
    min_height: u32,
    include_masks: bool,
    include_inline: bool,
    json_mode: bool,
    password: Option<String>,
}

pub async fn handler(headers: HeaderMap, multipart: Multipart) -> ServerResult<Response> {
    let accept_json = headers
        .get(header::ACCEPT)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.contains("application/json"))
        .unwrap_or(false);
    let params = extract_extract_images_fields(multipart, accept_json).await?;

    let engine = ContentEngine::open_bytes_with_password(
        params.file.to_vec(),
        params.password.as_deref().unwrap_or("").as_bytes(),
    )
    .map_err(ServerError::from)?;
    let total_pages = engine.page_count().map_err(ServerError::from)?;
    let requested_pages = parse_page_range(params.pages_str.as_deref(), total_pages)
        .map_err(ServerError::InvalidParameter)?;
    let pages_processed = requested_pages.len();

    let locate_opts = ImageLocateOptions {
        pages: Some(requested_pages),
        min_width: params.min_width,
        min_height: params.min_height,
        include_masks: params.include_masks,
        include_soft_masks: false,
        include_inline: params.include_inline,
    };

    let images = ImageLocator::find_all_images(&engine, &locate_opts).map_err(ServerError::from)?;
    let image_count = images.len();

    if params.json_mode {
        let metadata: Vec<serde_json::Value> = images
            .iter()
            .enumerate()
            .map(|(idx, img)| {
                let filename = format!(
                    "page-{:03}-image-{:03}.{}",
                    img.page_number,
                    idx + 1,
                    params.image_format.file_extension()
                );
                serde_json::json!({
                    "page": img.page_number,
                    "index": idx + 1,
                    "filename": filename,
                    "width": img.width,
                    "height": img.height,
                    "color_space": img.color_space,
                    "format": params.image_format.file_extension(),
                    "size_bytes": 0,
                    "is_mask": img.is_mask,
                    "has_alpha": false,
                })
            })
            .collect();

        let body = serde_json::json!({
            "image_count": image_count,
            "pages_processed": pages_processed,
            "images": metadata,
        });
        return Ok((StatusCode::OK, Json(body)).into_response());
    }

    let (zip_bytes, images_encoded) = build_zip(&engine, &images, &params)?;

    let mut response_headers = HeaderMap::new();
    response_headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/zip"),
    );
    response_headers.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_static("attachment; filename=\"images.zip\""),
    );
    insert_usize_header(&mut response_headers, "x-image-count", image_count)?;
    insert_usize_header(&mut response_headers, "x-images-encoded", images_encoded)?;
    insert_usize_header(&mut response_headers, "x-pages-processed", pages_processed)?;

    Ok((StatusCode::OK, response_headers, zip_bytes).into_response())
}

async fn extract_extract_images_fields(
    mut multipart: Multipart,
    accept_json: bool,
) -> ServerResult<ExtractImagesParams> {
    let mut file_bytes: Option<Bytes> = None;
    let mut pages_str: Option<String> = None;
    let mut format_str: Option<String> = None;
    let mut quality_str: Option<String> = None;
    let mut min_width_str: Option<String> = None;
    let mut min_height_str: Option<String> = None;
    let mut include_masks_str: Option<String> = None;
    let mut include_inline_str: Option<String> = None;
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
                file_bytes = Some(
                    field
                        .bytes()
                        .await
                        .map_err(|e| ServerError::InvalidParameter(format!("{}", e)))?,
                );
            }
            Some("pages") => pages_str = Some(read_text_field(field).await?),
            Some("format") => format_str = Some(read_text_field(field).await?),
            Some("quality") => quality_str = Some(read_text_field(field).await?),
            Some("min_width") => min_width_str = Some(read_text_field(field).await?),
            Some("min_height") => min_height_str = Some(read_text_field(field).await?),
            Some("include_masks") => include_masks_str = Some(read_text_field(field).await?),
            Some("include_inline") => include_inline_str = Some(read_text_field(field).await?),
            Some("output_format") => output_format_str = Some(read_text_field(field).await?),
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
    let format_trimmed = format_str.as_deref().map(str::trim);
    let image_format = match format_trimmed {
        None | Some("") | Some("original") => ImageOutputFormat::Original,
        Some("png") => ImageOutputFormat::Png,
        Some("jpg") | Some("jpeg") => ImageOutputFormat::Jpeg,
        Some("webp") => {
            return Err(ServerError::InvalidParameter(
                "webp output is not yet available; use png or jpg".to_string(),
            ))
        }
        Some(other) => {
            return Err(ServerError::InvalidParameter(format!(
                "unknown format '{}'; use png, jpg, or original",
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

    let min_width = parse_optional_u32(min_width_str.as_deref(), "min_width", 1)?;
    let min_height = parse_optional_u32(min_height_str.as_deref(), "min_height", 1)?;
    if min_width == 0 {
        return Err(ServerError::InvalidParameter(
            "min_width must be a positive integer".to_string(),
        ));
    }
    if min_height == 0 {
        return Err(ServerError::InvalidParameter(
            "min_height must be a positive integer".to_string(),
        ));
    }

    let include_masks = parse_bool_param(include_masks_str.as_deref(), false)?;
    let include_inline = parse_bool_param(include_inline_str.as_deref(), true)?;
    let json_mode =
        accept_json || matches!(output_format_str.as_deref().map(str::trim), Some("json"));

    Ok(ExtractImagesParams {
        file,
        pages_str,
        image_format,
        quality,
        min_width,
        min_height,
        include_masks,
        include_inline,
        json_mode,
        password: password_str,
    })
}

async fn read_text_field(field: axum::extract::multipart::Field<'_>) -> ServerResult<String> {
    field
        .text()
        .await
        .map_err(|e| ServerError::InvalidParameter(format!("{}", e)))
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
                "{} must be a positive integer, got '{}'",
                field, value
            ))
        }),
    }
}

fn insert_usize_header(
    headers: &mut HeaderMap,
    name: &'static str,
    value: usize,
) -> ServerResult<()> {
    let header_value = HeaderValue::from_str(&value.to_string()).map_err(|e| {
        ServerError::Internal(format!("failed to build response header '{}': {}", name, e))
    })?;
    headers.insert(name, header_value);
    Ok(())
}

fn build_zip(
    engine: &ContentEngine,
    images: &[ImageReference],
    params: &ExtractImagesParams,
) -> ServerResult<(Vec<u8>, usize)> {
    let mut buf: Vec<u8> = Vec::new();
    let mut images_encoded = 0usize;
    {
        let cursor = Cursor::new(&mut buf);
        let mut zw = ZipWriter::new(cursor);
        let zip_opts = FileOptions::<()>::default()
            .compression_method(CompressionMethod::Deflated)
            .compression_level(Some(6));

        // TODO(perf): parallelise image encoding using rayon, then write ZIP entries sequentially.
        for (idx, img_ref) in images.iter().enumerate() {
            // TODO(inline): inline image pixel bytes are not stored in ImageReference.
            // Re-parse the page content stream at encode time if inline image demand grows.
            let (image_bytes, ext) = match encode_image(engine, img_ref, params) {
                Ok(pair) => pair,
                Err(e) => {
                    tracing::warn!(
                        image = %img_ref.xobject_name,
                        page = img_ref.page_number,
                        error = %e,
                        "image failed to encode; skipping ZIP entry"
                    );
                    continue;
                }
            };

            let filename = format!(
                "page-{:03}-image-{:03}.{}",
                img_ref.page_number,
                idx + 1,
                ext
            );

            zw.start_file(&filename, zip_opts)
                .map_err(|e| ServerError::Internal(format!("ZIP start_file error: {}", e)))?;
            zw.write_all(&image_bytes)
                .map_err(|e| ServerError::Internal(format!("ZIP write error: {}", e)))?;
            images_encoded += 1;
        }

        zw.finish()
            .map_err(|e| ServerError::Internal(format!("ZIP finish error: {}", e)))?;
    }

    Ok((buf, images_encoded))
}

fn encode_image(
    engine: &ContentEngine,
    img_ref: &ImageReference,
    params: &ExtractImagesParams,
) -> oxide_engine::Result<(Vec<u8>, &'static str)> {
    if img_ref.is_inline {
        return Err(oxide_engine::OxideError::UnsupportedFeature(
            "inline image encoding in ZIP not yet implemented".to_string(),
        ));
    }

    if let Ok(Some((bytes, ext))) =
        ImageEncoder::keep_original(img_ref, engine.document().reader(), &params.image_format)
    {
        return Ok((bytes, ext));
    }

    let raw = ImageDecoder::decode(img_ref, engine.document().reader())?;
    let final_raw = if matches!(
        params.image_format,
        ImageOutputFormat::Png | ImageOutputFormat::Original
    ) {
        match SmaskLoader::load_and_combine(img_ref, raw.clone(), engine.document().reader())? {
            Some(rgba) => rgba,
            None => raw,
        }
    } else {
        raw
    };

    let ext = match params.image_format {
        ImageOutputFormat::Original => "png",
        _ => params.image_format.file_extension(),
    };
    let bytes = ImageEncoder::encode(&final_raw, &params.image_format, Some(params.quality))?;
    Ok((bytes, ext))
}
