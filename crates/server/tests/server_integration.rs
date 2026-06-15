use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
};
use serde_json::Value;
use std::path::Path;
use tower::util::ServiceExt;

fn fixture_pdf(name: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../engine/tests/fixtures")
        .join(name);
    std::fs::read(path).unwrap()
}

fn make_multipart(filename: &str, pdf: &[u8], extra: &[(&str, &str)]) -> (String, Vec<u8>) {
    let boundary = "oxide-test-boundary-xyz";
    let mut body: Vec<u8> = Vec::new();

    body.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
    body.extend_from_slice(
        format!(
            "Content-Disposition: form-data; name=\"file\"; filename=\"{}\"\r\n\
             Content-Type: application/pdf\r\n\r\n",
            filename
        )
        .as_bytes(),
    );
    body.extend_from_slice(pdf);
    body.extend_from_slice(b"\r\n");

    for (name, value) in extra {
        body.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
        body.extend_from_slice(
            format!(
                "Content-Disposition: form-data; name=\"{}\"\r\n\r\n{}",
                name, value
            )
            .as_bytes(),
        );
        body.extend_from_slice(b"\r\n");
    }

    body.extend_from_slice(format!("--{}--\r\n", boundary).as_bytes());
    let ct = format!("multipart/form-data; boundary={}", boundary);
    (ct, body)
}

#[tokio::test]
async fn health_check_returns_ok() {
    let app = oxide_server::app::create_app();
    let response = app
        .oneshot(Request::get("/health").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), 1024).await.unwrap();
    assert_eq!(&body[..], b"ok");
}

#[tokio::test]
async fn readiness_endpoint_returns_ready() {
    let app = oxide_server::app::create_app();
    let response = app
        .oneshot(Request::get("/readiness").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body_bytes = to_bytes(response.into_body(), 4096).await.unwrap();
    let json: Value = serde_json::from_slice(&body_bytes).unwrap();
    assert_eq!(json["status"], "ready");
    assert!(json["version"].is_string());
}

#[test]
fn server_config_defaults_are_sane() {
    let cfg = oxide_server::config::ServerConfig::default();
    assert_eq!(cfg.port, 8080);
    assert_eq!(cfg.max_dpi, 600);
    assert!(cfg.max_file_size > 0);
    assert!(cfg.max_pages > 0);
}

#[test]
fn config_default_port_and_max_dpi_are_sane() {
    let cfg = oxide_server::config::ServerConfig::default();
    assert_eq!(cfg.port, 8080, "default port should be 8080");
    assert_eq!(cfg.max_dpi, 600);
}

#[test]
fn server_config_max_dpi_default_is_capped_at_600() {
    let cfg = oxide_server::config::ServerConfig::default();
    assert!(cfg.max_dpi <= 600, "max_dpi should never exceed 600");
}

#[tokio::test]
async fn health_still_returns_ok() {
    let app = oxide_server::app::create_app();
    let response = app
        .oneshot(Request::get("/health").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn api_readiness_alias_works() {
    let app = oxide_server::app::create_app();
    let response = app
        .oneshot(
            Request::get("/api/v1/readiness")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn api_health_alias_works() {
    let app = oxide_server::app::create_app();
    let response = app
        .oneshot(Request::get("/api/v1/health").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn version_endpoint_returns_json() {
    let app = oxide_server::app::create_app();
    let response = app
        .oneshot(Request::get("/api/v1/version").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body_bytes = to_bytes(response.into_body(), 4096).await.unwrap();
    let json: Value = serde_json::from_slice(&body_bytes).unwrap();
    assert_eq!(json["product"], "Oxide");
    assert!(json["version"].is_string());
}

#[tokio::test]
async fn extract_text_missing_file_returns_400() {
    let app = oxide_server::app::create_app();
    let boundary = "test-boundary";
    let body = format!(
        "--{}\r\nContent-Disposition: form-data; name=\"pages\"\r\n\r\nall\r\n--{}--\r\n",
        boundary, boundary
    );
    let response = app
        .oneshot(
            Request::post("/api/v1/extract-text")
                .header(
                    "content-type",
                    format!("multipart/form-data; boundary={}", boundary),
                )
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body_bytes = to_bytes(response.into_body(), 4096).await.unwrap();
    let json: Value = serde_json::from_slice(&body_bytes).unwrap();
    assert_eq!(json["error"], "missing_file");
}

#[tokio::test]
async fn extract_text_with_flate_pdf_returns_text() {
    let pdf = fixture_pdf("flate.pdf");
    let (ct, body_bytes) = make_multipart("test.pdf", &pdf, &[("page_markers", "false")]);
    let app = oxide_server::app::create_app();
    let response = app
        .oneshot(
            Request::post("/api/v1/extract-text")
                .header("content-type", ct)
                .body(Body::from(body_bytes))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let resp_bytes = to_bytes(response.into_body(), 65536).await.unwrap();
    let text = String::from_utf8(resp_bytes.to_vec()).unwrap();
    assert!(
        !text.trim().is_empty(),
        "extracted text should not be empty"
    );
}

#[tokio::test]
async fn extract_text_json_format_returns_valid_json() {
    let pdf = fixture_pdf("flate.pdf");
    let (ct, body_bytes) = make_multipart("test.pdf", &pdf, &[("output_format", "json")]);
    let app = oxide_server::app::create_app();
    let response = app
        .oneshot(
            Request::post("/api/v1/extract-text")
                .header("content-type", ct)
                .body(Body::from(body_bytes))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body_bytes = to_bytes(response.into_body(), 65536).await.unwrap();
    let json: Value = serde_json::from_slice(&body_bytes).unwrap();
    assert!(
        json["pages"].is_array(),
        "response should have 'pages' array"
    );
    assert!(json["total_pages"].is_number());
    assert!(json["has_text_layer"].is_boolean());
    assert!(json["is_likely_scanned"].is_boolean());
    let pages = json["pages"].as_array().unwrap();
    assert!(!pages.is_empty(), "pages array should not be empty");
    let first_page = &pages[0];
    assert_eq!(first_page["page"], 1);
    assert!(first_page["text"].is_string());
    assert!(first_page["line_count"].is_number());
    assert!(first_page["char_count"].is_number());
}

#[tokio::test]
async fn extract_text_scanned_pdf_returns_422() {
    let pdf = fixture_pdf("image_only.pdf");
    let (ct, body_bytes) = make_multipart("scanned.pdf", &pdf, &[]);
    let app = oxide_server::app::create_app();
    let response = app
        .oneshot(
            Request::post("/api/v1/extract-text")
                .header("content-type", ct)
                .body(Body::from(body_bytes))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body_bytes = to_bytes(response.into_body(), 4096).await.unwrap();
    let json: Value = serde_json::from_slice(&body_bytes).unwrap();
    assert_eq!(json["error"], "no_text_layer");
}

#[tokio::test]
async fn extract_text_invalid_page_range_returns_400() {
    let pdf = fixture_pdf("flate.pdf");
    let (ct, body_bytes) = make_multipart("test.pdf", &pdf, &[("pages", "999")]);
    let app = oxide_server::app::create_app();
    let response = app
        .oneshot(
            Request::post("/api/v1/extract-text")
                .header("content-type", ct)
                .body(Body::from(body_bytes))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn analyze_endpoint_returns_analysis() {
    let pdf = fixture_pdf("flate.pdf");
    let (ct, body_bytes) = make_multipart("test.pdf", &pdf, &[]);
    let app = oxide_server::app::create_app();
    let response = app
        .oneshot(
            Request::post("/api/v1/analyze")
                .header("content-type", ct)
                .body(Body::from(body_bytes))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body_bytes = to_bytes(response.into_body(), 4096).await.unwrap();
    let json: Value = serde_json::from_slice(&body_bytes).unwrap();
    assert!(
        json["has_text_layer"].as_bool().unwrap_or(false),
        "flate.pdf should have text layer"
    );
    assert!(
        !json["is_likely_scanned"].as_bool().unwrap_or(true),
        "flate.pdf should not be scanned"
    );
    assert!(json["total_pages"].as_u64().unwrap_or(0) >= 1);
    assert_eq!(json["recommendation"], "UseExtractText");
}

#[tokio::test]
async fn analyze_missing_file_returns_400() {
    let app = oxide_server::app::create_app();
    let boundary = "test-boundary";
    let body = format!("--{}--\r\n", boundary);
    let response = app
        .oneshot(
            Request::post("/api/v1/analyze")
                .header(
                    "content-type",
                    format!("multipart/form-data; boundary={}", boundary),
                )
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn extract_text_page_markers_false_omits_marker() {
    let pdf = fixture_pdf("flate.pdf");
    let (ct, body_bytes) = make_multipart("test.pdf", &pdf, &[("page_markers", "false")]);
    let app = oxide_server::app::create_app();
    let response = app
        .oneshot(
            Request::post("/api/v1/extract-text")
                .header("content-type", ct)
                .body(Body::from(body_bytes))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body_bytes = to_bytes(response.into_body(), 65536).await.unwrap();
    let text = String::from_utf8_lossy(&body_bytes);
    assert!(
        !text.contains("--- Page"),
        "page_markers=false should produce no page markers"
    );
}

#[tokio::test]
async fn extract_text_specific_page_range() {
    let pdf = fixture_pdf("flate.pdf");
    let (ct, body_bytes) = make_multipart(
        "test.pdf",
        &pdf,
        &[("pages", "1"), ("page_markers", "false")],
    );
    let app = oxide_server::app::create_app();
    let response = app
        .oneshot(
            Request::post("/api/v1/extract-text")
                .header("content-type", ct)
                .body(Body::from(body_bytes))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body_bytes = to_bytes(response.into_body(), 65536).await.unwrap();
    assert!(
        !body_bytes.is_empty(),
        "specific page request should return text"
    );
}

#[tokio::test]
async fn extract_text_invalid_boolean_param_returns_400() {
    let pdf = fixture_pdf("flate.pdf");
    let (ct, body_bytes) = make_multipart("test.pdf", &pdf, &[("page_markers", "maybe")]);
    let app = oxide_server::app::create_app();
    let response = app
        .oneshot(
            Request::post("/api/v1/extract-text")
                .header("content-type", ct)
                .body(Body::from(body_bytes))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body_bytes = to_bytes(response.into_body(), 4096).await.unwrap();
    let json: Value = serde_json::from_slice(&body_bytes).unwrap();
    assert_eq!(json["error"], "invalid_parameter");
}

#[tokio::test]
async fn extract_text_invalid_output_format_returns_400() {
    let pdf = fixture_pdf("flate.pdf");
    let (ct, body_bytes) = make_multipart("test.pdf", &pdf, &[("output_format", "pdf")]);
    let app = oxide_server::app::create_app();
    let response = app
        .oneshot(
            Request::post("/api/v1/extract-text")
                .header("content-type", ct)
                .body(Body::from(body_bytes))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn extract_images_returns_zip() {
    let pdf = fixture_pdf("image_only.pdf");
    let (ct, body_bytes) = make_multipart("test.pdf", &pdf, &[]);
    let app = oxide_server::app::create_app();
    let response = app
        .oneshot(
            Request::post("/api/v1/extract-images")
                .header("content-type", ct)
                .body(Body::from(body_bytes))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let count = response
        .headers()
        .get("x-image-count")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(0);
    let body_bytes = to_bytes(response.into_body(), 1_000_000).await.unwrap();
    assert!(
        body_bytes.starts_with(b"PK"),
        "response body should be a ZIP file; got {:?}",
        &body_bytes[..4.min(body_bytes.len())]
    );
    if count > 0 {
        assert!(
            body_bytes.len() > 100,
            "non-empty ZIP should be more than 100 bytes"
        );
    }
}

#[tokio::test]
async fn extract_images_json_mode_returns_metadata_only() {
    let pdf = fixture_pdf("image_only.pdf");
    let (ct, body_bytes) = make_multipart("test.pdf", &pdf, &[("output_format", "json")]);
    let app = oxide_server::app::create_app();
    let response = app
        .oneshot(
            Request::post("/api/v1/extract-images")
                .header("content-type", ct)
                .body(Body::from(body_bytes))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body_bytes = to_bytes(response.into_body(), 65536).await.unwrap();
    let json: Value = serde_json::from_slice(&body_bytes).unwrap();
    assert!(json["image_count"].is_number(), "should have image_count");
    assert!(
        json["pages_processed"].is_number(),
        "should have pages_processed"
    );
    assert!(json["images"].is_array(), "should have images array");
    if let Some(images) = json["images"].as_array() {
        for img in images {
            assert!(
                img.get("data").is_none(),
                "JSON mode should not include image bytes"
            );
            assert!(
                img.get("bytes").is_none(),
                "JSON mode should not include byte payloads"
            );
        }
    }
}

#[tokio::test]
async fn extract_images_missing_file_returns_400() {
    let app = oxide_server::app::create_app();
    let boundary = "test-boundary";
    let body = format!("--{}--\r\n", boundary);
    let response = app
        .oneshot(
            Request::post("/api/v1/extract-images")
                .header(
                    "content-type",
                    format!("multipart/form-data; boundary={}", boundary),
                )
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body_bytes = to_bytes(response.into_body(), 4096).await.unwrap();
    let json: Value = serde_json::from_slice(&body_bytes).unwrap();
    assert_eq!(json["error"], "missing_file");
}

#[tokio::test]
async fn extract_images_invalid_format_returns_400() {
    let pdf = fixture_pdf("image_only.pdf");
    let (ct, body_bytes) = make_multipart("test.pdf", &pdf, &[("format", "bmp")]);
    let app = oxide_server::app::create_app();
    let response = app
        .oneshot(
            Request::post("/api/v1/extract-images")
                .header("content-type", ct)
                .body(Body::from(body_bytes))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn extract_images_text_only_pdf_returns_empty_zip() {
    let pdf = fixture_pdf("flate.pdf");
    let (ct, body_bytes) = make_multipart("test.pdf", &pdf, &[]);
    let app = oxide_server::app::create_app();
    let response = app
        .oneshot(
            Request::post("/api/v1/extract-images")
                .header("content-type", ct)
                .body(Body::from(body_bytes))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "text-only PDF should return 200, not error"
    );
    let count = response
        .headers()
        .get("x-image-count")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(usize::MAX);
    assert_eq!(count, 0, "text-only PDF should have X-Image-Count: 0");
    let body_bytes = to_bytes(response.into_body(), 65536).await.unwrap();
    assert!(body_bytes.starts_with(b"PK"));
}

#[tokio::test]
async fn extract_images_with_format_png_returns_zip_with_png_files() {
    let pdf = fixture_pdf("image_only.pdf");
    let (ct, body_bytes) = make_multipart("test.pdf", &pdf, &[("format", "png")]);
    let app = oxide_server::app::create_app();
    let response = app
        .oneshot(
            Request::post("/api/v1/extract-images")
                .header("content-type", ct)
                .body(Body::from(body_bytes))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body_bytes = to_bytes(response.into_body(), 1_000_000).await.unwrap();
    assert!(body_bytes.starts_with(b"PK"), "should return a ZIP file");
}

#[tokio::test]
async fn extract_images_with_format_webp_returns_zip_and_succeeds() {
    let pdf = fixture_pdf("image_only.pdf");
    let (ct, body_bytes) = make_multipart("test.pdf", &pdf, &[("format", "webp")]);
    let app = oxide_server::app::create_app();
    let response = app
        .oneshot(
            Request::post("/api/v1/extract-images")
                .header("content-type", ct)
                .body(Body::from(body_bytes))
                .unwrap(),
        )
        .await
        .unwrap();
    // WebP is now supported (no longer a 400).
    assert_eq!(response.status(), StatusCode::OK);
    let images_encoded = response
        .headers()
        .get("x-images-encoded")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(0);
    assert!(
        images_encoded >= 1,
        "image_only.pdf should encode at least one WebP image"
    );
    let body_bytes = to_bytes(response.into_body(), 1_000_000).await.unwrap();
    assert!(body_bytes.starts_with(b"PK"), "should return a ZIP file");
}

#[tokio::test]
async fn pdf2img_returns_zip_with_png_pages() {
    let pdf = fixture_pdf("flate.pdf");
    let (ct, body_bytes) = make_multipart("test.pdf", &pdf, &[("dpi", "72")]);
    let app = oxide_server::app::create_app();
    let response = app
        .oneshot(
            Request::post("/api/v1/pdf2img")
                .header("content-type", ct)
                .body(Body::from(body_bytes))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let x_page_count = response
        .headers()
        .get("x-page-count")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(0);
    let x_pages_rendered = response
        .headers()
        .get("x-pages-rendered")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(0);
    let x_dpi = response
        .headers()
        .get("x-dpi")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(0);
    println!(
        "pdf2img png headers: page_count={}, pages_rendered={}, dpi={}",
        x_page_count, x_pages_rendered, x_dpi
    );

    assert!(x_page_count > 0);
    assert_eq!(x_pages_rendered, x_page_count);
    assert_eq!(x_dpi, 72);

    let body_bytes = to_bytes(response.into_body(), 50_000_000).await.unwrap();
    assert!(body_bytes.starts_with(b"PK"));
    let cursor = std::io::Cursor::new(&body_bytes);
    let mut archive = zip::ZipArchive::new(cursor).unwrap();
    assert!(!archive.is_empty());

    let mut page_file = archive.by_index(0).unwrap();
    assert!(page_file.name().ends_with(".png"));
    let mut content = Vec::new();
    use std::io::Read;
    page_file.read_to_end(&mut content).unwrap();
    assert!(content.starts_with(&[0x89, b'P', b'N', b'G']));
}

#[tokio::test]
async fn pdf2img_with_jpeg_format_returns_jpeg_pages() {
    let pdf = fixture_pdf("flate.pdf");
    let (ct, body_bytes) = make_multipart("test.pdf", &pdf, &[("dpi", "72"), ("format", "jpg")]);
    let app = oxide_server::app::create_app();
    let response = app
        .oneshot(
            Request::post("/api/v1/pdf2img")
                .header("content-type", ct)
                .body(Body::from(body_bytes))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body_bytes = to_bytes(response.into_body(), 50_000_000).await.unwrap();
    assert!(body_bytes.starts_with(b"PK"));

    let cursor = std::io::Cursor::new(&body_bytes);
    let mut archive = zip::ZipArchive::new(cursor).unwrap();
    assert!(!archive.is_empty());
    let mut page_file = archive.by_index(0).unwrap();
    assert!(page_file.name().ends_with(".jpg"));
    let mut content = Vec::new();
    use std::io::Read;
    page_file.read_to_end(&mut content).unwrap();
    assert_eq!(&content[..2], &[0xFF, 0xD8]);
}

#[tokio::test]
async fn pdf2img_missing_file_returns_400() {
    let app = oxide_server::app::create_app();
    let boundary = "bound";
    let body = format!("--{}--\r\n", boundary);
    let response = app
        .oneshot(
            Request::post("/api/v1/pdf2img")
                .header(
                    "content-type",
                    format!("multipart/form-data; boundary={}", boundary),
                )
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn pdf2img_invalid_dpi_returns_400() {
    let pdf = fixture_pdf("flate.pdf");
    let (ct, body_bytes) = make_multipart("test.pdf", &pdf, &[("dpi", "2")]);
    let app = oxide_server::app::create_app();
    let response = app
        .oneshot(
            Request::post("/api/v1/pdf2img")
                .header("content-type", ct)
                .body(Body::from(body_bytes))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn pdf2img_invalid_format_returns_400() {
    let pdf = fixture_pdf("flate.pdf");
    let (ct, body_bytes) = make_multipart("test.pdf", &pdf, &[("format", "bmp")]);
    let app = oxide_server::app::create_app();
    let response = app
        .oneshot(
            Request::post("/api/v1/pdf2img")
                .header("content-type", ct)
                .body(Body::from(body_bytes))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn pdf2img_page_range_limits_pages() {
    let pdf = fixture_pdf("flate.pdf");
    let (ct, body_bytes) = make_multipart("test.pdf", &pdf, &[("pages", "1"), ("dpi", "72")]);
    let app = oxide_server::app::create_app();
    let response = app
        .oneshot(
            Request::post("/api/v1/pdf2img")
                .header("content-type", ct)
                .body(Body::from(body_bytes))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let x_pages = response
        .headers()
        .get("x-pages-rendered")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(999);
    assert_eq!(x_pages, 1);
}

#[tokio::test]
async fn pdf2img_high_dpi_page_has_larger_dimensions() {
    async fn render_zip_size(pdf: &[u8], dpi: &str) -> usize {
        let (ct, body_bytes) = make_multipart("test.pdf", pdf, &[("dpi", dpi)]);
        let app = oxide_server::app::create_app();
        let response = app
            .oneshot(
                Request::post("/api/v1/pdf2img")
                    .header("content-type", ct)
                    .body(Body::from(body_bytes))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        to_bytes(response.into_body(), 50_000_000)
            .await
            .unwrap()
            .len()
    }

    let pdf = fixture_pdf("flate.pdf");
    let size_72 = render_zip_size(&pdf, "72").await;
    let size_144 = render_zip_size(&pdf, "144").await;
    println!("pdf2img zip sizes: dpi72={}, dpi144={}", size_72, size_144);
    assert!(
        size_144 > size_72,
        "144 DPI output ({}) should exceed 72 DPI output ({})",
        size_144,
        size_72
    );
}

#[tokio::test]
async fn pdf2img_renders_pages_in_correct_order() {
    let pdf = fixture_pdf("flate.pdf");
    let (ct, body_bytes) = make_multipart("test.pdf", &pdf, &[("dpi", "72")]);
    let app = oxide_server::app::create_app();
    let response = app
        .oneshot(
            Request::post("/api/v1/pdf2img")
                .header("content-type", ct)
                .body(Body::from(body_bytes))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body_bytes = to_bytes(response.into_body(), 50_000_000).await.unwrap();
    let cursor = std::io::Cursor::new(&body_bytes);
    let mut archive = zip::ZipArchive::new(cursor).unwrap();
    let mut names = Vec::new();
    for i in 0..archive.len() {
        names.push(archive.by_index(i).unwrap().name().to_string());
    }

    let mut sorted = names.clone();
    sorted.sort();
    assert_eq!(names, sorted);
}

#[tokio::test]
async fn pdf2img_default_dpi_header_is_150() {
    let pdf = fixture_pdf("image_only.pdf");
    let (ct, body_bytes) = make_multipart("test.pdf", &pdf, &[]);
    let app = oxide_server::app::create_app();
    let response = app
        .oneshot(
            Request::post("/api/v1/pdf2img")
                .header("content-type", ct)
                .body(Body::from(body_bytes))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let x_dpi = response
        .headers()
        .get("x-dpi")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert_eq!(x_dpi, "150");
}

#[tokio::test]
async fn content_type_header_is_correct_for_txt() {
    let pdf = fixture_pdf("flate.pdf");
    let (ct, body_bytes) = make_multipart(
        "test.pdf",
        &pdf,
        &[("output_format", "txt"), ("page_markers", "false")],
    );
    let app = oxide_server::app::create_app();
    let response = app
        .oneshot(
            Request::post("/api/v1/extract-text")
                .header("content-type", ct)
                .body(Body::from(body_bytes))
                .unwrap(),
        )
        .await
        .unwrap();
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        content_type.contains("text/plain"),
        "txt output should have text/plain content-type, got: {}",
        content_type
    );
    assert!(
        content_type.contains("utf-8") || content_type.contains("UTF-8"),
        "should specify utf-8 charset"
    );
}

#[tokio::test]
async fn auth_disabled_by_default_allows_all_requests() {
    let pdf = fixture_pdf("flate.pdf");
    let (ct, body) = make_multipart("test.pdf", &pdf, &[]);
    let app = oxide_server::app::create_app();
    let response = app
        .oneshot(
            Request::post("/api/v1/extract-text")
                .header("content-type", ct)
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_ne!(response.status(), StatusCode::UNAUTHORIZED);
    assert_ne!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn rate_limit_disabled_by_default_allows_rapid_health_requests() {
    let app = oxide_server::app::create_app();
    for _ in 0..5 {
        let response = app
            .clone()
            .oneshot(Request::get("/health").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }
}

#[tokio::test]
async fn version_endpoint_returns_version_string() {
    let app = oxide_server::app::create_app();
    let response = app
        .oneshot(Request::get("/api/v1/version").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body_bytes = to_bytes(response.into_body(), 4096).await.unwrap();
    let json: Value = serde_json::from_slice(&body_bytes).unwrap();
    assert!(json["version"].is_string(), "version field should exist");
}

#[tokio::test]
async fn e2e_extract_text_returns_content() {
    let pdf = fixture_pdf("flate.pdf");
    let (ct, body) = make_multipart("test.pdf", &pdf, &[]);
    let app = oxide_server::app::create_app();
    let response = app
        .oneshot(
            Request::post("/api/v1/extract-text")
                .header("content-type", ct)
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body_bytes = to_bytes(response.into_body(), 65536).await.unwrap();
    let text = String::from_utf8(body_bytes.to_vec()).unwrap();
    assert!(
        text.contains("Hi"),
        "extract-text should include fixture text"
    );
}

#[tokio::test]
async fn e2e_pdf2img_produces_valid_zip_with_png() {
    let pdf = fixture_pdf("flate.pdf");
    let (ct, body) = make_multipart("test.pdf", &pdf, &[("dpi", "72")]);
    let app = oxide_server::app::create_app();
    let response = app
        .oneshot(
            Request::post("/api/v1/pdf2img")
                .header("content-type", ct)
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let zip_bytes = to_bytes(response.into_body(), 10_000_000).await.unwrap();
    assert!(zip_bytes.starts_with(b"PK"), "should be ZIP");

    let cursor = std::io::Cursor::new(&zip_bytes);
    let mut archive = zip::ZipArchive::new(cursor).unwrap();
    assert!(!archive.is_empty(), "ZIP should have at least 1 page");
    let mut page_file = archive.by_index(0).unwrap();
    assert!(page_file.name().ends_with(".png"));
    use std::io::Read;
    let mut content = Vec::new();
    page_file.read_to_end(&mut content).unwrap();
    assert!(content.starts_with(&[0x89, b'P', b'N', b'G']));
}

#[tokio::test]
async fn e2e_analyze_detects_text_layer() {
    let pdf = fixture_pdf("flate.pdf");
    let (ct, body) = make_multipart("test.pdf", &pdf, &[]);
    let app = oxide_server::app::create_app();
    let response = app
        .oneshot(
            Request::post("/api/v1/analyze")
                .header("content-type", ct)
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body_bytes = to_bytes(response.into_body(), 4096).await.unwrap();
    let json: Value = serde_json::from_slice(&body_bytes).unwrap();
    assert!(json["has_text_layer"].is_boolean());
}

#[tokio::test]
async fn e2e_all_endpoints_return_200_for_valid_pdf() {
    let pdf = fixture_pdf("flate.pdf");
    let app = oxide_server::app::create_app();

    for (path, extra) in [
        ("/api/v1/extract-text", Vec::<(&str, &str)>::new()),
        ("/api/v1/extract-images", Vec::<(&str, &str)>::new()),
        ("/api/v1/analyze", Vec::<(&str, &str)>::new()),
        ("/api/v1/pdf2img", vec![("dpi", "72")]),
    ] {
        let (ct, body) = make_multipart("test.pdf", &pdf, &extra);
        let response = app
            .clone()
            .oneshot(
                Request::post(path)
                    .header("content-type", ct)
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{} failed", path);
    }
}

#[tokio::test]
async fn e2e_non_pdf_bytes_returns_error() {
    let garbage = b"this is not a pdf file, definitely not";
    let (ct, body) = make_multipart("garbage.pdf", garbage, &[]);
    let app = oxide_server::app::create_app();
    let response = app
        .oneshot(
            Request::post("/api/v1/extract-text")
                .header("content-type", ct)
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();

    assert!(response.status().is_client_error());
    assert_ne!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
}

#[tokio::test]
async fn e2e_extract_images_from_image_pdf() {
    let pdf = fixture_pdf("image_only.pdf");
    let (ct, body) = make_multipart("test.pdf", &pdf, &[("format", "png")]);
    let app = oxide_server::app::create_app();
    let response = app
        .oneshot(
            Request::post("/api/v1/extract-images")
                .header("content-type", ct)
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let img_count = response
        .headers()
        .get("x-image-count")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(0);
    assert!(
        img_count > 0,
        "image_only.pdf should have at least one image"
    );
    let zip_bytes = to_bytes(response.into_body(), 1_000_000).await.unwrap();
    assert!(zip_bytes.starts_with(b"PK"));
}

#[tokio::test]
async fn e2e_render_image_pdf_contains_non_white_pixels() {
    let pdf = fixture_pdf("image_only.pdf");
    let (ct, body) = make_multipart("test.pdf", &pdf, &[("dpi", "72")]);
    let app = oxide_server::app::create_app();
    let response = app
        .oneshot(
            Request::post("/api/v1/pdf2img")
                .header("content-type", ct)
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let zip_bytes = to_bytes(response.into_body(), 10_000_000).await.unwrap();
    let cursor = std::io::Cursor::new(&zip_bytes);
    let mut archive = zip::ZipArchive::new(cursor).unwrap();
    assert_eq!(archive.len(), 1, "one page");

    let mut file = archive.by_index(0).unwrap();
    use std::io::Read;
    let mut png_bytes = Vec::new();
    file.read_to_end(&mut png_bytes).unwrap();
    assert!(png_bytes.starts_with(&[0x89, b'P', b'N', b'G']));

    let decoder = png::Decoder::new(std::io::Cursor::new(&png_bytes));
    let mut reader = decoder.read_info().unwrap();
    let mut pixels = vec![0u8; reader.output_buffer_size()];
    let info = reader.next_frame(&mut pixels).unwrap();
    pixels.truncate(info.buffer_size());
    let has_non_white = pixels
        .chunks(3)
        .any(|p| p[0] != 255 || p[1] != 255 || p[2] != 255);
    assert!(
        has_non_white,
        "rendered image_only.pdf should have non-white pixels"
    );
}

// ---------------------------------------------------------------------------
// Encryption: optional `password` form field
// ---------------------------------------------------------------------------

#[tokio::test]
async fn unencrypted_pdf_with_password_param_still_works() {
    // Supplying a password for a non-encrypted PDF must not break it.
    let pdf = fixture_pdf("flate.pdf");
    let (ct, body) = make_multipart("test.pdf", &pdf, &[("password", "any_password")]);
    let app = oxide_server::app::create_app();
    let response = app
        .oneshot(
            Request::post("/api/v1/extract-text")
                .header("content-type", ct)
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "non-encrypted PDF should succeed even with a password param"
    );
}

#[tokio::test]
async fn analyze_with_password_param_on_unencrypted_pdf_works() {
    let pdf = fixture_pdf("flate.pdf");
    let (ct, body) = make_multipart("test.pdf", &pdf, &[("password", "ignored")]);
    let app = oxide_server::app::create_app();
    let response = app
        .oneshot(
            Request::post("/api/v1/analyze")
                .header("content-type", ct)
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn invalid_pdf_returns_client_or_server_error() {
    let garbage = b"%PDF-1.4 ... this is not real PDF content";
    let (ct, body) = make_multipart("test.pdf", garbage, &[]);
    let app = oxide_server::app::create_app();
    let response = app
        .oneshot(
            Request::post("/api/v1/extract-text")
                .header("content-type", ct)
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        response.status().is_client_error() || response.status().is_server_error(),
        "invalid PDF should return an error status, got {}",
        response.status()
    );
}

#[tokio::test]
async fn password_protected_pdf_without_password_returns_422() {
    // A genuine V2/R3 encrypted PDF whose /U does not match the empty password.
    // Synthesised inline so the test needs no external fixture.
    let pdf = build_password_protected_pdf();
    let (ct, body) = make_multipart("protected.pdf", &pdf, &[]);
    let app = oxide_server::app::create_app();
    let response = app
        .oneshot(
            Request::post("/api/v1/extract-text")
                .header("content-type", ct)
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::UNPROCESSABLE_ENTITY,
        "password-protected PDF without a password should map to 422"
    );
    let body_bytes = to_bytes(response.into_body(), 4096).await.unwrap();
    let json: Value = serde_json::from_slice(&body_bytes).unwrap();
    assert_eq!(json["error"], "encrypted");
}

/// Build a minimal V2/R3 encrypted PDF whose `/U` deliberately does not match
/// the empty user password, so the server cannot open it without a password.
fn build_password_protected_pdf() -> Vec<u8> {
    fn hex(bytes: &[u8]) -> String {
        let mut s = String::with_capacity(bytes.len() * 2);
        for b in bytes {
            s.push_str(&format!("{:02X}", b));
        }
        s
    }

    let mut bytes: Vec<u8> = Vec::new();
    let mut offsets = [0usize; 3];
    bytes.extend_from_slice(b"%PDF-1.4\n");
    offsets[1] = bytes.len();
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    offsets[2] = bytes.len();
    bytes.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [] /Count 0 >>\nendobj\n");
    let xref = bytes.len();
    bytes.extend_from_slice(b"xref\n0 3\n0000000000 65535 f \n");
    for off in offsets.iter().skip(1) {
        bytes.extend_from_slice(format!("{:010} 00000 n \n", off).as_bytes());
    }
    let owner_o = vec![0xABu8; 32];
    let user_u = vec![0xCDu8; 32]; // will not verify against the empty password
    let file_id = b"0123456789abcdef";
    let trailer = format!(
        "trailer\n<< /Size 3 /Root 1 0 R /Encrypt << /Filter /Standard /V 2 /R 3 /Length 128 \
         /P -3904 /O <{}> /U <{}> >> /ID [<{}> <{}>] >>\nstartxref\n{}\n%%EOF\n",
        hex(&owner_o),
        hex(&user_u),
        hex(file_id),
        hex(file_id),
        xref
    );
    bytes.extend_from_slice(trailer.as_bytes());
    bytes
}

// ---------------------------------------------------------------------------
// Form XObject rendering
// ---------------------------------------------------------------------------

/// Same minimal Form-XObject PDF as the engine integration test (a 50×50 gray
/// square centred on a 100×100 page). Duplicated here because test helpers do
/// not cross crate boundaries.
fn build_form_xobject_pdf() -> Vec<u8> {
    let form_stream_content: &[u8] = b"0.5 g\n0 0 50 50 re\nf\n";
    let page_stream_content: &[u8] = b"q\n1 0 0 1 25 25 cm\n/Fm0 Do\nQ\n";

    let mut pdf: Vec<u8> = Vec::new();
    let mut offsets = [0usize; 6];
    pdf.extend_from_slice(b"%PDF-1.4\n");

    offsets[1] = pdf.len();
    pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    offsets[2] = pdf.len();
    pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");

    offsets[5] = pdf.len();
    pdf.extend_from_slice(
        format!(
            "5 0 obj\n<< /Type /XObject /Subtype /Form /BBox [0 0 50 50] \
             /Resources << /ProcSet [/PDF] >> /Length {} >>\nstream\n",
            form_stream_content.len()
        )
        .as_bytes(),
    );
    pdf.extend_from_slice(form_stream_content);
    pdf.extend_from_slice(b"\nendstream\nendobj\n");

    offsets[4] = pdf.len();
    pdf.extend_from_slice(
        format!(
            "4 0 obj\n<< /Length {} >>\nstream\n",
            page_stream_content.len()
        )
        .as_bytes(),
    );
    pdf.extend_from_slice(page_stream_content);
    pdf.extend_from_slice(b"\nendstream\nendobj\n");

    offsets[3] = pdf.len();
    pdf.extend_from_slice(
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] \
          /Contents 4 0 R \
          /Resources << /XObject << /Fm0 5 0 R >> /ProcSet [/PDF] >> >>\nendobj\n",
    );

    let xref_offset = pdf.len();
    pdf.extend_from_slice(b"xref\n0 6\n0000000000 65535 f \n");
    for off in offsets.iter().skip(1) {
        pdf.extend_from_slice(format!("{:010} 00000 n \n", off).as_bytes());
    }
    pdf.extend_from_slice(
        format!("trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n").as_bytes(),
    );
    pdf
}

#[tokio::test]
async fn pdf2img_form_xobject_pdf_returns_valid_png() {
    let pdf_bytes = build_form_xobject_pdf();
    let (ct, body) = make_multipart("form.pdf", &pdf_bytes, &[("dpi", "72")]);
    let app = oxide_server::app::create_app();
    let response = app
        .oneshot(
            Request::post("/api/v1/pdf2img")
                .header("content-type", ct)
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let zip_bytes = to_bytes(response.into_body(), 5_000_000).await.unwrap();
    assert!(zip_bytes.starts_with(b"PK"), "should be a ZIP archive");

    let cursor = std::io::Cursor::new(&zip_bytes);
    let mut archive = zip::ZipArchive::new(cursor).unwrap();
    assert_eq!(archive.len(), 1, "one page");
    let mut file = archive.by_index(0).unwrap();
    use std::io::Read;
    let mut png = Vec::new();
    file.read_to_end(&mut png).unwrap();
    assert!(
        png.starts_with(&[0x89, b'P', b'N', b'G']),
        "should be a PNG"
    );

    // The rendered page should contain non-white (gray) pixels from the Form.
    let decoder = png::Decoder::new(std::io::Cursor::new(&png));
    let mut reader = decoder.read_info().unwrap();
    let mut pixels = vec![0u8; reader.output_buffer_size()];
    let info = reader.next_frame(&mut pixels).unwrap();
    pixels.truncate(info.buffer_size());
    let has_non_white = pixels
        .chunks(3)
        .any(|p| p[0] != 255 || p[1] != 255 || p[2] != 255);
    assert!(has_non_white, "Form XObject should paint non-white pixels");
}
