//! Rate limiter middleware.
//!
//! Distributed Token Bucket rate limiting backed by Redis.
//! Provides both a Redis-backed implementation for production
//! and an in-memory implementation for testing/development.

use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Instant;

/// Configuration for rate limiting per client.
#[derive(Debug, Clone)]
pub struct RateLimitConfig {
    /// Maximum burst capacity (tokens in bucket).
    pub burst_capacity: u64,
    /// Tokens refilled per second.
    pub refill_rate: f64,
}

/// Error returned when rate limit is exceeded.
#[derive(Debug, Clone)]
pub struct RateLimitExceeded {
    /// Seconds the client should wait before retrying.
    pub retry_after_secs: u64,
}

/// Distributed rate limiter trait.
///
/// Implementations use the Token Bucket algorithm to control
/// request rates per client.
#[async_trait]
pub trait RateLimiter: Send + Sync {
    /// Attempts to consume one token from the client's bucket.
    ///
    /// Returns `Ok(remaining)` with the number of remaining tokens,
    /// or `Err(RateLimitExceeded)` with the wait time in seconds.
    async fn try_acquire(
        &self,
        client_id: &str,
        config: &RateLimitConfig,
    ) -> Result<u64, RateLimitExceeded>;
}

/// Lua script for atomic token bucket operations in Redis.
///
/// KEYS[1] = bucket key (melis:rl:{client_id})
/// ARGV[1] = capacity
/// ARGV[2] = refill_rate
/// ARGV[3] = now_ms (current time in milliseconds)
///
/// Returns: {1, remaining} on success, {0, wait_seconds} on rate limit.
const TOKEN_BUCKET_LUA: &str = r#"
local tokens = tonumber(redis.call('HGET', KEYS[1], 'tokens') or ARGV[1])
local last = tonumber(redis.call('HGET', KEYS[1], 'last_refill') or ARGV[3])
local elapsed = (tonumber(ARGV[3]) - last) / 1000.0
local refilled = math.min(tonumber(ARGV[1]), tokens + elapsed * tonumber(ARGV[2]))
if refilled >= 1 then
    redis.call('HSET', KEYS[1], 'tokens', refilled - 1, 'last_refill', ARGV[3])
    redis.call('EXPIRE', KEYS[1], 3600)
    return {1, math.floor(refilled - 1)}
else
    local wait = math.ceil((1 - refilled) / tonumber(ARGV[2]))
    return {0, wait}
end
"#;

/// Redis-backed Token Bucket rate limiter using atomic Lua scripts.
///
/// Uses the `fred` crate for Redis cluster communication.
/// Each client gets a Redis HASH at key `melis:rl:{client_id}` with
/// fields `tokens` (remaining) and `last_refill` (timestamp ms),
/// with a TTL of 3600 seconds.
pub struct RedisTokenBucket {
    client: fred::clients::RedisClient,
}

impl RedisTokenBucket {
    /// Creates a new Redis-backed rate limiter.
    pub fn new(client: fred::clients::RedisClient) -> Self {
        Self { client }
    }

    /// Returns the Redis key for a given client.
    fn bucket_key(client_id: &str) -> String {
        format!("melis:rl:{}", client_id)
    }
}

#[async_trait]
impl RateLimiter for RedisTokenBucket {
    async fn try_acquire(
        &self,
        client_id: &str,
        config: &RateLimitConfig,
    ) -> Result<u64, RateLimitExceeded> {
        use fred::interfaces::LuaInterface;

        let key = Self::bucket_key(client_id);
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        let result: Vec<i64> = self
            .client
            .eval(
                TOKEN_BUCKET_LUA,
                vec![key.as_str()],
                vec![
                    config.burst_capacity.to_string(),
                    config.refill_rate.to_string(),
                    now_ms.to_string(),
                ],
            )
            .await
            .map_err(|e| {
                tracing::warn!("Redis rate limiter error, failing open: {}", e);
                // Fail-open: allow request if Redis is unavailable
                return RateLimitExceeded { retry_after_secs: 0 };
            })?;

        if result.len() >= 2 && result[0] == 1 {
            Ok(result[1] as u64)
        } else if result.len() >= 2 {
            Err(RateLimitExceeded {
                retry_after_secs: result[1] as u64,
            })
        } else {
            // Unexpected response format, fail-open
            tracing::warn!("Unexpected Redis response format, allowing request");
            Ok(0)
        }
    }
}

/// In-memory Token Bucket rate limiter for testing and development.
///
/// Uses the same algorithm as the Lua script but stores state locally.
/// Not suitable for distributed deployments (no cross-instance coordination).
pub struct LocalTokenBucket {
    state: Mutex<HashMap<String, BucketState>>,
}

/// Internal state for a single token bucket.
#[derive(Debug, Clone)]
struct BucketState {
    tokens: f64,
    last_refill: Instant,
}

impl LocalTokenBucket {
    /// Creates a new in-memory rate limiter.
    pub fn new() -> Self {
        Self {
            state: Mutex::new(HashMap::new()),
        }
    }
}

impl Default for LocalTokenBucket {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl RateLimiter for LocalTokenBucket {
    async fn try_acquire(
        &self,
        client_id: &str,
        config: &RateLimitConfig,
    ) -> Result<u64, RateLimitExceeded> {
        let mut state = self.state.lock().unwrap();
        let now = Instant::now();

        let bucket = state
            .entry(client_id.to_string())
            .or_insert_with(|| BucketState {
                tokens: config.burst_capacity as f64,
                last_refill: now,
            });

        // Calculate elapsed time and refill tokens
        let elapsed_secs = bucket.last_refill.elapsed().as_secs_f64();
        let refilled = f64::min(
            config.burst_capacity as f64,
            bucket.tokens + elapsed_secs * config.refill_rate,
        );

        if refilled >= 1.0 {
            bucket.tokens = refilled - 1.0;
            bucket.last_refill = now;
            Ok(refilled as u64 - 1)
        } else {
            // Calculate wait time: how long until we have 1 token
            let wait = ((1.0 - refilled) / config.refill_rate).ceil() as u64;
            Err(RateLimitExceeded {
                retry_after_secs: wait,
            })
        }
    }
}

// ─── Axum Middleware ────────────────────────────────────────────────────────────

use axum::{extract::Request, middleware::Next, response::Response};

use crate::error::GatewayError;
use crate::middleware::auth::ClientIdentity;
use crate::state::AppState;

/// Axum middleware that enforces per-client rate limiting.
///
/// Pipeline:
/// 1. Extract `ClientIdentity` from request extensions (set by auth middleware).
/// 2. Determine the effective `RateLimitConfig` for this client (per-client override or global defaults).
/// 3. Call `rate_limiter.try_acquire(client_id, &config)`.
/// 4. On success: add `X-RateLimit-Remaining` header and proceed.
/// 5. On rate limit exceeded (with retry_after_secs > 0): return HTTP 429.
/// 6. Fail-open: if retry_after_secs == 0 (Redis unavailable), allow request through.
#[tracing::instrument(skip(state, request, next), fields(middleware = "rate_limit"))]
pub async fn rate_limit_middleware(
    axum::extract::State(state): axum::extract::State<AppState>,
    request: Request,
    next: Next,
) -> Result<Response, GatewayError> {
    // Extract client identity (set by auth middleware earlier in the pipeline)
    let identity = request
        .extensions()
        .get::<ClientIdentity>()
        .cloned();

    let identity = match identity {
        Some(id) => id,
        None => {
            // No identity means auth middleware didn't run or this is an unauthenticated route.
            // Allow through without rate limiting.
            return Ok(next.run(request).await);
        }
    };

    // Determine effective rate limit config for this client
    let config = match &identity.rate_limit_config {
        Some(client_rl) => RateLimitConfig {
            burst_capacity: client_rl.burst_capacity,
            refill_rate: client_rl.refill_rate,
        },
        None => RateLimitConfig {
            burst_capacity: state.gateway_config.rate_limit.burst_capacity,
            refill_rate: state.gateway_config.rate_limit.refill_rate,
        },
    };

    // Attempt to acquire a token
    match state.rate_limiter.try_acquire(&identity.client_id, &config).await {
        Ok(remaining) => {
            // Request allowed — proceed and add informational header
            let mut response = next.run(request).await;
            response.headers_mut().insert(
                "X-RateLimit-Remaining",
                remaining.to_string().parse().unwrap(),
            );
            Ok(response)
        }
        Err(exceeded) => {
            if exceeded.retry_after_secs == 0 {
                // Fail-open: Redis was unavailable, the implementation already logged a warning.
                // Allow the request through without rate limit enforcement.
                tracing::warn!(
                    client_id = %identity.client_id,
                    "Rate limiter fail-open: allowing request due to backend unavailability"
                );
                Ok(next.run(request).await)
            } else {
                // Rate limit exceeded — return HTTP 429
                Err(GatewayError::RateLimited {
                    retry_after_secs: exceeded.retry_after_secs,
                })
            }
        }
    }
}

#[cfg(test)]
mod proptest_tests {
    use super::*;
    use proptest::prelude::*;

    /// **Validates: Requirements 6.1, 6.2, 6.4**
    ///
    /// Property 13: Corretude do Token Bucket
    /// For any (capacity, refill_rate) config and sequences of acquire calls:
    /// - allows when tokens > 0
    /// - rejects when exhausted with correct retry_after
    /// - tokens never go negative
    /// - different clients are independent
    mod property_token_bucket {
        use super::*;

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(100))]

            #[test]
            fn allows_up_to_capacity_then_rejects(
                capacity in 1u64..20,
                refill_rate in 0.1f64..5.0,
            ) {
                let limiter = LocalTokenBucket::new();
                let config = RateLimitConfig {
                    burst_capacity: capacity,
                    refill_rate,
                };

                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();

                rt.block_on(async {
                    // Should allow exactly `capacity` requests
                    for i in 0..capacity {
                        let result = limiter.try_acquire("test-client", &config).await;
                        prop_assert!(
                            result.is_ok(),
                            "Request {} of {} should succeed, got: {:?}",
                            i + 1, capacity, result
                        );
                        let remaining = result.unwrap();
                        prop_assert_eq!(remaining, capacity - i - 1);
                    }

                    // Next request should be rejected
                    let result = limiter.try_acquire("test-client", &config).await;
                    prop_assert!(
                        result.is_err(),
                        "Request after exhausting capacity should fail"
                    );
                    let err = result.unwrap_err();
                    prop_assert!(
                        err.retry_after_secs >= 1,
                        "retry_after_secs should be >= 1, got: {}",
                        err.retry_after_secs
                    );

                    Ok(())
                })?;
            }

            #[test]
            fn tokens_never_go_negative(
                capacity in 1u64..10,
                num_requests in 1usize..30,
            ) {
                let limiter = LocalTokenBucket::new();
                let config = RateLimitConfig {
                    burst_capacity: capacity,
                    refill_rate: 0.001, // very slow refill to avoid refill during test
                };

                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();

                rt.block_on(async {
                    let mut successes = 0u64;
                    for _ in 0..num_requests {
                        match limiter.try_acquire("neg-client", &config).await {
                            Ok(_remaining) => {
                                successes += 1;
                            }
                            Err(_) => {
                                // All subsequent requests should also fail (no refill happening)
                            }
                        }
                    }
                    // Can never consume more tokens than capacity
                    prop_assert!(
                        successes <= capacity,
                        "Successes ({}) exceeded capacity ({})",
                        successes, capacity
                    );

                    Ok(())
                })?;
            }

            #[test]
            fn different_clients_are_independent(
                capacity in 2u64..10,
            ) {
                let limiter = LocalTokenBucket::new();
                let config = RateLimitConfig {
                    burst_capacity: capacity,
                    refill_rate: 0.001,
                };

                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();

                rt.block_on(async {
                    // Exhaust client-A
                    for _ in 0..capacity {
                        limiter.try_acquire("client-A", &config).await.unwrap();
                    }
                    let result_a = limiter.try_acquire("client-A", &config).await;
                    prop_assert!(result_a.is_err(), "client-A should be exhausted");

                    // client-B should still have full capacity
                    let result_b = limiter.try_acquire("client-B", &config).await;
                    prop_assert!(result_b.is_ok(), "client-B should have tokens");
                    let remaining = result_b.unwrap();
                    prop_assert_eq!(remaining, capacity - 1);

                    Ok(())
                })?;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    use axum::{
        body::Body,
        http::{Request as HttpRequest, StatusCode},
        middleware,
        routing::get,
        Router,
    };
    use tower::ServiceExt;

    use crate::config::{ClientRateLimit, GatewayConfig};
    use crate::middleware::auth::ClientIdentity;
    use crate::observability::Metrics;
    use crate::route_config::RouteConfigManager;
    use crate::state::AppState;

    /// Builds a test app with rate limiter middleware and a configurable rate limit.
    fn build_rate_limit_test_app(burst: u64, refill: f64) -> Router {
        let limiter = Arc::new(LocalTokenBucket::new());
        let gateway_config = Arc::new(test_gateway_config(burst, refill));
        let route_config = Arc::new(RouteConfigManager::new_for_test());

        let state = AppState {
            route_config,
            gateway_config,
            rate_limiter: limiter,
            load_balancer: Arc::new(crate::balancer::WeightedRoundRobin::new()),
            circuit_breaker: Arc::new(crate::circuit_breaker::LocalCircuitBreaker::new(Default::default())),
            http_client: Arc::new(crate::client::ReqwestLlmClient::new()),
            metrics: Arc::new(Metrics::new()),
            redis_available: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        };

        Router::new()
            .route("/test", get(|| async { "OK" }))
            .layer(middleware::from_fn_with_state(
                state.clone(),
                rate_limit_middleware,
            ))
            .with_state(state)
    }

    /// Creates a minimal GatewayConfig for testing rate limiting.
    fn test_gateway_config(burst: u64, refill: f64) -> GatewayConfig {
        let yaml = format!(
            r#"
server:
  host: "0.0.0.0"
  port: 8080
redis:
  cluster_urls:
    - "redis://localhost:6379"
rate_limit:
  burst_capacity: {}
  refill_rate: {}
"#,
            burst, refill
        );
        GatewayConfig::from_yaml(&yaml).unwrap()
    }

    /// Helper to create a request with ClientIdentity extension.
    fn request_with_identity(client_id: &str, rate_limit: Option<ClientRateLimit>) -> HttpRequest<Body> {
        let mut req = HttpRequest::builder()
            .uri("/test")
            .body(Body::empty())
            .unwrap();

        req.extensions_mut().insert(ClientIdentity {
            client_id: client_id.to_string(),
            rate_limit_config: rate_limit,
            allowed_models: vec![],
        });

        req
    }

    #[tokio::test]
    async fn test_middleware_allows_request_within_limit() {
        let app = build_rate_limit_test_app(10, 2.0);

        let request = request_with_identity("client-1", None);
        let response = app.oneshot(request).await.unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        // Should have X-RateLimit-Remaining header
        let remaining = response
            .headers()
            .get("X-RateLimit-Remaining")
            .unwrap()
            .to_str()
            .unwrap();
        assert_eq!(remaining, "9"); // 10 burst - 1 consumed = 9
    }

    #[tokio::test]
    async fn test_middleware_returns_429_when_limit_exceeded() {
        let app = build_rate_limit_test_app(2, 1.0);

        // Exhaust all tokens (burst_capacity = 2)
        let request = request_with_identity("client-1", None);
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let request = request_with_identity("client-1", None);
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // Third request should be rate limited
        let request = request_with_identity("client-1", None);
        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);

        // Should have Retry-After header
        let retry_after = response
            .headers()
            .get("Retry-After")
            .unwrap()
            .to_str()
            .unwrap();
        assert!(retry_after.parse::<u64>().unwrap() >= 1);
    }

    #[tokio::test]
    async fn test_middleware_uses_client_specific_rate_limit() {
        // Global config has burst=10, but client override has burst=1
        let app = build_rate_limit_test_app(10, 2.0);

        let client_rl = ClientRateLimit {
            burst_capacity: 1,
            refill_rate: 1.0,
        };

        // First request with client override: 1 token available, consumed
        let request = request_with_identity("client-override", Some(client_rl.clone()));
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // Second request should be rate limited (only 1 token capacity)
        let request = request_with_identity("client-override", Some(client_rl));
        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    }

    #[tokio::test]
    async fn test_middleware_allows_request_without_identity() {
        let app = build_rate_limit_test_app(1, 1.0);

        // Request without ClientIdentity extension — should pass through
        let request = HttpRequest::builder()
            .uri("/test")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_middleware_fail_open_on_zero_retry_after() {
        // Create a custom limiter that always returns Err with retry_after_secs = 0
        // (simulating Redis unavailability fail-open behavior)
        struct FailOpenLimiter;

        #[async_trait]
        impl RateLimiter for FailOpenLimiter {
            async fn try_acquire(
                &self,
                _client_id: &str,
                _config: &RateLimitConfig,
            ) -> Result<u64, RateLimitExceeded> {
                Err(RateLimitExceeded { retry_after_secs: 0 })
            }
        }

        let gateway_config = Arc::new(test_gateway_config(10, 2.0));
        let route_config = Arc::new(RouteConfigManager::new_for_test());

        let state = AppState {
            route_config,
            gateway_config,
            rate_limiter: Arc::new(FailOpenLimiter),
            load_balancer: Arc::new(crate::balancer::WeightedRoundRobin::new()),
            circuit_breaker: Arc::new(crate::circuit_breaker::LocalCircuitBreaker::new(Default::default())),
            http_client: Arc::new(crate::client::ReqwestLlmClient::new()),
            metrics: Arc::new(Metrics::new()),
            redis_available: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        };

        let app = Router::new()
            .route("/test", get(|| async { "OK" }))
            .layer(middleware::from_fn_with_state(
                state.clone(),
                rate_limit_middleware,
            ))
            .with_state(state);

        let request = request_with_identity("client-1", None);
        let response = app.oneshot(request).await.unwrap();

        // Should pass through (fail-open) instead of returning 429
        assert_eq!(response.status(), StatusCode::OK);
    }

    // ─── Unit tests for LocalTokenBucket ─────────────────────────────────────────

    fn default_config() -> RateLimitConfig {
        RateLimitConfig {
            burst_capacity: 10,
            refill_rate: 2.0, // 2 tokens per second
        }
    }

    #[tokio::test]
    async fn test_first_request_succeeds() {
        let limiter = LocalTokenBucket::new();
        let config = default_config();

        let result = limiter.try_acquire("client-1", &config).await;
        assert!(result.is_ok());
        // First request: starts with 10 tokens, consumes 1 → 9 remaining
        assert_eq!(result.unwrap(), 9);
    }

    #[tokio::test]
    async fn test_burst_capacity_consumed() {
        let limiter = LocalTokenBucket::new();
        let config = RateLimitConfig {
            burst_capacity: 5,
            refill_rate: 1.0,
        };

        // Consume all 5 tokens
        for i in (0..5).rev() {
            let result = limiter.try_acquire("client-1", &config).await;
            assert!(result.is_ok(), "Request {} should succeed", 5 - i);
            assert_eq!(result.unwrap(), i);
        }

        // 6th request should be rate limited
        let result = limiter.try_acquire("client-1", &config).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.retry_after_secs >= 1);
    }

    #[tokio::test]
    async fn test_different_clients_independent() {
        let limiter = LocalTokenBucket::new();
        let config = RateLimitConfig {
            burst_capacity: 2,
            refill_rate: 1.0,
        };

        // Exhaust client-1
        limiter.try_acquire("client-1", &config).await.unwrap();
        limiter.try_acquire("client-1", &config).await.unwrap();
        let result = limiter.try_acquire("client-1", &config).await;
        assert!(result.is_err());

        // client-2 should still have tokens
        let result = limiter.try_acquire("client-2", &config).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 1);
    }

    #[tokio::test]
    async fn test_tokens_refill_over_time() {
        let limiter = LocalTokenBucket::new();
        let config = RateLimitConfig {
            burst_capacity: 2,
            refill_rate: 10.0, // 10 tokens/sec → refills fast
        };

        // Exhaust all tokens
        limiter.try_acquire("client-1", &config).await.unwrap();
        limiter.try_acquire("client-1", &config).await.unwrap();
        let result = limiter.try_acquire("client-1", &config).await;
        assert!(result.is_err());

        // Wait for refill (at 10 tokens/sec, 100ms gives ~1 token)
        tokio::time::sleep(Duration::from_millis(150)).await;

        // Should have refilled at least 1 token
        let result = limiter.try_acquire("client-1", &config).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_tokens_capped_at_burst_capacity() {
        let limiter = LocalTokenBucket::new();
        let config = RateLimitConfig {
            burst_capacity: 3,
            refill_rate: 100.0, // Very high refill rate
        };

        // Use one token
        limiter.try_acquire("client-1", &config).await.unwrap();

        // Wait long enough to "overfill" if uncapped
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Should still be capped at burst_capacity (3)
        // First call: consumes 1 from 3 → 2 remaining
        let result = limiter.try_acquire("client-1", &config).await;
        assert!(result.is_ok());
        assert!(result.unwrap() <= 2); // Cannot exceed burst_capacity - 1
    }

    #[tokio::test]
    async fn test_retry_after_calculation() {
        let limiter = LocalTokenBucket::new();
        let config = RateLimitConfig {
            burst_capacity: 1,
            refill_rate: 1.0, // 1 token/sec
        };

        // Consume the only token
        limiter.try_acquire("client-1", &config).await.unwrap();

        // Next request should fail with retry_after = 1 second
        let result = limiter.try_acquire("client-1", &config).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.retry_after_secs, 1);
    }

    #[tokio::test]
    async fn test_rate_limit_config_respected() {
        let limiter = LocalTokenBucket::new();

        // Client with high capacity
        let high_config = RateLimitConfig {
            burst_capacity: 100,
            refill_rate: 10.0,
        };

        // Client with low capacity
        let low_config = RateLimitConfig {
            burst_capacity: 2,
            refill_rate: 1.0,
        };

        // High capacity client can make many requests
        for _ in 0..50 {
            let result = limiter.try_acquire("high-client", &high_config).await;
            assert!(result.is_ok());
        }

        // Low capacity client gets limited quickly
        limiter.try_acquire("low-client", &low_config).await.unwrap();
        limiter.try_acquire("low-client", &low_config).await.unwrap();
        let result = limiter.try_acquire("low-client", &low_config).await;
        assert!(result.is_err());
    }
}
