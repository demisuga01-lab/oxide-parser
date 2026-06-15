use std::io::{Cursor, Write};

use axum::{
    extract::Multipart,
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use bytes::Bytes;
use oxide_engine::{
    ContentEngine, ImageEncoder, ImageLocateOptions, ImageLocator, ImageOutputFormat,
    ImageReference, SmaskLoader,
};
use zip::{write::FileOptions, CompressionMethod, ZipWriter};

use crate::{
    error::{ServerError, ServerResult},
    params::{parse_bool_param, parse_page_range},
    processing::ProcessedOutput,
};

pub(crate) struct ExtractImagesParams {
    pub file: Bytes,
    pub pages_str: Option<String>,
    pub image_format: ImageOutputFormat,
    pub quality: u8,
    pub min_width: u32,
    pub min_height: u32,
    pub include_masks: bool,
    pub include_inline: bool,
    pub json_mode: bool,
    pub password: Option<String>,
}

pub async fn handler(headers: HeaderMap, multipart: Multipart) -> ServerResult<Response> {
    let accept_json = headers
        .get(header::ACCEPT)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.contains("application/json"))
        .unwrap_or(false);
    let params = extract_extract_images_fields(multipart, accept_json).await?;
    let config = crate::config::get_config();

    // JSON (metadata-only) mode is lightweight — it never decodes/encodes image
    // bytes — so it stays on the synchronous path even when invoked via a job
    // submit; the heavy ZIP path is what the async model exists for.
    if params.json_mode {
        let body = build_images_json(params, config)?;
        return Ok((StatusCode::OK, Json(body)).into_response());
    }

    let output = process_extract_images(params, config.max_image_count, config.max_output_bytes)?;
    Ok(crate::routes::pdf2img::output_to_response(output))
}

/// Build the JSON metadata response (no image bytes decoded).
fn build_images_json(
    params: ExtractImagesParams,
    config: &crate::config::ServerConfig,
) -> ServerResult<serde_json::Value> {
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
    crate::processing::check_image_count(config, image_count)?;

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

    Ok(serde_json::json!({
        "image_count": image_count,
        "pages_processed": pages_processed,
        "images": metadata,
    }))
}

/// Core extract-images (ZIP) processing shared by the synchronous handler and
/// the async job worker. Produces byte-identical output regardless of caller.
/// Takes owned limit values (not `&config`) so the job worker can run it inside
/// a `'static` blocking closure.
pub(crate) fn process_extract_images(
    params: ExtractImagesParams,
    max_image_count: usize,
    max_output_bytes: u64,
) -> ServerResult<ProcessedOutput> {
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
    // Reject an extraction that found more images than the cap before we spend
    // time decoding/encoding any of them.
    crate::processing::check_image_count_limit(max_image_count, image_count)?;

    let (zip_bytes, images_encoded) = build_zip(&engine, &images, &params, max_output_bytes)?;

    Ok(ProcessedOutput {
        bytes: zip_bytes,
        content_type: "application/zip",
        filename: "images.zip",
        extra_headers: vec![
            ("x-image-count", image_count.to_string()),
            ("x-images-encoded", images_encoded.to_string()),
            ("x-pages-processed", pages_processed.to_string()),
        ],
    })
}

pub(crate) async fn extract_extract_images_fields(
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
        Some("webp") => ImageOutputFormat::Webp,
        Some(other) => {
            return Err(ServerError::InvalidParameter(format!(
                "unknown format '{}'; use png, jpg, webp, or original",
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

fn build_zip(
    engine: &ContentEngine,
    images: &[ImageReference],
    params: &ExtractImagesParams,
    max_output_bytes: u64,
) -> ServerResult<(Vec<u8>, usize)> {
    let mut buf: Vec<u8> = Vec::new();
    let mut images_encoded = 0usize;
    let mut accumulated = 0usize;
    {
        let cursor = Cursor::new(&mut buf);
        let mut zw = ZipWriter::new(cursor);
        let zip_opts = FileOptions::<()>::default()
            .compression_method(CompressionMethod::Deflated)
            .compression_level(Some(6));

        // TODO: parallelise image encoding using rayon, then write ZIP entries sequentially.
        for (idx, img_ref) in images.iter().enumerate() {
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

            let suffix = if img_ref.is_inline { "-inline" } else { "" };
            let filename = format!(
                "page-{:03}-image-{:03}{}.{}",
                img_ref.page_number,
                idx + 1,
                suffix,
                ext
            );

            zw.start_file(&filename, zip_opts)
                .map_err(|e| ServerError::Internal(format!("ZIP start_file error: {}", e)))?;
            zw.write_all(&image_bytes)
                .map_err(|e| ServerError::Internal(format!("ZIP write error: {}", e)))?;
            images_encoded += 1;
            // Bound accumulated output so a PDF with thousands of large images
            // can't expand into an absurd ZIP held entirely in memory; error at
            // the cap instead. Pre-compression sizes are an upper bound on the
            // final ZIP.
            accumulated = accumulated.saturating_add(image_bytes.len());
            crate::processing::check_output_size_limit(max_output_bytes, accumulated)?;
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
    // XObject images can sometimes be emitted with their original compressed
    // bytes; inline images are always decoded from their captured data.
    if !img_ref.is_inline {
        if let Ok(Some((bytes, ext))) =
            ImageEncoder::keep_original(img_ref, engine.document().reader(), &params.image_format)
        {
            return Ok((bytes, ext));
        }
    }

    let raw = engine.decode_image(img_ref)?;
    let final_raw = if matches!(
        params.image_format,
        ImageOutputFormat::Png | ImageOutputFormat::Webp | ImageOutputFormat::Original
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
