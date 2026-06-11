use axum::extract::Multipart;
use axum::Json;
use bytes::Bytes;
use oxide_engine::{ContentEngine, PdfAnalyzer, TextLayerAnalysis};

use crate::error::{ServerError, ServerResult};
use crate::params::parse_bool_param;

pub async fn handler(multipart: Multipart) -> ServerResult<Json<TextLayerAnalysis>> {
    let mut file_bytes: Option<Bytes> = None;
    let mut full_str: Option<String> = None;
    let mut password_str: Option<String> = None;
    let mut mp = multipart;

    while let Some(field) = mp
        .next_field()
        .await
        .map_err(|e| ServerError::InvalidParameter(format!("{}", e)))?
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
            Some("full") => {
                full_str = Some(
                    field
                        .text()
                        .await
                        .map_err(|e| ServerError::InvalidParameter(format!("{}", e)))?,
                );
            }
            Some("password") => {
                password_str = Some(
                    field
                        .text()
                        .await
                        .map_err(|e| ServerError::InvalidParameter(format!("{}", e)))?,
                );
            }
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
    let do_full = parse_bool_param(full_str.as_deref(), false)?;

    tracing::info!(
        pdf_size = file.len(),
        full = do_full,
        "processing analyze request"
    );

    let engine = ContentEngine::open_bytes_with_password(
        file.to_vec(),
        password_str.as_deref().unwrap_or("").as_bytes(),
    )
    .map_err(ServerError::from)?;

    let analysis = if do_full {
        PdfAnalyzer::full_analysis(&engine)
    } else {
        PdfAnalyzer::quick_analysis(&engine)
    }
    .map_err(ServerError::from)?;

    tracing::info!(
        page_count = analysis.total_pages,
        has_text = analysis.has_text_layer,
        "analyze complete"
    );

    Ok(Json(analysis))
}
