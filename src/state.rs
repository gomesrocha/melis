//! Application state module.
//!
//! Defines the shared application state (`AppState`) that is passed
//! to all Axum handlers via state extraction. Holds references to
//! the route configuration manager and gateway configuration.

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use crate::balancer::LoadBalancer;
use crate::circuit_breaker::CircuitBreaker;
use crate::client::LlmHttpClient;
use crate::config::GatewayConfig;
use crate::middleware::rate_limiter::RateLimiter;
use crate::observability::Metrics;
use crate::route_config::RouteConfigManager;

/// Shared application state accessible by all request handlers.
///
/// Wrapped in `Arc` when passed to Axum for cheap cloning across tasks.
#[derive(Clone)]
pub struct AppState {
    /// Route configuration manager with hot-reload support.
    pub route_config: Arc<RouteConfigManager>,
    /// Global gateway configuration.
    pub gateway_config: Arc<GatewayConfig>,
    /// Rate limiter instance (distributed or local).
    pub rate_limiter: Arc<dyn RateLimiter>,
    /// Load balancer for weighted provider selection.
    pub load_balancer: Arc<dyn LoadBalancer>,
    /// Circuit breaker for provider health tracking.
    pub circuit_breaker: Arc<dyn CircuitBreaker>,
    /// HTTP client for forwarding requests to LLM providers.
    pub http_client: Arc<dyn LlmHttpClient>,
    /// Prometheus metrics registry and metric instances.
    pub metrics: Arc<Metrics>,
    /// Flag indicating whether Redis is reachable.
    /// Used by the readiness probe (`/readyz`).
    // TODO: Ping Redis connection pool to update this flag periodically.
    pub redis_available: Arc<AtomicBool>,
}
