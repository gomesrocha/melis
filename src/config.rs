//! Gateway configuration module.
//!
//! Defines all configuration structs for the Melis AI Gateway,
//! loaded from YAML files and environment variables.
//! Includes validation logic for required values and valid ranges.

use serde::Deserialize;
use std::path::PathBuf;
use std::time::Duration;

/// Errors that can occur during configuration loading or validation.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("Failed to read config file '{path}': {source}")]
    IoError {
        path: String,
        source: std::io::Error,
    },

    #[error("Failed to parse YAML config: {0}")]
    ParseError(#[from] serde_yaml::Error),

    #[error("Configuration validation failed:\n{}", .0.join("\n"))]
    ValidationError(Vec<String>),
}

/// Main gateway configuration.
#[derive(Debug, Clone, Deserialize)]
pub struct GatewayConfig {
    pub server: ServerConfig,
    pub redis: RedisConfig,
    #[serde(default)]
    pub providers: Vec<ProviderConfig>,
    #[serde(default)]
    pub rate_limit: RateLimitDefaults,
    #[serde(default)]
    pub compactor: CompactorConfig,
    #[serde(default)]
    pub circuit_breaker: CircuitBreakerConfig,
    #[serde(default)]
    pub observability: OtelConfig,
    #[serde(default)]
    pub auth: AuthConfig,
    #[serde(default = "default_routes_config_path")]
    pub routes_config_path: PathBuf,
}

fn default_routes_config_path() -> PathBuf {
    PathBuf::from("./routes.yaml")
}

/// HTTP server configuration.
#[derive(Debug, Clone, Deserialize)]
pub struct ServerConfig {
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default = "default_max_payload_size")]
    pub max_payload_size: usize,
    #[serde(default = "default_graceful_shutdown_timeout_secs")]
    pub graceful_shutdown_timeout_secs: u64,
    #[serde(default = "default_max_concurrent_connections")]
    pub max_concurrent_connections: usize,
}

impl ServerConfig {
    pub fn graceful_shutdown_timeout(&self) -> Duration {
        Duration::from_secs(self.graceful_shutdown_timeout_secs)
    }
}

fn default_host() -> String {
    "0.0.0.0".to_string()
}

fn default_port() -> u16 {
    8080
}

fn default_max_payload_size() -> usize {
    10 * 1024 * 1024 // 10MB
}

fn default_graceful_shutdown_timeout_secs() -> u64 {
    30
}

fn default_max_concurrent_connections() -> usize {
    5000
}

/// Provider endpoint configuration.
#[derive(Debug, Clone, Deserialize)]
pub struct ProviderConfig {
    pub id: String,
    pub provider_type: String,
    pub base_url: String,
    pub api_key: String,
    #[serde(default = "default_weight")]
    pub weight: u32,
    #[serde(default = "default_provider_timeout_secs")]
    pub timeout_secs: u64,
    #[serde(default)]
    pub models: Vec<String>,
}

impl ProviderConfig {
    pub fn timeout(&self) -> Duration {
        Duration::from_secs(self.timeout_secs)
    }
}

fn default_weight() -> u32 {
    1
}

fn default_provider_timeout_secs() -> u64 {
    30
}

/// Redis cluster configuration.
#[derive(Debug, Clone, Deserialize)]
pub struct RedisConfig {
    pub cluster_urls: Vec<String>,
    #[serde(default = "default_pool_size")]
    pub pool_size: usize,
    #[serde(default = "default_connect_timeout_secs")]
    pub connect_timeout_secs: u64,
    #[serde(default = "default_command_timeout_secs")]
    pub command_timeout_secs: u64,
}

impl RedisConfig {
    pub fn connect_timeout(&self) -> Duration {
        Duration::from_secs(self.connect_timeout_secs)
    }

    pub fn command_timeout(&self) -> Duration {
        Duration::from_secs(self.command_timeout_secs)
    }
}

fn default_pool_size() -> usize {
    10
}

fn default_connect_timeout_secs() -> u64 {
    5
}

fn default_command_timeout_secs() -> u64 {
    2
}

/// Rate limit defaults.
#[derive(Debug, Clone, Deserialize)]
pub struct RateLimitDefaults {
    #[serde(default = "default_burst_capacity")]
    pub burst_capacity: u64,
    #[serde(default = "default_refill_rate")]
    pub refill_rate: f64,
}

impl Default for RateLimitDefaults {
    fn default() -> Self {
        Self {
            burst_capacity: default_burst_capacity(),
            refill_rate: default_refill_rate(),
        }
    }
}

fn default_burst_capacity() -> u64 {
    100
}

fn default_refill_rate() -> f64 {
    10.0
}

/// Context compactor configuration.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct CompactorConfig {
    #[serde(default = "default_token_threshold")]
    pub token_threshold: usize,
    #[serde(default = "default_max_history_messages")]
    pub max_history_messages: usize,
    #[serde(default)]
    pub stop_words: Vec<String>,
    #[serde(default = "default_tokenizer_name")]
    pub tokenizer_name: String,
}

impl Default for CompactorConfig {
    fn default() -> Self {
        Self {
            token_threshold: default_token_threshold(),
            max_history_messages: default_max_history_messages(),
            stop_words: Vec::new(),
            tokenizer_name: default_tokenizer_name(),
        }
    }
}

fn default_token_threshold() -> usize {
    4096
}

fn default_max_history_messages() -> usize {
    20
}

fn default_tokenizer_name() -> String {
    "cl100k_base".to_string()
}

/// Circuit breaker configuration.
#[derive(Debug, Clone, Deserialize)]
pub struct CircuitBreakerConfig {
    #[serde(default = "default_failure_threshold")]
    pub failure_threshold_percent: f64,
    #[serde(default = "default_window_duration_secs")]
    pub window_duration_secs: u64,
    #[serde(default = "default_min_requests")]
    pub min_requests_in_window: u64,
    #[serde(default = "default_open_ttl_secs")]
    pub open_ttl_secs: u64,
    #[serde(default = "default_max_ttl_secs")]
    pub max_ttl_secs: u64,
    #[serde(default = "default_backoff_factor")]
    pub backoff_factor: f64,
}

impl CircuitBreakerConfig {
    pub fn window_duration(&self) -> Duration {
        Duration::from_secs(self.window_duration_secs)
    }

    pub fn open_ttl(&self) -> Duration {
        Duration::from_secs(self.open_ttl_secs)
    }

    pub fn max_ttl(&self) -> Duration {
        Duration::from_secs(self.max_ttl_secs)
    }
}

impl Default for CircuitBreakerConfig {
    fn default() -> Self {
        Self {
            failure_threshold_percent: default_failure_threshold(),
            window_duration_secs: default_window_duration_secs(),
            min_requests_in_window: default_min_requests(),
            open_ttl_secs: default_open_ttl_secs(),
            max_ttl_secs: default_max_ttl_secs(),
            backoff_factor: default_backoff_factor(),
        }
    }
}

fn default_failure_threshold() -> f64 {
    50.0
}

fn default_window_duration_secs() -> u64 {
    60
}

fn default_min_requests() -> u64 {
    5
}

fn default_open_ttl_secs() -> u64 {
    30
}

fn default_max_ttl_secs() -> u64 {
    300
}

fn default_backoff_factor() -> f64 {
    2.0
}

/// OpenTelemetry configuration.
#[derive(Debug, Clone, Deserialize)]
pub struct OtelConfig {
    #[serde(default = "default_otlp_endpoint")]
    pub otlp_endpoint: String,
    #[serde(default = "default_service_name")]
    pub service_name: String,
    #[serde(default = "default_otel_enabled")]
    pub enabled: bool,
}

impl Default for OtelConfig {
    fn default() -> Self {
        Self {
            otlp_endpoint: default_otlp_endpoint(),
            service_name: default_service_name(),
            enabled: default_otel_enabled(),
        }
    }
}

fn default_otlp_endpoint() -> String {
    "http://localhost:4317".to_string()
}

fn default_service_name() -> String {
    "melis-gateway".to_string()
}

fn default_otel_enabled() -> bool {
    false
}

/// Authentication configuration.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct AuthConfig {
    #[serde(default)]
    pub api_keys: Vec<ApiKeyEntry>,
    #[serde(default = "default_auth_enabled")]
    pub enabled: bool,
}

/// API key entry for authentication.
#[derive(Debug, Clone, Deserialize)]
pub struct ApiKeyEntry {
    pub key: String,
    pub client_id: String,
    #[serde(default)]
    pub allowed_models: Vec<String>,
    #[serde(default)]
    pub rate_limit: Option<ClientRateLimit>,
}

/// Per-client rate limit override.
#[derive(Debug, Clone, Deserialize)]
pub struct ClientRateLimit {
    pub burst_capacity: u64,
    pub refill_rate: f64,
}

fn default_auth_enabled() -> bool {
    false
}

// ─── Loading & Validation ─────────────────────────────────────────────────────

impl GatewayConfig {
    /// Load configuration from a YAML file with environment variable overrides.
    ///
    /// Resolution order:
    /// 1. YAML file at `path` (defaults to `./config.yaml`)
    /// 2. Environment variable overrides (where supported)
    ///
    /// Environment variables:
    /// - `MELIS_ROUTES_CONFIG`: Override `routes_config_path` (default: `./routes.yaml`)
    /// - `MELIS_SERVER_HOST`: Override server host
    /// - `MELIS_SERVER_PORT`: Override server port
    pub fn load(path: Option<&str>) -> Result<Self, ConfigError> {
        let config_path = path.unwrap_or("./config.yaml");

        let contents = std::fs::read_to_string(config_path).map_err(|e| {
            ConfigError::IoError {
                path: config_path.to_string(),
                source: e,
            }
        })?;

        let mut config: GatewayConfig =
            serde_yaml::from_str(&contents)?;

        // Apply environment variable overrides
        config.apply_env_overrides();

        // Validate configuration
        config.validate()?;

        Ok(config)
    }

    /// Load configuration from a YAML string (useful for testing).
    pub fn from_yaml(yaml: &str) -> Result<Self, ConfigError> {
        let mut config: GatewayConfig = serde_yaml::from_str(yaml)?;
        config.apply_env_overrides();
        config.validate()?;
        Ok(config)
    }

    /// Apply environment variable overrides to the loaded config.
    fn apply_env_overrides(&mut self) {
        if let Ok(val) = std::env::var("MELIS_ROUTES_CONFIG") {
            self.routes_config_path = PathBuf::from(val);
        }

        if let Ok(val) = std::env::var("MELIS_SERVER_HOST") {
            self.server.host = val;
        }

        if let Ok(val) = std::env::var("MELIS_SERVER_PORT") {
            if let Ok(port) = val.parse::<u16>() {
                self.server.port = port;
            }
        }

        if let Ok(val) = std::env::var("MELIS_OTLP_ENDPOINT") {
            self.observability.otlp_endpoint = val;
        }
    }

    /// Validate the configuration, returning descriptive errors for invalid values.
    fn validate(&self) -> Result<(), ConfigError> {
        let mut errors: Vec<String> = Vec::new();

        // Server validation
        if self.server.port == 0 {
            errors.push("server.port must be > 0".to_string());
        }
        if self.server.max_payload_size == 0 {
            errors.push("server.max_payload_size must be > 0".to_string());
        }
        if self.server.max_concurrent_connections == 0 {
            errors.push(
                "server.max_concurrent_connections must be > 0".to_string(),
            );
        }

        // Redis validation
        if self.redis.cluster_urls.is_empty() {
            errors.push(
                "redis.cluster_urls must contain at least one URL".to_string(),
            );
        }
        for (i, url) in self.redis.cluster_urls.iter().enumerate() {
            if url.trim().is_empty() {
                errors.push(format!(
                    "redis.cluster_urls[{}] must not be empty",
                    i
                ));
            }
        }
        if self.redis.pool_size == 0 {
            errors.push("redis.pool_size must be > 0".to_string());
        }

        // Provider validation
        for (i, provider) in self.providers.iter().enumerate() {
            if provider.id.trim().is_empty() {
                errors.push(format!("providers[{}].id must not be empty", i));
            }
            if provider.base_url.trim().is_empty() {
                errors.push(format!(
                    "providers[{}].base_url must not be empty",
                    i
                ));
            }
            if provider.api_key.trim().is_empty() {
                errors.push(format!(
                    "providers[{}].api_key must not be empty",
                    i
                ));
            }
            if provider.weight == 0 {
                errors.push(format!(
                    "providers[{}].weight must be > 0",
                    i
                ));
            }
            let valid_types = [
                "openai",
                "anthropic",
                "google_vertex_ai",
                "oci_genai",
                "ollama",
            ];
            if !valid_types.contains(&provider.provider_type.as_str()) {
                errors.push(format!(
                    "providers[{}].provider_type '{}' is invalid. Valid values: {}",
                    i,
                    provider.provider_type,
                    valid_types.join(", ")
                ));
            }
        }

        // Compactor validation: token_threshold between 512–128000
        if self.compactor.token_threshold < 512
            || self.compactor.token_threshold > 128_000
        {
            errors.push(format!(
                "compactor.token_threshold must be between 512 and 128000, got {}",
                self.compactor.token_threshold
            ));
        }
        if self.compactor.tokenizer_name.trim().is_empty() {
            errors.push(
                "compactor.tokenizer_name must not be empty".to_string(),
            );
        }

        // Rate limit validation
        if self.rate_limit.burst_capacity == 0 {
            errors.push(
                "rate_limit.burst_capacity must be > 0".to_string(),
            );
        }
        if self.rate_limit.refill_rate <= 0.0 {
            errors.push(
                "rate_limit.refill_rate must be > 0.0".to_string(),
            );
        }

        // Circuit breaker validation
        if self.circuit_breaker.failure_threshold_percent <= 0.0
            || self.circuit_breaker.failure_threshold_percent > 100.0
        {
            errors.push(format!(
                "circuit_breaker.failure_threshold_percent must be between 0 (exclusive) and 100 (inclusive), got {}",
                self.circuit_breaker.failure_threshold_percent
            ));
        }
        if self.circuit_breaker.window_duration_secs == 0 {
            errors.push(
                "circuit_breaker.window_duration_secs must be > 0".to_string(),
            );
        }
        if self.circuit_breaker.open_ttl_secs == 0 {
            errors.push(
                "circuit_breaker.open_ttl_secs must be > 0".to_string(),
            );
        }
        if self.circuit_breaker.max_ttl_secs
            < self.circuit_breaker.open_ttl_secs
        {
            errors.push(format!(
                "circuit_breaker.max_ttl_secs ({}) must be >= open_ttl_secs ({})",
                self.circuit_breaker.max_ttl_secs,
                self.circuit_breaker.open_ttl_secs
            ));
        }
        if self.circuit_breaker.backoff_factor < 1.0 {
            errors.push(format!(
                "circuit_breaker.backoff_factor must be >= 1.0, got {}",
                self.circuit_breaker.backoff_factor
            ));
        }

        // Auth validation
        if self.auth.enabled && self.auth.api_keys.is_empty() {
            errors.push(
                "auth.api_keys must contain at least one entry when auth is enabled".to_string(),
            );
        }
        for (i, key_entry) in self.auth.api_keys.iter().enumerate() {
            if key_entry.key.trim().is_empty() {
                errors.push(format!(
                    "auth.api_keys[{}].key must not be empty",
                    i
                ));
            }
            if key_entry.client_id.trim().is_empty() {
                errors.push(format!(
                    "auth.api_keys[{}].client_id must not be empty",
                    i
                ));
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(ConfigError::ValidationError(errors))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal_valid_yaml() -> String {
        r#"
server:
  host: "0.0.0.0"
  port: 8080
redis:
  cluster_urls:
    - "redis://localhost:6379"
providers:
  - id: "openai-1"
    provider_type: "openai"
    base_url: "https://api.openai.com"
    api_key: "sk-test-key"
    weight: 1
    models:
      - "gpt-4o"
"#
        .to_string()
    }

    #[test]
    fn test_load_minimal_valid_config() {
        let config = GatewayConfig::from_yaml(&minimal_valid_yaml()).unwrap();
        assert_eq!(config.server.host, "0.0.0.0");
        assert_eq!(config.server.port, 8080);
        assert_eq!(config.redis.cluster_urls.len(), 1);
        assert_eq!(config.providers.len(), 1);
        assert_eq!(config.providers[0].id, "openai-1");
        assert_eq!(
            config.routes_config_path,
            PathBuf::from("./routes.yaml")
        );
    }

    #[test]
    fn test_defaults_applied() {
        let config = GatewayConfig::from_yaml(&minimal_valid_yaml()).unwrap();
        // Compactor defaults
        assert_eq!(config.compactor.token_threshold, 4096);
        assert_eq!(config.compactor.tokenizer_name, "cl100k_base");
        // Rate limit defaults
        assert_eq!(config.rate_limit.burst_capacity, 100);
        assert_eq!(config.rate_limit.refill_rate, 10.0);
        // Circuit breaker defaults
        assert_eq!(config.circuit_breaker.failure_threshold_percent, 50.0);
        assert_eq!(config.circuit_breaker.window_duration_secs, 60);
        assert_eq!(config.circuit_breaker.backoff_factor, 2.0);
        // Server defaults
        assert_eq!(config.server.max_payload_size, 10 * 1024 * 1024);
        assert_eq!(config.server.max_concurrent_connections, 5000);
    }

    #[test]
    fn test_validation_token_threshold_too_low() {
        let yaml = r#"
server:
  host: "0.0.0.0"
  port: 8080
redis:
  cluster_urls:
    - "redis://localhost:6379"
compactor:
  token_threshold: 100
"#;
        let err = GatewayConfig::from_yaml(yaml).unwrap_err();
        match err {
            ConfigError::ValidationError(errors) => {
                assert!(errors.iter().any(|e| e.contains("token_threshold")));
            }
            _ => panic!("Expected ValidationError, got {:?}", err),
        }
    }

    #[test]
    fn test_validation_token_threshold_too_high() {
        let yaml = r#"
server:
  host: "0.0.0.0"
  port: 8080
redis:
  cluster_urls:
    - "redis://localhost:6379"
compactor:
  token_threshold: 200000
"#;
        let err = GatewayConfig::from_yaml(yaml).unwrap_err();
        match err {
            ConfigError::ValidationError(errors) => {
                assert!(errors.iter().any(|e| e.contains("token_threshold")));
            }
            _ => panic!("Expected ValidationError, got {:?}", err),
        }
    }

    #[test]
    fn test_validation_empty_redis_urls() {
        let yaml = r#"
server:
  host: "0.0.0.0"
  port: 8080
redis:
  cluster_urls: []
"#;
        let err = GatewayConfig::from_yaml(yaml).unwrap_err();
        match err {
            ConfigError::ValidationError(errors) => {
                assert!(errors.iter().any(|e| e.contains("cluster_urls")));
            }
            _ => panic!("Expected ValidationError, got {:?}", err),
        }
    }

    #[test]
    fn test_validation_invalid_provider_type() {
        let yaml = r#"
server:
  host: "0.0.0.0"
  port: 8080
redis:
  cluster_urls:
    - "redis://localhost:6379"
providers:
  - id: "bad-provider"
    provider_type: "invalid_type"
    base_url: "https://example.com"
    api_key: "sk-test"
    weight: 1
"#;
        let err = GatewayConfig::from_yaml(yaml).unwrap_err();
        match err {
            ConfigError::ValidationError(errors) => {
                assert!(errors.iter().any(|e| e.contains("provider_type")));
                assert!(errors.iter().any(|e| e.contains("invalid_type")));
            }
            _ => panic!("Expected ValidationError, got {:?}", err),
        }
    }

    #[test]
    fn test_validation_circuit_breaker_threshold_out_of_range() {
        let yaml = r#"
server:
  host: "0.0.0.0"
  port: 8080
redis:
  cluster_urls:
    - "redis://localhost:6379"
circuit_breaker:
  failure_threshold_percent: 150.0
"#;
        let err = GatewayConfig::from_yaml(yaml).unwrap_err();
        match err {
            ConfigError::ValidationError(errors) => {
                assert!(errors
                    .iter()
                    .any(|e| e.contains("failure_threshold_percent")));
            }
            _ => panic!("Expected ValidationError, got {:?}", err),
        }
    }

    #[test]
    fn test_validation_max_ttl_less_than_open_ttl() {
        let yaml = r#"
server:
  host: "0.0.0.0"
  port: 8080
redis:
  cluster_urls:
    - "redis://localhost:6379"
circuit_breaker:
  open_ttl_secs: 60
  max_ttl_secs: 30
"#;
        let err = GatewayConfig::from_yaml(yaml).unwrap_err();
        match err {
            ConfigError::ValidationError(errors) => {
                assert!(errors.iter().any(|e| e.contains("max_ttl_secs")));
            }
            _ => panic!("Expected ValidationError, got {:?}", err),
        }
    }

    #[test]
    fn test_env_override_routes_config_path() {
        std::env::set_var("MELIS_ROUTES_CONFIG", "/tmp/custom-routes.yaml");
        let config = GatewayConfig::from_yaml(&minimal_valid_yaml()).unwrap();
        assert_eq!(
            config.routes_config_path,
            PathBuf::from("/tmp/custom-routes.yaml")
        );
        std::env::remove_var("MELIS_ROUTES_CONFIG");
    }

    #[test]
    fn test_env_override_server_port() {
        std::env::set_var("MELIS_SERVER_PORT", "9090");
        let config = GatewayConfig::from_yaml(&minimal_valid_yaml()).unwrap();
        assert_eq!(config.server.port, 9090);
        std::env::remove_var("MELIS_SERVER_PORT");
    }

    #[test]
    fn test_load_from_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        std::fs::write(&path, minimal_valid_yaml()).unwrap();
        let config =
            GatewayConfig::load(Some(path.to_str().unwrap())).unwrap();
        assert_eq!(config.server.port, 8080);
    }

    #[test]
    fn test_load_missing_file() {
        let err =
            GatewayConfig::load(Some("/nonexistent/config.yaml")).unwrap_err();
        assert!(matches!(err, ConfigError::IoError { .. }));
    }
}
