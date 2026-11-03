pub mod balancer;
mod bedrock;
pub mod circuit_breaker;
pub mod client;
mod compactor;
mod config;
pub mod embeddings;
mod error;
mod guardrails;
mod models;
pub mod observability;
mod route_config;
mod router;
mod routing;
mod semantic_cache;
mod state;
mod tokenizer;

mod middleware;
pub mod streaming;
mod transpiler;

#[cfg(test)]
mod integration_tests;

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use crate::balancer::WeightedRoundRobin;
use crate::circuit_breaker::LocalCircuitBreaker;
use crate::client::ReqwestLlmClient;
use crate::config::GatewayConfig;
use crate::middleware::rate_limiter::LocalTokenBucket;
use crate::observability::{init_tracing, Metrics};
use crate::route_config::RouteConfigManager;
use crate::state::AppState;

#[tokio::main]
async fn main() {
    // Load gateway configuration first so we can use OtelConfig for tracing init.
    // If config loading fails, fall back to defaults.
    let gateway_config = GatewayConfig::load(None).unwrap_or_else(|e| {
        // Can't use tracing yet, so print to stderr
        eprintln!("Failed to load config.yaml: {}. Using defaults.", e);
        default_dev_config()
    });

    // Initialize tracing (console + optional OTLP based on config)
    if let Err(e) = init_tracing(&gateway_config.observability) {
        eprintln!(
            "Failed to initialize tracing: {}. Falling back to basic fmt.",
            e
        );
        // Fallback: basic fmt subscriber so the process can still run
        tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
            )
            .init();
    }

    tracing::info!("Melis AI Gateway starting...");

    let graceful_shutdown_timeout = gateway_config.server.graceful_shutdown_timeout();

    let addr = std::net::SocketAddr::from(([0, 0, 0, 0], gateway_config.server.port));

    // Initialize RouteConfigManager with the routes_config_path from GatewayConfig
    let route_config = RouteConfigManager::new(gateway_config.routes_config_path.clone());

    // Initialize circuit breaker with config
    let circuit_breaker = Arc::new(LocalCircuitBreaker::new(
        gateway_config.circuit_breaker.clone(),
    ));

    // Initialize load balancer
    let load_balancer = Arc::new(WeightedRoundRobin::new());

    // Initialize HTTP client for LLM providers
    let http_client = Arc::new(ReqwestLlmClient::new());

    let metrics = Arc::new(Metrics::new());

    // Initialize the semantic cache (Phase 2). Fail-open: if Redis cannot be
    // reached/configured, fall back to a no-op cache so boot never blocks.
    let semantic_cache = build_semantic_cache(&gateway_config, metrics.clone()).await;

    // Best-effort cache cleanup on model change (hot reload). Fires a background
    // SCAN+DEL of the old namespace; never on the request path; TTL covers failure.
    {
        let cache_for_hook = semantic_cache.clone();
        route_config.set_model_change_hook(Arc::new(move |path, method, old_model| {
            let cache = cache_for_hook.clone();
            tokio::spawn(async move {
                let ctx = semantic_cache::CacheKeyCtx {
                    route_path: path,
                    route_method: method,
                    model: old_model,
                    threshold: 0.0,
                    ttl_secs: 0,
                    max_entries: 0,
                };
                cache.invalidate_namespace(&ctx).await;
            });
        }));
    }

    // Build the guardrail engine (Phase 3) from the initial route config, and
    // register a reload hook so it recompiles whenever routes.yaml changes.
    let guardrails = {
        let resolver = route_config.current();
        Arc::new(guardrails::GuardrailEngine::new(
            resolver.config(),
            gateway_config.guardrails.clone(),
        ))
    };
    {
        let engine = guardrails.clone();
        route_config.set_reload_hook(Arc::new(move |cfg| engine.recompile(cfg)));
    }

    // Build shared application state with all components
    let app_state = AppState {
        route_config: Arc::new(route_config),
        gateway_config: Arc::new(gateway_config),
        rate_limiter: Arc::new(LocalTokenBucket::new()),
        load_balancer,
        circuit_breaker,
        http_client,
        metrics,
        redis_available: Arc::new(AtomicBool::new(true)),
        semantic_cache,
        outbound_guardrail: Arc::new(semantic_cache::NoopOutboundGuardrail),
        guardrails,
    };

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("Failed to bind to address");

    tracing::info!("Listening on {}", addr);

    let app = router::build_router(app_state);

    // Graceful shutdown: listen for SIGTERM (Unix) and SIGINT (Ctrl+C)
    let shutdown_signal = async move {
        #[cfg(unix)]
        {
            let mut sigterm =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                    .expect("Failed to install SIGTERM handler");

            let ctrl_c = tokio::signal::ctrl_c();

            tokio::select! {
                _ = sigterm.recv() => {
                    tracing::info!("Received SIGTERM, initiating graceful shutdown...");
                }
                _ = ctrl_c => {
                    tracing::info!("Received SIGINT (Ctrl+C), initiating graceful shutdown...");
                }
            }
        }

        #[cfg(not(unix))]
        {
            tokio::signal::ctrl_c()
                .await
                .expect("Failed to listen for Ctrl+C");
            tracing::info!("Received Ctrl+C, initiating graceful shutdown...");
        }

        tracing::info!(
            "Waiting up to {}s for in-flight requests to complete...",
            graceful_shutdown_timeout.as_secs()
        );
    };

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal)
        .await
        .expect("Server error");

    tracing::info!("Melis AI Gateway shut down gracefully.");
}

/// Builds the semantic cache backend.
///
/// Attempts to connect to Redis using `config.redis.cluster_urls[0]`. On any
/// failure (no URL, connect error), logs a warning and returns a `DisabledCache`
/// so the gateway boots and operates normally (fail-open).
async fn build_semantic_cache(
    config: &GatewayConfig,
    metrics: Arc<Metrics>,
) -> Arc<dyn semantic_cache::SemanticCache> {
    use fred::prelude::*;

    let url = match config.redis.cluster_urls.first() {
        Some(u) if !u.trim().is_empty() => u.clone(),
        _ => {
            tracing::warn!("semantic cache: no Redis URL configured; cache disabled (fail-open)");
            return Arc::new(semantic_cache::DisabledCache);
        }
    };

    let redis_config = match RedisConfig::from_url(&url) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(
                "semantic cache: invalid Redis URL '{}': {}; cache disabled",
                url,
                e
            );
            return Arc::new(semantic_cache::DisabledCache);
        }
    };

    let client = Builder::from_config(redis_config)
        .build()
        .unwrap_or_else(|_| {
            // Should not happen with a valid config, but stay fail-open.
            panic!("unreachable: redis client build")
        });

    // Try to connect; if it fails quickly, degrade to DisabledCache.
    if client.connect().await.is_err() || client.wait_for_connect().await.is_err() {
        tracing::warn!(
            "semantic cache: could not connect to Redis at {}; cache disabled (fail-open)",
            url
        );
        return Arc::new(semantic_cache::DisabledCache);
    }

    tracing::info!("semantic cache: connected to Redis at {}", url);
    let hook: Arc<dyn Fn(&str, i64) + Send + Sync> = {
        let metrics = metrics.clone();
        Arc::new(move |namespace: &str, count: i64| {
            metrics
                .cache_entries
                .with_label_values(&[namespace])
                .set(count);
        })
    };
    Arc::new(semantic_cache::RedisSemanticCache::new(client).with_entry_count_hook(hook))
}

/// Provides a minimal development configuration when config.yaml is not available.
///
/// This allows `cargo run` to work out of the box for local development.
fn default_dev_config() -> GatewayConfig {
    let yaml = r#"
server:
  host: "0.0.0.0"
  port: 8080
redis:
  cluster_urls:
    - "redis://localhost:6379"
routes_config_path: "./routes.yaml"
"#;
    // Use from_yaml which also applies env overrides and validates
    GatewayConfig::from_yaml(yaml).expect("Default dev config is invalid")
}
