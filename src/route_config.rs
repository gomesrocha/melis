//! Route configuration module.
//!
//! Defines data models for the `routes.yaml` configuration file,
//! including route definitions, provider types, token optimization
//! strategies, and YAML parsing via serde_yaml.
//!
//! Also provides `RouteResolver` for O(1) route resolution by
//! exact (path, method) matching, and `effective_token_config`
//! for resolving per-route vs global compactor configuration.

use arc_swap::ArcSwap;
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use serde::Deserialize;
use std::collections::HashMap;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

use crate::config::CompactorConfig;

/// Known built-in provider names.
const KNOWN_PROVIDERS: &[&str] = &[
    "openai",
    "anthropic",
    "google_vertex_ai",
    "oci_genai",
    "ollama",
];

/// Errors that can occur during route configuration loading and validation.
#[derive(Debug, thiserror::Error)]
pub enum RoutesConfigError {
    #[error("Failed to read routes config file: {0}")]
    IoError(#[from] std::io::Error),

    #[error("Failed to parse routes YAML: {0}")]
    ParseError(#[from] serde_yaml::Error),

    #[error("Campo obrigatório ausente na rota {route_index}: {field}")]
    MissingField { route_index: usize, field: String },

    #[error("Provedor inválido '{provider}' na rota {route_index}. Valores aceitos: openai, anthropic, google_vertex_ai, oci_genai, ollama ou custom_providers registrados")]
    InvalidProvider { route_index: usize, provider: String },

    #[error("Estratégia inválida '{strategy}' na rota {route_index}. Valores aceitos: adaptive_trimming, sliding_window, none")]
    InvalidStrategy { route_index: usize, strategy: String },

    #[error("Rota duplicada: {path} {method} (rotas {first} e {second})")]
    DuplicateRoute {
        path: String,
        method: String,
        first: usize,
        second: usize,
    },
}

/// Supported LLM provider types.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ProviderType {
    Openai,
    Anthropic,
    GoogleVertexAi,
    OciGenai,
    Ollama,
    #[serde(untagged)]
    Custom(String),
}

/// Token optimization strategies per route.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum TokenOptimizationStrategy {
    AdaptiveTrimming,
    SlidingWindow,
    None,
}

/// Token optimization configuration per route.
#[derive(Debug, Clone, Deserialize)]
pub struct TokenOptimizationConfig {
    pub strategy: TokenOptimizationStrategy,
    #[serde(default = "default_max_history_messages")]
    pub max_history_messages: usize,
    #[serde(default = "default_compress_above_tokens")]
    pub compress_above_tokens: usize,
    #[serde(default = "default_local_tokenizer")]
    pub local_tokenizer: String,
}

fn default_max_history_messages() -> usize {
    20
}

fn default_compress_above_tokens() -> usize {
    4096
}

fn default_local_tokenizer() -> String {
    "cl100k_base".to_string()
}

/// A weighted provider for load balancing within a route.
#[derive(Debug, Clone, Deserialize)]
pub struct WeightedProvider {
    pub name: String,
    pub weight: u32,
    pub model: String,
}

/// Definition of an individual route.
#[derive(Debug, Clone, Deserialize)]
pub struct RouteDefinition {
    pub path: String,
    pub method: String,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub providers: Option<Vec<WeightedProvider>>,
    pub token_optimization: Option<TokenOptimizationConfig>,
}

/// Custom provider registration.
#[derive(Debug, Clone, Deserialize)]
pub struct CustomProviderDef {
    pub name: String,
    pub base_url: String,
    pub api_format: String,
}

/// Root structure of the routes.yaml file.
#[derive(Debug, Clone, Deserialize)]
pub struct RouteConfigFile {
    pub routes: Vec<RouteDefinition>,
    #[serde(default)]
    pub custom_providers: Vec<CustomProviderDef>,
}

/// Load and parse a routes.yaml file from the given path.
pub fn load_routes(path: &Path) -> Result<RouteConfigFile, RoutesConfigError> {
    let contents = std::fs::read_to_string(path)?;
    let config: RouteConfigFile = serde_yaml::from_str(&contents)?;
    Ok(config)
}

/// Validate a parsed route configuration.
///
/// Checks:
/// - Each route has non-empty `path` and `method`
/// - Each route has either `provider` or `providers` (at least one)
/// - Provider names are in the known list or registered as custom_providers
/// - Token optimization strategy values are valid (handled by serde, but validated for providers in `providers` list)
/// - No duplicate routes (same path + method combination)
///
/// Returns `Ok(())` if valid, or `Err(Vec<RoutesConfigError>)` with all validation errors found.
pub fn validate(config: &RouteConfigFile) -> Result<(), Vec<RoutesConfigError>> {
    let mut errors: Vec<RoutesConfigError> = Vec::new();

    // Build set of valid provider names: known + custom_providers
    let custom_names: HashSet<&str> = config
        .custom_providers
        .iter()
        .map(|cp| cp.name.as_str())
        .collect();

    // Track seen (path, method) combinations for duplicate detection
    let mut seen_routes: HashMap<(String, String), usize> = HashMap::new();

    for (idx, route) in config.routes.iter().enumerate() {
        // Validate path is non-empty
        if route.path.trim().is_empty() {
            errors.push(RoutesConfigError::MissingField {
                route_index: idx,
                field: "path".to_string(),
            });
        }

        // Validate method is non-empty
        if route.method.trim().is_empty() {
            errors.push(RoutesConfigError::MissingField {
                route_index: idx,
                field: "method".to_string(),
            });
        }

        // Validate provider or providers is present
        let has_provider = route
            .provider
            .as_ref()
            .map(|p| !p.trim().is_empty())
            .unwrap_or(false);
        let has_providers = route
            .providers
            .as_ref()
            .map(|ps| !ps.is_empty())
            .unwrap_or(false);

        if !has_provider && !has_providers {
            errors.push(RoutesConfigError::MissingField {
                route_index: idx,
                field: "provider ou providers".to_string(),
            });
        }

        // Validate provider value against known + custom providers
        if let Some(ref provider) = route.provider {
            if !provider.trim().is_empty()
                && !KNOWN_PROVIDERS.contains(&provider.as_str())
                && !custom_names.contains(provider.as_str())
            {
                errors.push(RoutesConfigError::InvalidProvider {
                    route_index: idx,
                    provider: provider.clone(),
                });
            }
        }

        // Validate provider names within multi-provider list
        if let Some(ref providers) = route.providers {
            for wp in providers {
                if !KNOWN_PROVIDERS.contains(&wp.name.as_str())
                    && !custom_names.contains(wp.name.as_str())
                {
                    errors.push(RoutesConfigError::InvalidProvider {
                        route_index: idx,
                        provider: wp.name.clone(),
                    });
                }
            }
        }

        // Detect duplicate routes (same path + method)
        if !route.path.trim().is_empty() && !route.method.trim().is_empty() {
            let key = (route.path.clone(), route.method.to_uppercase());
            if let Some(&first_idx) = seen_routes.get(&key) {
                errors.push(RoutesConfigError::DuplicateRoute {
                    path: route.path.clone(),
                    method: route.method.clone(),
                    first: first_idx,
                    second: idx,
                });
            } else {
                seen_routes.insert(key, idx);
            }
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

/// Route resolver with O(1) lookup by (path, method) combination.
///
/// Internally uses a HashMap keyed by `(path, method_uppercase)` for
/// case-sensitive path matching and case-insensitive method matching.
/// The HashMap value is the index into the `RouteConfigFile.routes` Vec.
#[derive(Debug)]
pub struct RouteResolver {
    config: RouteConfigFile,
    /// Maps (path, METHOD_UPPERCASE) -> index in config.routes
    lookup: HashMap<(String, String), usize>,
}

impl RouteResolver {
    /// Create a new RouteResolver from a parsed RouteConfigFile.
    ///
    /// Builds the internal HashMap for O(1) lookups.
    pub fn new(config: RouteConfigFile) -> Self {
        let mut lookup = HashMap::with_capacity(config.routes.len());
        for (idx, route) in config.routes.iter().enumerate() {
            let key = (route.path.clone(), route.method.to_uppercase());
            lookup.insert(key, idx);
        }
        RouteResolver { config, lookup }
    }

    /// Resolve a route by exact path and method match.
    ///
    /// Path matching is case-sensitive. Method matching is case-insensitive.
    /// Returns `None` if no route matches the given path + method combination.
    pub fn resolve_route(&self, path: &str, method: &str) -> Option<&RouteDefinition> {
        let key = (path.to_string(), method.to_uppercase());
        self.lookup.get(&key).map(|&idx| &self.config.routes[idx])
    }

    /// Returns the effective compactor configuration for a route.
    ///
    /// If the route has `token_optimization` defined, creates a `CompactorConfig`
    /// from those per-route values. Otherwise, returns the global `CompactorConfig`.
    pub fn effective_token_config(
        &self,
        route: &RouteDefinition,
        global: &CompactorConfig,
    ) -> CompactorConfig {
        match &route.token_optimization {
            Some(opt) => CompactorConfig {
                token_threshold: opt.compress_above_tokens,
                max_history_messages: opt.max_history_messages,
                stop_words: global.stop_words.clone(),
                tokenizer_name: opt.local_tokenizer.clone(),
            },
            None => global.clone(),
        }
    }

    /// Get a reference to the underlying route config.
    pub fn config(&self) -> &RouteConfigFile {
        &self.config
    }
}

/// Manages the route configuration with hot-reload support.
///
/// Uses `ArcSwap<RouteResolver>` for lock-free reading and `notify::RecommendedWatcher`
/// to detect modifications to the config file. When changes are detected, the new config
/// is loaded, parsed, and validated. If valid, it's atomically swapped in; if invalid,
/// the previous config is kept and a warning is emitted.
pub struct RouteConfigManager {
    config: Arc<ArcSwap<RouteResolver>>,
    _watcher_handle: tokio::task::JoinHandle<()>,
}

impl RouteConfigManager {
    /// Creates a new RouteConfigManager.
    ///
    /// Loads and validates the initial configuration. If validation fails, panics with
    /// a descriptive message (refusing to start with an invalid config).
    ///
    /// Starts a file watcher on the parent directory of the config file and spawns a
    /// Tokio task to process file change events with debouncing (~200ms).
    pub fn new(config_path: PathBuf) -> Self {
        // 1. Load and validate initial config (fail = refuse to start)
        let config_file = load_routes(&config_path).unwrap_or_else(|e| {
            panic!(
                "Failed to load initial route config from '{}': {}",
                config_path.display(),
                e
            );
        });

        if let Err(errors) = validate(&config_file) {
            let error_msgs: Vec<String> = errors.iter().map(|e| e.to_string()).collect();
            panic!(
                "Route config validation failed for '{}'. Errors:\n{}",
                config_path.display(),
                error_msgs.join("\n")
            );
        }

        let resolver = RouteResolver::new(config_file);
        let config = Arc::new(ArcSwap::from_pointee(resolver));

        // 2. Set up file watcher
        let (tx, rx) = mpsc::channel::<()>(16);

        let watch_path = config_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();
        let watched_file = config_path.clone();

        let mut watcher = notify::recommended_watcher(move |res: Result<Event, notify::Error>| {
            if let Ok(event) = res {
                // Only react to modifications/creates that affect our config file
                match event.kind {
                    EventKind::Modify(_) | EventKind::Create(_) => {
                        let is_our_file = event.paths.iter().any(|p| p == &watched_file);
                        if is_our_file {
                            // Non-blocking send - if channel is full, a reload is already pending
                            let _ = tx.try_send(());
                        }
                    }
                    _ => {}
                }
            }
        })
        .unwrap_or_else(|e| {
            panic!(
                "Failed to create file watcher for route config: {}",
                e
            );
        });

        watcher
            .watch(&watch_path, RecursiveMode::NonRecursive)
            .unwrap_or_else(|e| {
                panic!(
                    "Failed to watch directory '{}' for route config changes: {}",
                    watch_path.display(),
                    e
                );
            });

        // 3. Spawn Tokio task to process file change events with debouncing
        let config_clone = Arc::clone(&config);
        let reload_path = config_path;
        let watcher_handle = tokio::spawn(Self::reload_loop(
            rx,
            config_clone,
            reload_path,
            watcher,
        ));

        RouteConfigManager {
            config,
            _watcher_handle: watcher_handle,
        }
    }

    /// Returns the current RouteResolver (lock-free via ArcSwap).
    pub fn current(&self) -> arc_swap::Guard<Arc<RouteResolver>> {
        self.config.load()
    }

    /// Creates a RouteConfigManager with an empty config for testing purposes.
    ///
    /// This does not start a file watcher and is suitable only for unit tests
    /// that need an AppState but don't exercise route config behavior.
    #[cfg(test)]
    pub fn new_for_test() -> Self {
        let empty_config = RouteConfigFile {
            routes: vec![],
            custom_providers: vec![],
        };
        let resolver = RouteResolver::new(empty_config);
        let config = Arc::new(ArcSwap::from_pointee(resolver));
        let watcher_handle = tokio::spawn(async {});
        RouteConfigManager {
            config,
            _watcher_handle: watcher_handle,
        }
    }

    /// Background task that processes file change events with debouncing.
    ///
    /// Waits ~200ms after receiving an event before reloading, to avoid
    /// double reloads from editors that write-then-rename.
    async fn reload_loop(
        mut rx: mpsc::Receiver<()>,
        config: Arc<ArcSwap<RouteResolver>>,
        config_path: PathBuf,
        _watcher: RecommendedWatcher, // Keep watcher alive for the lifetime of this task
    ) {
        loop {
            // Wait for a file change event
            if rx.recv().await.is_none() {
                // Channel closed, stop the loop
                break;
            }

            // Debounce: wait 200ms and drain any additional events that arrived
            tokio::time::sleep(Duration::from_millis(200)).await;
            while rx.try_recv().is_ok() {
                // Drain pending events
            }

            // Attempt reload
            Self::try_reload(&config, &config_path);
        }
    }

    /// Attempts to reload the configuration from disk.
    ///
    /// If the new config is valid, performs an atomic swap. If invalid,
    /// keeps the previous config and logs a warning.
    fn try_reload(config: &Arc<ArcSwap<RouteResolver>>, config_path: &Path) {
        tracing::info!("Detected route config file change, attempting reload...");

        match load_routes(config_path) {
            Ok(new_config_file) => match validate(&new_config_file) {
                Ok(()) => {
                    let new_resolver = RouteResolver::new(new_config_file);
                    config.store(Arc::new(new_resolver));
                    tracing::info!("Route config reloaded successfully from '{}'", config_path.display());
                }
                Err(errors) => {
                    let error_msgs: Vec<String> = errors.iter().map(|e| e.to_string()).collect();
                    tracing::warn!(
                        "Invalid route config in '{}', keeping previous configuration. Errors:\n{}",
                        config_path.display(),
                        error_msgs.join("\n")
                    );
                }
            },
            Err(e) => {
                tracing::warn!(
                    "Failed to read route config from '{}', keeping previous configuration: {}",
                    config_path.display(),
                    e
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn sample_yaml() -> &'static str {
        r#"
custom_providers:
  - name: "internal_llm"
    base_url: "http://internal-llm.corp:8080"
    api_format: "openai_compatible"

routes:
  - path: "/v1/chat/agent"
    method: "POST"
    provider: "openai"
    model: "gpt-4o"
    token_optimization:
      strategy: "adaptive_trimming"
      max_history_messages: 10
      compress_above_tokens: 4000
      local_tokenizer: "cl100k_base"

  - path: "/v1/chat/support"
    method: "POST"
    provider: "anthropic"
    model: "claude-sonnet-4-20250514"
    token_optimization:
      strategy: "sliding_window"
      max_history_messages: 50
      compress_above_tokens: 8000
      local_tokenizer: "cl100k_base"

  - path: "/v1/chat/completions"
    method: "POST"
    providers:
      - name: "openai"
        weight: 60
        model: "gpt-4o"
      - name: "anthropic"
        weight: 30
        model: "claude-sonnet-4-20250514"
      - name: "google_vertex_ai"
        weight: 10
        model: "gemini-pro"
    token_optimization:
      strategy: "adaptive_trimming"
      max_history_messages: 20
      compress_above_tokens: 4096
      local_tokenizer: "cl100k_base"

  - path: "/v1/chat/raw"
    method: "POST"
    provider: "ollama"
    model: "llama3"

  - path: "/v1/chat/internal"
    method: "POST"
    provider: "internal_llm"
    model: "custom-fine-tuned-v2"
    token_optimization:
      strategy: "none"
"#
    }

    fn build_resolver() -> RouteResolver {
        let config: RouteConfigFile = serde_yaml::from_str(sample_yaml()).unwrap();
        RouteResolver::new(config)
    }

    #[test]
    fn test_parse_valid_routes_yaml() {
        let config: RouteConfigFile = serde_yaml::from_str(sample_yaml()).unwrap();

        assert_eq!(config.routes.len(), 5);
        assert_eq!(config.custom_providers.len(), 1);
        assert_eq!(config.custom_providers[0].name, "internal_llm");
        assert_eq!(config.custom_providers[0].api_format, "openai_compatible");
    }

    #[test]
    fn test_parse_route_with_single_provider() {
        let config: RouteConfigFile = serde_yaml::from_str(sample_yaml()).unwrap();
        let route = &config.routes[0];

        assert_eq!(route.path, "/v1/chat/agent");
        assert_eq!(route.method, "POST");
        assert_eq!(route.provider.as_deref(), Some("openai"));
        assert_eq!(route.model.as_deref(), Some("gpt-4o"));
        assert!(route.providers.is_none());
    }

    #[test]
    fn test_parse_route_with_multiple_providers() {
        let config: RouteConfigFile = serde_yaml::from_str(sample_yaml()).unwrap();
        let route = &config.routes[2];

        assert_eq!(route.path, "/v1/chat/completions");
        assert!(route.provider.is_none());
        let providers = route.providers.as_ref().unwrap();
        assert_eq!(providers.len(), 3);
        assert_eq!(providers[0].name, "openai");
        assert_eq!(providers[0].weight, 60);
        assert_eq!(providers[0].model, "gpt-4o");
        assert_eq!(providers[1].name, "anthropic");
        assert_eq!(providers[1].weight, 30);
        assert_eq!(providers[2].name, "google_vertex_ai");
        assert_eq!(providers[2].weight, 10);
    }

    #[test]
    fn test_parse_token_optimization_adaptive_trimming() {
        let config: RouteConfigFile = serde_yaml::from_str(sample_yaml()).unwrap();
        let route = &config.routes[0];
        let opt = route.token_optimization.as_ref().unwrap();

        assert_eq!(opt.strategy, TokenOptimizationStrategy::AdaptiveTrimming);
        assert_eq!(opt.max_history_messages, 10);
        assert_eq!(opt.compress_above_tokens, 4000);
        assert_eq!(opt.local_tokenizer, "cl100k_base");
    }

    #[test]
    fn test_parse_token_optimization_sliding_window() {
        let config: RouteConfigFile = serde_yaml::from_str(sample_yaml()).unwrap();
        let route = &config.routes[1];
        let opt = route.token_optimization.as_ref().unwrap();

        assert_eq!(opt.strategy, TokenOptimizationStrategy::SlidingWindow);
        assert_eq!(opt.max_history_messages, 50);
        assert_eq!(opt.compress_above_tokens, 8000);
    }

    #[test]
    fn test_parse_token_optimization_none_strategy() {
        let config: RouteConfigFile = serde_yaml::from_str(sample_yaml()).unwrap();
        let route = &config.routes[4]; // /v1/chat/internal
        let opt = route.token_optimization.as_ref().unwrap();

        assert_eq!(opt.strategy, TokenOptimizationStrategy::None);
    }

    #[test]
    fn test_token_optimization_defaults() {
        let yaml = r#"
routes:
  - path: "/v1/chat/test"
    method: "POST"
    provider: "openai"
    token_optimization:
      strategy: "adaptive_trimming"
"#;
        let config: RouteConfigFile = serde_yaml::from_str(yaml).unwrap();
        let opt = config.routes[0].token_optimization.as_ref().unwrap();

        assert_eq!(opt.max_history_messages, 20);
        assert_eq!(opt.compress_above_tokens, 4096);
        assert_eq!(opt.local_tokenizer, "cl100k_base");
    }

    #[test]
    fn test_route_without_token_optimization() {
        let config: RouteConfigFile = serde_yaml::from_str(sample_yaml()).unwrap();
        let route = &config.routes[3]; // /v1/chat/raw

        assert_eq!(route.path, "/v1/chat/raw");
        assert_eq!(route.provider.as_deref(), Some("ollama"));
        assert!(route.token_optimization.is_none());
    }

    #[test]
    fn test_empty_custom_providers_defaults() {
        let yaml = r#"
routes:
  - path: "/v1/chat/test"
    method: "POST"
    provider: "openai"
"#;
        let config: RouteConfigFile = serde_yaml::from_str(yaml).unwrap();
        assert!(config.custom_providers.is_empty());
    }

    #[test]
    fn test_provider_type_deserialization() {
        // Test known provider types
        let yaml_openai = r#""openai""#;
        let pt: ProviderType = serde_yaml::from_str(yaml_openai).unwrap();
        assert_eq!(pt, ProviderType::Openai);

        let yaml_anthropic = r#""anthropic""#;
        let pt: ProviderType = serde_yaml::from_str(yaml_anthropic).unwrap();
        assert_eq!(pt, ProviderType::Anthropic);

        let yaml_vertex = r#""google_vertex_ai""#;
        let pt: ProviderType = serde_yaml::from_str(yaml_vertex).unwrap();
        assert_eq!(pt, ProviderType::GoogleVertexAi);

        let yaml_oci = r#""oci_genai""#;
        let pt: ProviderType = serde_yaml::from_str(yaml_oci).unwrap();
        assert_eq!(pt, ProviderType::OciGenai);

        let yaml_ollama = r#""ollama""#;
        let pt: ProviderType = serde_yaml::from_str(yaml_ollama).unwrap();
        assert_eq!(pt, ProviderType::Ollama);

        // Custom provider type
        let yaml_custom = r#""my_custom_provider""#;
        let pt: ProviderType = serde_yaml::from_str(yaml_custom).unwrap();
        assert_eq!(pt, ProviderType::Custom("my_custom_provider".to_string()));
    }

    #[test]
    fn test_token_optimization_strategy_deserialization() {
        let yaml_at = r#""adaptive_trimming""#;
        let s: TokenOptimizationStrategy = serde_yaml::from_str(yaml_at).unwrap();
        assert_eq!(s, TokenOptimizationStrategy::AdaptiveTrimming);

        let yaml_sw = r#""sliding_window""#;
        let s: TokenOptimizationStrategy = serde_yaml::from_str(yaml_sw).unwrap();
        assert_eq!(s, TokenOptimizationStrategy::SlidingWindow);

        let yaml_none = r#""none""#;
        let s: TokenOptimizationStrategy = serde_yaml::from_str(yaml_none).unwrap();
        assert_eq!(s, TokenOptimizationStrategy::None);
    }

    #[test]
    fn test_load_routes_from_file() {
        let mut file = NamedTempFile::new().unwrap();
        write!(file, "{}", sample_yaml()).unwrap();

        let config = load_routes(file.path()).unwrap();
        assert_eq!(config.routes.len(), 5);
        assert_eq!(config.custom_providers.len(), 1);
    }

    #[test]
    fn test_load_routes_file_not_found() {
        let result = load_routes(Path::new("/nonexistent/routes.yaml"));
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), RoutesConfigError::IoError(_)));
    }

    #[test]
    fn test_load_routes_invalid_yaml() {
        let mut file = NamedTempFile::new().unwrap();
        write!(file, "{{{{ invalid yaml content").unwrap();

        let result = load_routes(file.path());
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), RoutesConfigError::ParseError(_)));
    }

    // ─── RouteResolver Tests ──────────────────────────────────────────────────

    #[test]
    fn test_resolve_route_exact_match() {
        let resolver = build_resolver();

        let route = resolver.resolve_route("/v1/chat/agent", "POST").unwrap();
        assert_eq!(route.path, "/v1/chat/agent");
        assert_eq!(route.provider.as_deref(), Some("openai"));
        assert_eq!(route.model.as_deref(), Some("gpt-4o"));
    }

    #[test]
    fn test_resolve_route_case_insensitive_method() {
        let resolver = build_resolver();

        // Method matching should be case-insensitive
        let route = resolver.resolve_route("/v1/chat/agent", "post").unwrap();
        assert_eq!(route.path, "/v1/chat/agent");

        let route = resolver.resolve_route("/v1/chat/agent", "Post").unwrap();
        assert_eq!(route.path, "/v1/chat/agent");

        let route = resolver.resolve_route("/v1/chat/agent", "POST").unwrap();
        assert_eq!(route.path, "/v1/chat/agent");

        let route = resolver.resolve_route("/v1/chat/agent", "pOsT").unwrap();
        assert_eq!(route.path, "/v1/chat/agent");
    }

    #[test]
    fn test_resolve_route_case_sensitive_path() {
        let resolver = build_resolver();

        // Path matching is case-sensitive — different case should not match
        assert!(resolver.resolve_route("/V1/CHAT/AGENT", "POST").is_none());
        assert!(resolver.resolve_route("/v1/Chat/Agent", "POST").is_none());
    }

    #[test]
    fn test_resolve_route_no_match_returns_none() {
        let resolver = build_resolver();

        // Non-existent path
        assert!(resolver.resolve_route("/v1/chat/unknown", "POST").is_none());

        // Existing path but wrong method
        assert!(resolver.resolve_route("/v1/chat/agent", "GET").is_none());
        assert!(resolver.resolve_route("/v1/chat/agent", "DELETE").is_none());

        // Empty path and method
        assert!(resolver.resolve_route("", "").is_none());
    }

    #[test]
    fn test_resolve_route_all_routes_resolvable() {
        let resolver = build_resolver();

        // All 5 routes from sample_yaml should be resolvable
        assert!(resolver.resolve_route("/v1/chat/agent", "POST").is_some());
        assert!(resolver.resolve_route("/v1/chat/support", "POST").is_some());
        assert!(resolver.resolve_route("/v1/chat/completions", "POST").is_some());
        assert!(resolver.resolve_route("/v1/chat/raw", "POST").is_some());
        assert!(resolver.resolve_route("/v1/chat/internal", "POST").is_some());
    }

    #[test]
    fn test_resolve_route_different_methods_same_path() {
        let yaml = r#"
routes:
  - path: "/v1/chat/test"
    method: "POST"
    provider: "openai"
    model: "gpt-4o"
  - path: "/v1/chat/test"
    method: "GET"
    provider: "anthropic"
    model: "claude-sonnet-4-20250514"
"#;
        let config: RouteConfigFile = serde_yaml::from_str(yaml).unwrap();
        let resolver = RouteResolver::new(config);

        let post_route = resolver.resolve_route("/v1/chat/test", "POST").unwrap();
        assert_eq!(post_route.provider.as_deref(), Some("openai"));

        let get_route = resolver.resolve_route("/v1/chat/test", "GET").unwrap();
        assert_eq!(get_route.provider.as_deref(), Some("anthropic"));
    }

    // ─── effective_token_config Tests ─────────────────────────────────────────

    #[test]
    fn test_effective_token_config_uses_per_route_when_defined() {
        let resolver = build_resolver();
        let global = CompactorConfig {
            token_threshold: 4096,
            max_history_messages: 20,
            stop_words: vec!["the".to_string(), "a".to_string()],
            tokenizer_name: "global_tokenizer".to_string(),
        };

        // /v1/chat/agent has token_optimization with compress_above_tokens: 4000
        let route = resolver.resolve_route("/v1/chat/agent", "POST").unwrap();
        let effective = resolver.effective_token_config(route, &global);

        assert_eq!(effective.token_threshold, 4000);
        assert_eq!(effective.tokenizer_name, "cl100k_base");
        // stop_words should come from global config
        assert_eq!(effective.stop_words, global.stop_words);
    }

    #[test]
    fn test_effective_token_config_uses_global_when_no_route_config() {
        let resolver = build_resolver();
        let global = CompactorConfig {
            token_threshold: 4096,
            max_history_messages: 20,
            stop_words: vec!["the".to_string()],
            tokenizer_name: "global_tokenizer".to_string(),
        };

        // /v1/chat/raw has no token_optimization
        let route = resolver.resolve_route("/v1/chat/raw", "POST").unwrap();
        let effective = resolver.effective_token_config(route, &global);

        assert_eq!(effective, global);
    }

    #[test]
    fn test_effective_token_config_sliding_window_route() {
        let resolver = build_resolver();
        let global = CompactorConfig {
            token_threshold: 4096,
            max_history_messages: 20,
            stop_words: vec![],
            tokenizer_name: "global_tokenizer".to_string(),
        };

        // /v1/chat/support has sliding_window with compress_above_tokens: 8000
        let route = resolver.resolve_route("/v1/chat/support", "POST").unwrap();
        let effective = resolver.effective_token_config(route, &global);

        assert_eq!(effective.token_threshold, 8000);
        assert_eq!(effective.tokenizer_name, "cl100k_base");
    }

    #[test]
    fn test_effective_token_config_preserves_global_stop_words() {
        let resolver = build_resolver();
        let global = CompactorConfig {
            token_threshold: 4096,
            max_history_messages: 20,
            stop_words: vec![
                "the".to_string(),
                "is".to_string(),
                "at".to_string(),
            ],
            tokenizer_name: "global_tokenizer".to_string(),
        };

        // Per-route config should inherit stop_words from global
        let route = resolver.resolve_route("/v1/chat/agent", "POST").unwrap();
        let effective = resolver.effective_token_config(route, &global);

        assert_eq!(effective.stop_words.len(), 3);
        assert_eq!(effective.stop_words, global.stop_words);
    }

    // ─── Validation Tests ─────────────────────────────────────────────────────

    #[test]
    fn test_validate_valid_config() {
        let config: RouteConfigFile = serde_yaml::from_str(sample_yaml()).unwrap();
        assert!(validate(&config).is_ok());
    }

    #[test]
    fn test_validate_missing_path() {
        let yaml = r#"
routes:
  - path: ""
    method: "POST"
    provider: "openai"
"#;
        let config: RouteConfigFile = serde_yaml::from_str(yaml).unwrap();
        let errors = validate(&config).unwrap_err();
        assert!(errors.iter().any(|e| matches!(e,
            RoutesConfigError::MissingField { route_index: 0, field } if field == "path"
        )));
    }

    #[test]
    fn test_validate_missing_method() {
        let yaml = r#"
routes:
  - path: "/v1/chat/test"
    method: ""
    provider: "openai"
"#;
        let config: RouteConfigFile = serde_yaml::from_str(yaml).unwrap();
        let errors = validate(&config).unwrap_err();
        assert!(errors.iter().any(|e| matches!(e,
            RoutesConfigError::MissingField { route_index: 0, field } if field == "method"
        )));
    }

    #[test]
    fn test_validate_missing_provider_and_providers() {
        let yaml = r#"
routes:
  - path: "/v1/chat/test"
    method: "POST"
"#;
        let config: RouteConfigFile = serde_yaml::from_str(yaml).unwrap();
        let errors = validate(&config).unwrap_err();
        assert!(errors.iter().any(|e| matches!(e,
            RoutesConfigError::MissingField { route_index: 0, field } if field.contains("provider")
        )));
    }

    #[test]
    fn test_validate_invalid_provider() {
        let yaml = r#"
routes:
  - path: "/v1/chat/test"
    method: "POST"
    provider: "unknown_provider"
"#;
        let config: RouteConfigFile = serde_yaml::from_str(yaml).unwrap();
        let errors = validate(&config).unwrap_err();
        assert!(errors.iter().any(|e| matches!(e,
            RoutesConfigError::InvalidProvider { route_index: 0, provider } if provider == "unknown_provider"
        )));
    }

    #[test]
    fn test_validate_invalid_provider_in_providers_list() {
        let yaml = r#"
routes:
  - path: "/v1/chat/test"
    method: "POST"
    providers:
      - name: "openai"
        weight: 50
        model: "gpt-4o"
      - name: "fake_provider"
        weight: 50
        model: "fake-model"
"#;
        let config: RouteConfigFile = serde_yaml::from_str(yaml).unwrap();
        let errors = validate(&config).unwrap_err();
        assert!(errors.iter().any(|e| matches!(e,
            RoutesConfigError::InvalidProvider { route_index: 0, provider } if provider == "fake_provider"
        )));
    }

    #[test]
    fn test_validate_custom_provider_accepted() {
        let yaml = r#"
custom_providers:
  - name: "internal_llm"
    base_url: "http://internal:8080"
    api_format: "openai_compatible"

routes:
  - path: "/v1/chat/internal"
    method: "POST"
    provider: "internal_llm"
"#;
        let config: RouteConfigFile = serde_yaml::from_str(yaml).unwrap();
        assert!(validate(&config).is_ok());
    }

    #[test]
    fn test_validate_duplicate_routes() {
        let yaml = r#"
routes:
  - path: "/v1/chat/test"
    method: "POST"
    provider: "openai"
  - path: "/v1/chat/test"
    method: "POST"
    provider: "anthropic"
"#;
        let config: RouteConfigFile = serde_yaml::from_str(yaml).unwrap();
        let errors = validate(&config).unwrap_err();
        assert!(errors.iter().any(|e| matches!(e,
            RoutesConfigError::DuplicateRoute { path, method, first: 0, second: 1 }
            if path == "/v1/chat/test" && method == "POST"
        )));
    }

    #[test]
    fn test_validate_duplicate_routes_case_insensitive_method() {
        let yaml = r#"
routes:
  - path: "/v1/chat/test"
    method: "post"
    provider: "openai"
  - path: "/v1/chat/test"
    method: "POST"
    provider: "anthropic"
"#;
        let config: RouteConfigFile = serde_yaml::from_str(yaml).unwrap();
        let errors = validate(&config).unwrap_err();
        assert!(errors.iter().any(|e| matches!(e,
            RoutesConfigError::DuplicateRoute { first: 0, second: 1, .. }
        )));
    }

    #[test]
    fn test_validate_same_path_different_method_not_duplicate() {
        let yaml = r#"
routes:
  - path: "/v1/chat/test"
    method: "POST"
    provider: "openai"
  - path: "/v1/chat/test"
    method: "GET"
    provider: "openai"
"#;
        let config: RouteConfigFile = serde_yaml::from_str(yaml).unwrap();
        assert!(validate(&config).is_ok());
    }

    #[test]
    fn test_validate_multiple_errors_collected() {
        let yaml = r#"
routes:
  - path: ""
    method: ""
    provider: "invalid_prov"
  - path: "/v1/chat/test"
    method: "POST"
"#;
        let config: RouteConfigFile = serde_yaml::from_str(yaml).unwrap();
        let errors = validate(&config).unwrap_err();
        // Route 0: missing path, missing method, invalid provider
        // Route 1: missing provider/providers
        assert!(errors.len() >= 4);
    }

    #[test]
    fn test_validate_providers_list_satisfies_provider_requirement() {
        let yaml = r#"
routes:
  - path: "/v1/chat/test"
    method: "POST"
    providers:
      - name: "openai"
        weight: 60
        model: "gpt-4o"
      - name: "anthropic"
        weight: 40
        model: "claude-sonnet-4-20250514"
"#;
        let config: RouteConfigFile = serde_yaml::from_str(yaml).unwrap();
        assert!(validate(&config).is_ok());
    }

    #[test]
    fn test_validate_whitespace_only_path_is_missing() {
        let yaml = r#"
routes:
  - path: "   "
    method: "POST"
    provider: "openai"
"#;
        let config: RouteConfigFile = serde_yaml::from_str(yaml).unwrap();
        let errors = validate(&config).unwrap_err();
        assert!(errors.iter().any(|e| matches!(e,
            RoutesConfigError::MissingField { route_index: 0, field } if field == "path"
        )));
    }

    #[test]
    fn test_validate_all_known_providers_accepted() {
        let yaml = r#"
routes:
  - path: "/v1/openai"
    method: "POST"
    provider: "openai"
  - path: "/v1/anthropic"
    method: "POST"
    provider: "anthropic"
  - path: "/v1/vertex"
    method: "POST"
    provider: "google_vertex_ai"
  - path: "/v1/oci"
    method: "POST"
    provider: "oci_genai"
  - path: "/v1/ollama"
    method: "POST"
    provider: "ollama"
"#;
        let config: RouteConfigFile = serde_yaml::from_str(yaml).unwrap();
        assert!(validate(&config).is_ok());
    }

    // ─── RouteConfigManager Tests ─────────────────────────────────────────────

    #[tokio::test]
    async fn test_route_config_manager_initial_load_success() {
        let mut file = NamedTempFile::new().unwrap();
        write!(file, "{}", sample_yaml()).unwrap();
        file.flush().unwrap();

        let manager = RouteConfigManager::new(file.path().to_path_buf());
        let resolver = manager.current();

        // Verify the resolver has routes loaded
        let route = resolver.resolve_route("/v1/chat/agent", "POST");
        assert!(route.is_some());
        assert_eq!(route.unwrap().provider.as_deref(), Some("openai"));

        // Verify all 5 routes are resolvable
        assert!(resolver.resolve_route("/v1/chat/support", "POST").is_some());
        assert!(resolver.resolve_route("/v1/chat/completions", "POST").is_some());
        assert!(resolver.resolve_route("/v1/chat/raw", "POST").is_some());
        assert!(resolver.resolve_route("/v1/chat/internal", "POST").is_some());
    }

    #[test]
    #[should_panic(expected = "Failed to load initial route config")]
    fn test_route_config_manager_initial_load_file_not_found() {
        let _manager = RouteConfigManager::new(PathBuf::from("/nonexistent/routes.yaml"));
    }

    #[test]
    #[should_panic(expected = "Route config validation failed")]
    fn test_route_config_manager_initial_load_invalid_config() {
        let mut file = NamedTempFile::new().unwrap();
        // Write a config with an invalid provider
        write!(
            file,
            r#"
routes:
  - path: "/v1/chat/test"
    method: "POST"
    provider: "nonexistent_provider"
"#
        )
        .unwrap();
        file.flush().unwrap();

        let _manager = RouteConfigManager::new(file.path().to_path_buf());
    }

    #[test]
    #[should_panic(expected = "Failed to load initial route config")]
    fn test_route_config_manager_initial_load_invalid_yaml() {
        let mut file = NamedTempFile::new().unwrap();
        write!(file, "{{{{ not valid yaml").unwrap();
        file.flush().unwrap();

        let _manager = RouteConfigManager::new(file.path().to_path_buf());
    }

    #[tokio::test]
    async fn test_route_config_manager_current_returns_valid_resolver() {
        let mut file = NamedTempFile::new().unwrap();
        write!(file, "{}", sample_yaml()).unwrap();
        file.flush().unwrap();

        let manager = RouteConfigManager::new(file.path().to_path_buf());

        // current() should be callable multiple times and return valid data
        let resolver1 = manager.current();
        let route1 = resolver1.resolve_route("/v1/chat/agent", "POST").unwrap();
        assert_eq!(route1.model.as_deref(), Some("gpt-4o"));
        drop(resolver1);

        let resolver2 = manager.current();
        let route2 = resolver2.resolve_route("/v1/chat/support", "POST").unwrap();
        assert_eq!(route2.provider.as_deref(), Some("anthropic"));
        drop(resolver2);
    }

    #[tokio::test]
    async fn test_route_config_manager_hot_reload_valid_config() {
        // Create initial config file in a temp directory
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("routes.yaml");
        std::fs::write(&config_path, sample_yaml()).unwrap();

        let manager = RouteConfigManager::new(config_path.clone());

        // Verify initial state
        {
            let resolver = manager.current();
            let route = resolver.resolve_route("/v1/chat/agent", "POST").unwrap();
            assert_eq!(route.provider.as_deref(), Some("openai"));
        }

        // Write new valid config
        let new_yaml = r#"
routes:
  - path: "/v1/chat/agent"
    method: "POST"
    provider: "anthropic"
    model: "claude-sonnet-4-20250514"
"#;
        std::fs::write(&config_path, new_yaml).unwrap();

        // Wait for debounce + processing (200ms debounce + some buffer)
        tokio::time::sleep(Duration::from_millis(500)).await;

        // Verify config was reloaded
        let resolver = manager.current();
        let route = resolver.resolve_route("/v1/chat/agent", "POST").unwrap();
        assert_eq!(route.provider.as_deref(), Some("anthropic"));
        assert_eq!(route.model.as_deref(), Some("claude-sonnet-4-20250514"));
    }

    #[tokio::test]
    async fn test_route_config_manager_hot_reload_invalid_keeps_previous() {
        // Create initial config file in a temp directory
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("routes.yaml");
        std::fs::write(&config_path, sample_yaml()).unwrap();

        let manager = RouteConfigManager::new(config_path.clone());

        // Verify initial state
        {
            let resolver = manager.current();
            let route = resolver.resolve_route("/v1/chat/agent", "POST").unwrap();
            assert_eq!(route.provider.as_deref(), Some("openai"));
        }

        // Write invalid config (unknown provider)
        let invalid_yaml = r#"
routes:
  - path: "/v1/chat/agent"
    method: "POST"
    provider: "nonexistent_provider"
"#;
        std::fs::write(&config_path, invalid_yaml).unwrap();

        // Wait for debounce + processing
        tokio::time::sleep(Duration::from_millis(500)).await;

        // Verify previous config is still active
        let resolver = manager.current();
        let route = resolver.resolve_route("/v1/chat/agent", "POST").unwrap();
        assert_eq!(route.provider.as_deref(), Some("openai"));
    }
}

// ─── Property-Based Tests (proptest) ──────────────────────────────────────────

#[cfg(test)]
mod property_tests {
    use super::*;
    use crate::config::CompactorConfig;
    use proptest::prelude::*;
    use std::io::Write;

    // ─── Strategies (generators) ──────────────────────────────────────────────

    /// Generate a valid HTTP method.
    fn arb_http_method() -> impl Strategy<Value = String> {
        prop_oneof![
            Just("GET".to_string()),
            Just("POST".to_string()),
            Just("PUT".to_string()),
            Just("DELETE".to_string()),
            Just("PATCH".to_string()),
        ]
    }

    /// Generate a valid path (starts with /).
    fn arb_path() -> impl Strategy<Value = String> {
        "[a-z][a-z0-9/]{1,30}".prop_map(|s| format!("/{}", s))
    }

    /// Generate a known provider name.
    fn arb_known_provider() -> impl Strategy<Value = String> {
        prop_oneof![
            Just("openai".to_string()),
            Just("anthropic".to_string()),
            Just("google_vertex_ai".to_string()),
            Just("oci_genai".to_string()),
            Just("ollama".to_string()),
        ]
    }

    /// Generate an arbitrary model name.
    fn arb_model() -> impl Strategy<Value = String> {
        "[a-z][a-z0-9_-]{2,20}".prop_map(|s| s)
    }

    /// Generate a valid TokenOptimizationConfig.
    fn arb_token_optimization() -> impl Strategy<Value = TokenOptimizationConfig> {
        (
            prop_oneof![
                Just(TokenOptimizationStrategy::AdaptiveTrimming),
                Just(TokenOptimizationStrategy::SlidingWindow),
                Just(TokenOptimizationStrategy::None),
            ],
            1usize..100,
            512usize..32000,
        )
            .prop_map(|(strategy, max_hist, compress_above)| TokenOptimizationConfig {
                strategy,
                max_history_messages: max_hist,
                compress_above_tokens: compress_above,
                local_tokenizer: "cl100k_base".to_string(),
            })
    }

    /// Generate a valid RouteDefinition.
    fn arb_valid_route() -> impl Strategy<Value = RouteDefinition> {
        (arb_path(), arb_http_method(), arb_known_provider(), arb_model(), proptest::option::of(arb_token_optimization()))
            .prop_map(|(path, method, provider, model, token_opt)| RouteDefinition {
                path,
                method,
                provider: Some(provider),
                model: Some(model),
                providers: None,
                token_optimization: token_opt,
            })
    }

    /// Generate a valid RouteConfigFile with unique (path, method) pairs.
    fn arb_valid_config(max_routes: usize) -> impl Strategy<Value = RouteConfigFile> {
        proptest::collection::vec(arb_valid_route(), 1..=max_routes).prop_map(|routes| {
            // Deduplicate by (path, method) to avoid DuplicateRoute errors
            let mut seen = std::collections::HashSet::new();
            let unique_routes: Vec<RouteDefinition> = routes
                .into_iter()
                .filter(|r| seen.insert((r.path.clone(), r.method.to_uppercase())))
                .collect();
            // Ensure at least one route
            let routes = if unique_routes.is_empty() {
                vec![RouteDefinition {
                    path: "/v1/default".to_string(),
                    method: "POST".to_string(),
                    provider: Some("openai".to_string()),
                    model: Some("gpt-4o".to_string()),
                    providers: None,
                    token_optimization: None,
                }]
            } else {
                unique_routes
            };
            RouteConfigFile {
                routes,
                custom_providers: vec![],
            }
        })
    }

    // ═══════════════════════════════════════════════════════════════════════════
    // Property 16: Validação Estrutural da Route Config
    // **Validates: Requirements 11.1, 11.2, 11.7, 11.10**
    // ═══════════════════════════════════════════════════════════════════════════

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        /// Valid configs with all required fields validate Ok.
        /// **Validates: Requirements 11.1, 11.2, 11.7, 11.10**
        #[test]
        fn prop16_valid_config_validates_ok(config in arb_valid_config(5)) {
            let result = validate(&config);
            prop_assert!(result.is_ok(), "Valid config should pass validation");
        }

        /// Configs with missing path field return MissingField error.
        /// **Validates: Requirements 11.1, 11.2, 11.7, 11.10**
        #[test]
        fn prop16_missing_path_returns_error(method in arb_http_method(), provider in arb_known_provider()) {
            let config = RouteConfigFile {
                routes: vec![RouteDefinition {
                    path: "".to_string(),
                    method,
                    provider: Some(provider),
                    model: None,
                    providers: None,
                    token_optimization: None,
                }],
                custom_providers: vec![],
            };
            let result = validate(&config);
            prop_assert!(result.is_err());
            let errors = result.unwrap_err();
            let has_path_error = errors.iter().any(|e| matches!(e,
                RoutesConfigError::MissingField { ref field, .. } if field == "path"
            ));
            prop_assert!(has_path_error, "Expected MissingField error for path");
        }

        /// Configs with missing method field return MissingField error.
        /// **Validates: Requirements 11.1, 11.2, 11.7, 11.10**
        #[test]
        fn prop16_missing_method_returns_error(path in arb_path(), provider in arb_known_provider()) {
            let config = RouteConfigFile {
                routes: vec![RouteDefinition {
                    path,
                    method: "".to_string(),
                    provider: Some(provider),
                    model: None,
                    providers: None,
                    token_optimization: None,
                }],
                custom_providers: vec![],
            };
            let result = validate(&config);
            prop_assert!(result.is_err());
            let errors = result.unwrap_err();
            let has_method_error = errors.iter().any(|e| matches!(e,
                RoutesConfigError::MissingField { ref field, .. } if field == "method"
            ));
            prop_assert!(has_method_error, "Expected MissingField error for method");
        }

        /// Configs missing both provider and providers field return MissingField error.
        /// **Validates: Requirements 11.1, 11.2, 11.7, 11.10**
        #[test]
        fn prop16_missing_provider_returns_error(path in arb_path(), method in arb_http_method()) {
            let config = RouteConfigFile {
                routes: vec![RouteDefinition {
                    path,
                    method,
                    provider: None,
                    model: None,
                    providers: None,
                    token_optimization: None,
                }],
                custom_providers: vec![],
            };
            let result = validate(&config);
            prop_assert!(result.is_err());
            let errors = result.unwrap_err();
            let has_provider_error = errors.iter().any(|e| matches!(e,
                RoutesConfigError::MissingField { ref field, .. } if field.contains("provider")
            ));
            prop_assert!(has_provider_error, "Expected MissingField error for provider");
        }

        /// Configs with unknown provider return InvalidProvider error.
        /// **Validates: Requirements 11.1, 11.2, 11.7, 11.10**
        #[test]
        fn prop16_invalid_provider_returns_error(
            path in arb_path(),
            method in arb_http_method(),
            bad_provider in "[a-z]{5,15}_invalid"
        ) {
            let config = RouteConfigFile {
                routes: vec![RouteDefinition {
                    path,
                    method,
                    provider: Some(bad_provider.clone()),
                    model: None,
                    providers: None,
                    token_optimization: None,
                }],
                custom_providers: vec![],
            };
            let result = validate(&config);
            prop_assert!(result.is_err());
            let errors = result.unwrap_err();
            let has_invalid_provider = errors.iter().any(|e| matches!(e,
                RoutesConfigError::InvalidProvider { ref provider, .. } if *provider == bad_provider
            ));
            prop_assert!(has_invalid_provider, "Expected InvalidProvider error");
        }
    }

    // ═══════════════════════════════════════════════════════════════════════════
    // Property 17: Override de Modelo pela Rota
    // **Validates: Requirements 11.3**
    // ═══════════════════════════════════════════════════════════════════════════

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        /// When route.model is Some(x), the effective model is x (not the payload model).
        /// **Validates: Requirements 11.3**
        #[test]
        fn prop17_route_model_overrides_payload_model(
            route_model in arb_model(),
            payload_model in arb_model(),
        ) {
            // Simulate the model override logic:
            // If route has model defined, it takes precedence over the payload model.
            let route = RouteDefinition {
                path: "/v1/chat/test".to_string(),
                method: "POST".to_string(),
                provider: Some("openai".to_string()),
                model: Some(route_model.clone()),
                providers: None,
                token_optimization: None,
            };

            let effective_model = route.model.as_deref().unwrap_or(&payload_model);
            prop_assert_eq!(effective_model, route_model.as_str());
        }

        /// When route.model is None, the effective model is the payload model.
        /// **Validates: Requirements 11.3**
        #[test]
        fn prop17_no_route_model_uses_payload_model(
            payload_model in arb_model(),
        ) {
            let route = RouteDefinition {
                path: "/v1/chat/test".to_string(),
                method: "POST".to_string(),
                provider: Some("openai".to_string()),
                model: None,
                providers: None,
                token_optimization: None,
            };

            let effective_model = route.model.as_deref().unwrap_or(&payload_model);
            prop_assert_eq!(effective_model, payload_model.as_str());
        }
    }

    // ═══════════════════════════════════════════════════════════════════════════
    // Property 18: Resolução de Token Optimization (Per-Route vs Global)
    // **Validates: Requirements 11.4, 11.5**
    // ═══════════════════════════════════════════════════════════════════════════

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        /// When route has token_optimization, effective config uses route values.
        /// **Validates: Requirements 11.4, 11.5**
        #[test]
        fn prop18_route_token_optimization_takes_precedence(
            route_threshold in 512usize..32000,
            route_tokenizer in "[a-z][a-z0-9_]{3,15}",
            global_threshold in 512usize..32000,
            global_tokenizer in "[a-z][a-z0-9_]{3,15}",
        ) {
            let route = RouteDefinition {
                path: "/v1/chat/test".to_string(),
                method: "POST".to_string(),
                provider: Some("openai".to_string()),
                model: None,
                providers: None,
                token_optimization: Some(TokenOptimizationConfig {
                    strategy: TokenOptimizationStrategy::AdaptiveTrimming,
                    max_history_messages: 20,
                    compress_above_tokens: route_threshold,
                    local_tokenizer: route_tokenizer.clone(),
                }),
            };

            let global = CompactorConfig {
                token_threshold: global_threshold,
                max_history_messages: 20,
                stop_words: vec!["the".to_string(), "a".to_string()],
                tokenizer_name: global_tokenizer,
            };

            let config = RouteConfigFile {
                routes: vec![route.clone()],
                custom_providers: vec![],
            };
            let resolver = RouteResolver::new(config);
            let effective = resolver.effective_token_config(&route, &global);

            // Per-route values should be used
            prop_assert_eq!(effective.token_threshold, route_threshold);
            prop_assert_eq!(&effective.tokenizer_name, &route_tokenizer);
            // stop_words come from global
            prop_assert_eq!(&effective.stop_words, &global.stop_words);
        }

        /// When route has no token_optimization, effective config equals global.
        /// **Validates: Requirements 11.4, 11.5**
        #[test]
        fn prop18_no_route_optimization_uses_global(
            global_threshold in 512usize..32000,
            global_tokenizer in "[a-z][a-z0-9_]{3,15}",
        ) {
            let route = RouteDefinition {
                path: "/v1/chat/test".to_string(),
                method: "POST".to_string(),
                provider: Some("openai".to_string()),
                model: None,
                providers: None,
                token_optimization: None,
            };

            let global = CompactorConfig {
                token_threshold: global_threshold,
                max_history_messages: 20,
                stop_words: vec!["stop".to_string()],
                tokenizer_name: global_tokenizer.clone(),
            };

            let config = RouteConfigFile {
                routes: vec![route.clone()],
                custom_providers: vec![],
            };
            let resolver = RouteResolver::new(config);
            let effective = resolver.effective_token_config(&route, &global);

            prop_assert_eq!(effective, global,
                "Without route optimization, effective should equal global config");
        }
    }

    // ═══════════════════════════════════════════════════════════════════════════
    // Property 19: Hot-Reload Seguro (Config Inválida Preserva Anterior)
    // **Validates: Requirements 11.9**
    // ═══════════════════════════════════════════════════════════════════════════

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(20))]

        /// After writing invalid YAML to the config file, the previous valid config
        /// remains accessible via current().
        /// **Validates: Requirements 11.9**
        #[test]
        fn prop19_invalid_reload_preserves_previous(
            initial_provider in arb_known_provider(),
            initial_model in arb_model(),
            invalid_content in prop_oneof![
                Just("{{{{ not valid yaml".to_string()),
                Just("routes:\n  - path: \"/test\"\n    method: \"POST\"\n    provider: \"nonexistent_xyz\"".to_string()),
                Just("garbage content here!!!".to_string()),
                Just("routes:\n  - path: \"\"\n    method: \"\"\n".to_string()),
            ],
        ) {
            // Create a valid initial config
            let valid_yaml = format!(
                "routes:\n  - path: \"/v1/chat/test\"\n    method: \"POST\"\n    provider: \"{}\"\n    model: \"{}\"",
                initial_provider, initial_model
            );

            // Write to a temp directory so the file watcher can detect changes
            let dir = tempfile::tempdir().unwrap();
            let config_path = dir.path().join("routes.yaml");
            std::fs::write(&config_path, &valid_yaml).unwrap();

            // Use try_reload directly (synchronous test, no need for tokio runtime)
            let config_file = load_routes(&config_path).unwrap();
            validate(&config_file).unwrap();
            let resolver = RouteResolver::new(config_file);
            let arc_config = Arc::new(ArcSwap::from_pointee(resolver));

            // Verify initial state
            {
                let current = arc_config.load();
                let route = current.resolve_route("/v1/chat/test", "POST");
                prop_assert!(route.is_some(), "Initial route should be resolvable");
                let route = route.unwrap();
                prop_assert_eq!(route.provider.as_deref(), Some(initial_provider.as_str()));
                prop_assert_eq!(route.model.as_deref(), Some(initial_model.as_str()));
            }

            // Write invalid content to the file
            std::fs::write(&config_path, &invalid_content).unwrap();

            // Call try_reload - it should NOT swap in the invalid config
            RouteConfigManager::try_reload(&arc_config, &config_path);

            // Verify previous config is still active
            let current = arc_config.load();
            let route = current.resolve_route("/v1/chat/test", "POST");
            prop_assert!(route.is_some(), "Previous route should still be resolvable after invalid reload");
            let route = route.unwrap();
            prop_assert_eq!(route.provider.as_deref(), Some(initial_provider.as_str()));
            prop_assert_eq!(route.model.as_deref(), Some(initial_model.as_str()));
        }
    }

    // ═══════════════════════════════════════════════════════════════════════════
    // Property 20: Resolução de Rota por Matching Exato (path + method)
    // **Validates: Requirements 11.3**
    // ═══════════════════════════════════════════════════════════════════════════

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        /// Exact (path, method) match returns the correct route.
        /// **Validates: Requirements 11.3**
        #[test]
        fn prop20_exact_match_resolves_correct_route(
            routes in proptest::collection::vec(
                (arb_path(), arb_http_method(), arb_known_provider(), arb_model()),
                1..=10
            ),
            query_idx in 0usize..10,
        ) {
            // Deduplicate routes by (path, method)
            let mut seen = std::collections::HashSet::new();
            let unique_routes: Vec<_> = routes
                .into_iter()
                .filter(|(p, m, _, _)| seen.insert((p.clone(), m.to_uppercase())))
                .collect();

            if unique_routes.is_empty() {
                return Ok(());
            }

            let route_defs: Vec<RouteDefinition> = unique_routes
                .iter()
                .map(|(path, method, provider, model)| RouteDefinition {
                    path: path.clone(),
                    method: method.clone(),
                    provider: Some(provider.clone()),
                    model: Some(model.clone()),
                    providers: None,
                    token_optimization: None,
                })
                .collect();

            let config = RouteConfigFile {
                routes: route_defs,
                custom_providers: vec![],
            };
            let resolver = RouteResolver::new(config);

            // Pick a route to query (wrap index if out of bounds)
            let idx = query_idx % unique_routes.len();
            let (ref target_path, ref target_method, ref target_provider, ref target_model) = unique_routes[idx];

            let result = resolver.resolve_route(target_path, target_method);
            prop_assert!(result.is_some(),
                "Route should be resolvable");
            let resolved = result.unwrap();
            prop_assert_eq!(&resolved.path, target_path);
            prop_assert_eq!(resolved.provider.as_deref(), Some(target_provider.as_str()));
            prop_assert_eq!(resolved.model.as_deref(), Some(target_model.as_str()));
        }

        /// Non-matching (path, method) returns None.
        /// **Validates: Requirements 11.3**
        #[test]
        fn prop20_no_match_returns_none(
            routes in proptest::collection::vec(
                (arb_path(), arb_http_method(), arb_known_provider(), arb_model()),
                1..=5
            ),
            query_path in arb_path(),
            query_method in arb_http_method(),
        ) {
            // Deduplicate
            let mut seen = std::collections::HashSet::new();
            let unique_routes: Vec<_> = routes
                .into_iter()
                .filter(|(p, m, _, _)| seen.insert((p.clone(), m.to_uppercase())))
                .collect();

            if unique_routes.is_empty() {
                return Ok(());
            }

            let route_defs: Vec<RouteDefinition> = unique_routes
                .iter()
                .map(|(path, method, provider, model)| RouteDefinition {
                    path: path.clone(),
                    method: method.clone(),
                    provider: Some(provider.clone()),
                    model: Some(model.clone()),
                    providers: None,
                    token_optimization: None,
                })
                .collect();

            let config = RouteConfigFile {
                routes: route_defs,
                custom_providers: vec![],
            };
            let resolver = RouteResolver::new(config);

            // Only assert None if the query (path, method) is not in our route table
            let is_in_table = unique_routes.iter().any(|(p, m, _, _)| {
                *p == query_path && m.to_uppercase() == query_method.to_uppercase()
            });

            if !is_in_table {
                let result = resolver.resolve_route(&query_path, &query_method);
                prop_assert!(result.is_none(),
                    "Route should NOT be resolvable when not in table");
            }
        }

        /// Method matching is case-insensitive.
        /// **Validates: Requirements 11.3**
        #[test]
        fn prop20_method_case_insensitive(
            path in arb_path(),
            method in arb_http_method(),
            provider in arb_known_provider(),
            model in arb_model(),
        ) {
            let config = RouteConfigFile {
                routes: vec![RouteDefinition {
                    path: path.clone(),
                    method: method.clone(),
                    provider: Some(provider.clone()),
                    model: Some(model.clone()),
                    providers: None,
                    token_optimization: None,
                }],
                custom_providers: vec![],
            };
            let resolver = RouteResolver::new(config);

            // Query with different case variations of the method
            let lower = method.to_lowercase();
            let upper = method.to_uppercase();

            let r1 = resolver.resolve_route(&path, &lower);
            let r2 = resolver.resolve_route(&path, &upper);

            prop_assert!(r1.is_some(), "Lowercase method should match");
            prop_assert!(r2.is_some(), "Uppercase method should match");
            prop_assert_eq!(&r1.unwrap().path, &path);
            prop_assert_eq!(&r2.unwrap().path, &path);
        }
    }
}
