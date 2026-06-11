#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub port: u16,
    pub log_level: String,
    pub max_file_size: usize,
    pub max_dpi: u32,
    pub max_pages: usize,
    /// Comma-separated list of valid API keys. Empty means open access mode.
    pub api_keys: Vec<String>,
    /// Maximum requests per minute per key. Zero disables rate limiting.
    pub rate_limit_per_min: u32,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            port: 8080,
            log_level: "info".to_string(),
            max_file_size: 52_428_800,
            max_dpi: 600,
            max_pages: 200,
            api_keys: Vec::new(),
            rate_limit_per_min: 60,
        }
    }
}

impl ServerConfig {
    pub fn from_env() -> Self {
        let mut cfg = Self::default();

        if let Ok(value) = std::env::var("OXIDE_PORT") {
            if let Ok(port) = value.parse::<u16>() {
                cfg.port = port;
            }
        }

        if let Ok(value) = std::env::var("OXIDE_LOG_LEVEL") {
            cfg.log_level = value;
        }

        if let Ok(value) = std::env::var("OXIDE_MAX_FILE_SIZE") {
            if let Ok(max_file_size) = value.parse::<usize>() {
                cfg.max_file_size = max_file_size;
            }
        }

        if let Ok(value) = std::env::var("OXIDE_MAX_DPI") {
            if let Ok(max_dpi) = value.parse::<u32>() {
                cfg.max_dpi = max_dpi.min(600);
            }
        }

        if let Ok(value) = std::env::var("OXIDE_MAX_PAGES") {
            if let Ok(max_pages) = value.parse::<usize>() {
                cfg.max_pages = max_pages;
            }
        }

        if let Ok(value) = std::env::var("OXIDE_API_KEYS") {
            cfg.api_keys = value
                .split(',')
                .map(|key| key.trim().to_string())
                .filter(|key| !key.is_empty())
                .collect();
        }

        if let Ok(value) = std::env::var("OXIDE_RATE_LIMIT_PER_MIN") {
            if let Ok(rate_limit_per_min) = value.parse::<u32>() {
                cfg.rate_limit_per_min = rate_limit_per_min;
            }
        }

        cfg
    }
}

pub static CONFIG: std::sync::OnceLock<ServerConfig> = std::sync::OnceLock::new();

pub fn get_config() -> &'static ServerConfig {
    CONFIG.get_or_init(ServerConfig::default)
}
