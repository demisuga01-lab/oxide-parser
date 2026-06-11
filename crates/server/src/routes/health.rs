pub async fn health() -> &'static str {
    "ok"
}

pub async fn readiness() -> axum::Json<serde_json::Value> {
    axum::Json(serde_json::json!({
        "status": "ready",
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

pub async fn version() -> axum::Json<serde_json::Value> {
    axum::Json(serde_json::json!({
        "product": "Oxide",
        "version": env!("CARGO_PKG_VERSION"),
        "engine": oxide_engine::ENGINE_VERSION,
    }))
}
