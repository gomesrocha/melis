pub mod balancer;
pub mod circuit_breaker;
pub mod client;
mod compactor;
mod config;
mod error;
mod models;
pub mod observability;
mod route_config;
mod router;
mod routing;
mod state;

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
        eprintln!("Failed to initialize tracing: {}. Falling back to basic fmt.", e);
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

    let addr = std::net::SocketAddr::from((
        [0, 0, 0, 0],
        gateway_config.server.port,
    ));

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

    // Build shared application state with all components
    let app_state = AppState {
        route_config: Arc::new(route_config),
        gateway_config: Arc::new(gateway_config),
        rate_limiter: Arc::new(LocalTokenBucket::new()),
        load_balancer,
        circuit_breaker,
        http_client,
        metrics: Arc::new(Metrics::new()),
        redis_available: Arc::new(AtomicBool::new(true)),
    };

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("Failed to bind to address");

    tracing::info!("Listening on {}", addr);

    let app = router::build_router(app_state);

    // Graceful shutdown: listen for SIGTERM and SIGINT (Ctrl+C)
    let shutdown_signal = async move {
        let mut sigterm = tokio::signal::unix::signal(
            tokio::signal::unix::SignalKind::terminate(),
        )
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
