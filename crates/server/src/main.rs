use std::sync::Arc;
use std::time::Duration;

use tokio::net::TcpListener;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = oxide_server::config::ServerConfig::from_env();
    let port = config.port;
    let log_level = config.log_level.clone();

    let env_filter = EnvFilter::try_new(&log_level).unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(env_filter).init();

    // Fail-closed startup: refuse to come up unauthenticated unless the
    // operator has explicitly opted into the dev mode. This turns a silent
    // misconfiguration (forgot to set keys) into a loud, immediate failure
    // rather than an open server.
    if let Err(msg) = config.validate() {
        tracing::error!("{}", msg);
        eprintln!("FATAL: {}", msg);
        std::process::exit(1);
    }

    if !config.auth_enforced() {
        tracing::warn!(
            "OXIDE_ALLOW_UNAUTHENTICATED is set: starting WITHOUT API-key \
             authentication. Every data endpoint is open. This is intended for \
             local development ONLY — set OXIDE_API_KEYS before deploying."
        );
    }
    if config.cors_allow_any {
        tracing::warn!(
            "OXIDE_CORS_ALLOW_ANY is set: CORS will accept ANY origin. Intended \
             for local development ONLY — set OXIDE_CORS_ALLOWED_ORIGINS in prod."
        );
    }

    let _ = oxide_server::config::CONFIG.set(config);
    let config = oxide_server::config::get_config();

    // Build the app and start the rate-limiter cleanup task on the same limiter
    // the app uses, so per-key buckets don't accumulate unbounded.
    let limiter = Arc::new(oxide_server::rate_limit::RateLimiter::new(
        config.rate_limit_per_min,
    ));
    let _cleanup = limiter.spawn_cleanup(Duration::from_secs(60));
    let app = oxide_server::app::create_app_with_limiter(config.clone(), limiter);

    let addr = std::net::SocketAddr::from(([0, 0, 0, 0], port));
    let listener = TcpListener::bind(&addr).await?;

    tracing::info!("Oxide listening on {}", addr);
    // axum::serve handles SIGTERM gracefully under the tokio runtime.
    axum::serve(listener, app).await?;

    Ok(())
}
