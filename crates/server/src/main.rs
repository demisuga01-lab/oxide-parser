use tokio::net::TcpListener;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = oxide_server::config::ServerConfig::from_env();
    let port = config.port;
    let log_level = config.log_level.clone();
    let _ = oxide_server::config::CONFIG.set(config);

    let env_filter = EnvFilter::try_new(log_level).unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(env_filter).init();

    let addr = std::net::SocketAddr::from(([0, 0, 0, 0], port));
    let listener = TcpListener::bind(&addr).await?;

    tracing::info!("Oxide listening on {}", addr);
    // axum::serve handles SIGTERM gracefully under the tokio runtime.
    axum::serve(listener, oxide_server::app::create_app()).await?;

    Ok(())
}
