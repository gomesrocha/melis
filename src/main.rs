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
mod redis_client;
mod route_config;
mod router;
mod routing;
mod semantic_cache;
mod semantic_scoring;
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

    // Initialize load balancer
    let load_balancer = Arc::new(WeightedRoundRobin::new());

    // Initialize HTTP client for LLM providers
    let http_client = Arc::new(ReqwestLlmClient::new());

    let metrics = Arc::new(Metrics::new());

    // Shared Redis client (Phase 4): one pool reused by cache and circuit breaker.
    // The client exists whenever a Redis URL is configured, even if Redis is down
    // at boot; it reconnects in the background. Boot never blocks on Redis.
    let shared_redis = redis_client::SharedRedis::connect(&gateway_config.redis).await;
    let redis_healthy = shared_redis.is_healthy().await;
    let redis_available = Arc::new(AtomicBool::new(redis_healthy));
    metrics.redis_up.set(redis_healthy as i64);
    {
        // Keep `redis_available` (/readyz) and `melis_redis_up` real over time.
        let flag = redis_available.clone();
        let redis = shared_redis.clone();
        let metrics = metrics.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                let healthy = redis.is_healthy().await;
                flag.store(healthy, std::sync::atomic::Ordering::Relaxed);
                metrics.redis_up.set(healthy as i64);
            }
        });
    }

    // Select circuit breaker backend (default local; backward compatible).
    let circuit_breaker: Arc<dyn circuit_breaker::CircuitBreaker> =
        match gateway_config.circuit_breaker.backend {
            config::CircuitBreakerBackend::Redis => match shared_redis.client() {
                Some(c) => {
                    tracing::info!("circuit breaker backend: redis (shared state)");
                    metrics
                        .circuit_breaker_backend
                        .with_label_values(&["redis"])
                        .set(1);
                    Arc::new(circuit_breaker::RedisCircuitBreaker::new(
                        c.clone(),
                        gateway_config.circuit_breaker.clone(),
                        metrics.clone(),
                    ))
                }
                None => {
                    // Reached ONLY when no usable Redis URL is configured (a config
                    // problem). A Redis that is merely down/unreachable still selects
                    // the Redis backend: it fails open and recovers by itself.
                    tracing::warn!(
                        "circuit_breaker.backend=redis but no usable Redis URL is configured; \
                         using the local breaker"
                    );
                    metrics
                        .circuit_breaker_backend
                        .with_label_values(&["local"])
                        .set(1);
                    Arc::new(LocalCircuitBreaker::new(
                        gateway_config.circuit_breaker.clone(),
                    ))
                }
            },
            config::CircuitBreakerBackend::Local => {
                metrics
                    .circuit_breaker_backend
                    .with_label_values(&["local"])
                    .set(1);
                Arc::new(LocalCircuitBreaker::new(
                    gateway_config.circuit_breaker.clone(),
                ))
            }
        };

    // Initialize the semantic cache (Phase 2), reusing the shared Redis client.
    let semantic_cache = build_semantic_cache(shared_redis.client(), metrics.clone());

    // Injectable embedding provider (Phase 4): production uses Ollama.
    let embeddings: Arc<dyn embeddings::EmbeddingProvider> =
        Arc::new(embeddings::OllamaEmbeddingProvider::new(
            gateway_config.providers.clone(),
            http_client.clone(),
        ));

    // Best-effort cache cleanup on model change (hot reload). Fires a background
    // SCAN+DEL of the old namespace; never on the request path; TTL covers failure.
    {
        let cache_for_hook = semantic_cache.clone();
        route_config.set_model_change_hook(Arc::new(move |path, method, old_model| {
            let cache = cache_for_hook.clone();
            tokio::spawn(async move {
                // Salt is unknown here; invalidate_namespace clears every salt for
                // this (path, method, model) by glob, so an empty salt is fine.
                let ctx = semantic_cache::CacheKeyCtx {
                    route_path: path,
                    route_method: method,
                    model: old_model,
                    key_salt: String::new(),
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
        redis_available,
        semantic_cache,
        outbound_guardrail: Arc::new(semantic_cache::NoopOutboundGuardrail),
        guardrails,
        embeddings,
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

/// Builds the semantic cache backend from the shared Redis client (Phase 4).
///
/// When the shared client is `None` (Redis unavailable), returns a `DisabledCache`
/// so the gateway operates normally (fail-open).
fn build_semantic_cache(
    client: Option<&fred::clients::RedisClient>,
    metrics: Arc<Metrics>,
) -> Arc<dyn semantic_cache::SemanticCache> {
    match client {
        Some(c) => {
            let hook: semantic_cache::EntryCountHook = {
                let metrics = metrics.clone();
                Arc::new(move |namespace: &str, count: i64| {
                    metrics
                        .cache_entries
                        .with_label_values(&[namespace])
                        .set(count);
                })
            };
            Arc::new(semantic_cache::RedisSemanticCache::new(c.clone()).with_entry_count_hook(hook))
        }
        None => {
            tracing::warn!("semantic cache: Redis unavailable; cache disabled (fail-open)");
            Arc::new(semantic_cache::DisabledCache)
        }
    }
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
