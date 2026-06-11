use axum::{
    body::Body,
    http::{Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};

pub(crate) fn extract_api_key(req: &Request<Body>) -> Option<String> {
    if let Some(value) = req.headers().get("x-api-key") {
        if let Ok(key) = value.to_str() {
            return Some(key.to_string());
        }
    }

    if let Some(value) = req.headers().get("authorization") {
        if let Ok(header) = value.to_str() {
            if let Some(key) = header.strip_prefix("Bearer ") {
                return Some(key.to_string());
            }
        }
    }

    None
}

pub async fn auth_middleware(req: Request<Body>, next: Next) -> Response {
    let config = crate::config::get_config();

    if config.api_keys.is_empty() {
        return next.run(req).await;
    }

    let path = req.uri().path();
    if is_public_probe(path) {
        return next.run(req).await;
    }

    let provided_key = match extract_api_key(&req) {
        Some(key) => key,
        None => {
            return (
                StatusCode::UNAUTHORIZED,
                axum::Json(serde_json::json!({
                    "error": "missing_api_key",
                    "message": "Provide your API key via X-API-Key header or Authorization: Bearer <key>"
                })),
            )
                .into_response()
        }
    };

    if !config.api_keys.contains(&provided_key) {
        return (
            StatusCode::FORBIDDEN,
            axum::Json(serde_json::json!({
                "error": "invalid_api_key",
                "message": "The provided API key is not valid"
            })),
        )
            .into_response();
    }

    next.run(req).await
}

fn is_public_probe(path: &str) -> bool {
    matches!(
        path,
        "/health" | "/readiness" | "/api/v1/health" | "/api/v1/readiness"
    )
}

#[cfg(test)]
mod tests {
    use axum::http::Method;

    use super::*;

    #[test]
    fn extract_api_key_from_x_api_key_header() {
        let req = Request::builder()
            .method(Method::GET)
            .uri("/api/v1/test")
            .header("x-api-key", "test-key-123")
            .body(Body::empty())
            .unwrap();

        assert_eq!(extract_api_key(&req), Some("test-key-123".to_string()));
    }

    #[test]
    fn extract_api_key_from_authorization_bearer() {
        let req = Request::builder()
            .method(Method::GET)
            .uri("/api/v1/test")
            .header("authorization", "Bearer my-secret-key")
            .body(Body::empty())
            .unwrap();

        assert_eq!(extract_api_key(&req), Some("my-secret-key".to_string()));
    }

    #[test]
    fn extract_api_key_returns_none_when_no_header() {
        let req = Request::builder()
            .method(Method::GET)
            .uri("/api/v1/test")
            .body(Body::empty())
            .unwrap();

        assert!(extract_api_key(&req).is_none());
    }
}
