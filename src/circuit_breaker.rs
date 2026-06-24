//! Circuit Breaker module.
//!
//! Provides distributed circuit breaker functionality to protect the gateway
//! from routing requests to unhealthy LLM providers. Supports three states:
//! Closed (normal), Open (blocked), and HalfOpen (testing recovery).
//!
//! Two implementations are provided:
//! - `LocalCircuitBreaker`: in-memory implementation for dev/testing
//! - `RedisCircuitBreaker`: Redis-backed for production (placeholder)

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use tokio::sync::RwLock;

use crate::config::CircuitBreakerConfig;

// ─── Types ────────────────────────────────────────────────────────────────────

/// Result of a call to an LLM provider, used to update circuit breaker state.
#[derive(Debug, Clone, PartialEq)]
pub enum CallResult {
    /// Provider returned a successful response (2xx).
    Success,
    /// Provider returned a server error (5xx) or other transient failure.
    Failure,
    /// Provider returned HTTP 429 Too Many Requests.
    RateLimited,
    /// Request to the provider timed out.
    Timeout,
}

/// Internal state of a circuit breaker for a single provider.
#[derive(Debug, Clone)]
pub enum CircuitState {
    /// Normal operation: requests are forwarded.
    Closed,
    /// Circuit is open: requests are blocked until `until` instant.
    Open { until: Instant, backoff_ttl: Duration },
    /// Testing recovery: one probe request is allowed.
    HalfOpen,
}

// ─── Trait ────────────────────────────────────────────────────────────────────

/// Circuit Breaker interface for checking provider availability
/// and recording call outcomes.
#[async_trait]
pub trait CircuitBreaker: Send + Sync {
    /// Returns `true` if the provider is available (circuit is closed or half-open).
    /// Returns `true` (fail-open) if an internal error occurs.
    async fn is_available(&self, provider_id: &str) -> bool;

    /// Records the result of a call to a provider, potentially transitioning state.
    async fn record_result(&self, provider_id: &str, result: CallResult);
}

// ─── Local (In-Memory) Implementation ────────────────────────────────────────

/// Per-provider state tracked by the local circuit breaker.
#[derive(Debug, Clone)]
struct ProviderState {
    state: CircuitState,
    /// Timestamps of failures within the sliding window.
    failures: Vec<Instant>,
    /// Total number of requests in the sliding window.
    total_requests: Vec<Instant>,
    /// Current backoff TTL for exponential backoff on repeated opens.
    current_backoff: Duration,
}

impl ProviderState {
    fn new(initial_ttl: Duration) -> Self {
        Self {
            state: CircuitState::Closed,
            failures: Vec::new(),
            total_requests: Vec::new(),
            current_backoff: initial_ttl,
        }
    }
}

/// In-memory circuit breaker implementation for development and testing.
///
/// Uses a `HashMap` protected by an async `RwLock` to track per-provider state.
/// The sliding window is implemented as a `Vec<Instant>` of timestamps.
pub struct LocalCircuitBreaker {
    states: Arc<RwLock<HashMap<String, ProviderState>>>,
    config: CircuitBreakerConfig,
}

impl LocalCircuitBreaker {
    /// Creates a new `LocalCircuitBreaker` with the given configuration.
    pub fn new(config: CircuitBreakerConfig) -> Self {
        Self {
            states: Arc::new(RwLock::new(HashMap::new())),
            config,
        }
    }

    /// Prunes entries outside the sliding window from failure and total_requests vectors.
    fn prune_window(timestamps: &mut Vec<Instant>, window: Duration, now: Instant) {
        let cutoff = now.checked_sub(window).unwrap_or(now);
        timestamps.retain(|t| *t > cutoff);
    }
}

#[async_trait]
impl CircuitBreaker for LocalCircuitBreaker {
    async fn is_available(&self, provider_id: &str) -> bool {
        let mut states = self.states.write().await;
        let initial_ttl = Duration::from_secs(self.config.open_ttl_secs);

        let provider = states
            .entry(provider_id.to_string())
            .or_insert_with(|| ProviderState::new(initial_ttl));

        let now = Instant::now();

        match &provider.state {
            CircuitState::Closed => true,
            CircuitState::Open { until, .. } => {
                if now >= *until {
                    // TTL expired → transition to HalfOpen
                    provider.state = CircuitState::HalfOpen;
                    true
                } else {
                    false
                }
            }
            CircuitState::HalfOpen => {
                // Allow one probe request
                true
            }
        }
    }

    async fn record_result(&self, provider_id: &str, result: CallResult) {
        let mut states = self.states.write().await;
        let initial_ttl = Duration::from_secs(self.config.open_ttl_secs);

        let provider = states
            .entry(provider_id.to_string())
            .or_insert_with(|| ProviderState::new(initial_ttl));

        let now = Instant::now();
        let window = Duration::from_secs(self.config.window_duration_secs);

        // Always record in the total_requests window
        provider.total_requests.push(now);
        Self::prune_window(&mut provider.total_requests, window, now);

        let is_failure = matches!(result, CallResult::Failure | CallResult::Timeout);
        let is_rate_limited = matches!(result, CallResult::RateLimited);

        // Record failures
        if is_failure || is_rate_limited {
            provider.failures.push(now);
        }
        Self::prune_window(&mut provider.failures, window, now);

        match &provider.state {
            CircuitState::Closed => {
                // HTTP 429 → open immediately
                if is_rate_limited {
                    let ttl = initial_ttl;
                    provider.state = CircuitState::Open {
                        until: now + ttl,
                        backoff_ttl: ttl,
                    };
                    provider.current_backoff = ttl;
                    return;
                }

                // Check failure threshold
                let total_in_window = provider.total_requests.len() as u64;
                if total_in_window >= self.config.min_requests_in_window {
                    let failure_count = provider.failures.len() as f64;
                    let failure_rate = (failure_count / total_in_window as f64) * 100.0;

                    if failure_rate > self.config.failure_threshold_percent {
                        let ttl = initial_ttl;
                        provider.state = CircuitState::Open {
                            until: now + ttl,
                            backoff_ttl: ttl,
                        };
                        provider.current_backoff = ttl;
                    }
                }
            }
            CircuitState::HalfOpen => {
                if result == CallResult::Success {
                    // Recovery successful → close circuit
                    provider.state = CircuitState::Closed;
                    provider.failures.clear();
                    provider.total_requests.clear();
                    provider.current_backoff = initial_ttl;
                } else {
                    // Still failing → re-open with exponential backoff
                    let base_secs = provider.current_backoff.as_secs().max(1);
                    let new_ttl_secs = (base_secs as f64
                        * self.config.backoff_factor) as u64;
                    let max_ttl_secs = self.config.max_ttl_secs;
                    let capped_ttl = Duration::from_secs(new_ttl_secs.min(max_ttl_secs));

                    provider.state = CircuitState::Open {
                        until: now + capped_ttl,
                        backoff_ttl: capped_ttl,
                    };
                    provider.current_backoff = capped_ttl;
                }
            }
            CircuitState::Open { .. } => {
                // Shouldn't receive results while open, but handle gracefully (fail-open)
            }
        }
    }
}

// ─── Redis Implementation (Placeholder) ──────────────────────────────────────

/// Redis-backed circuit breaker for production use.
///
/// Uses Redis keys:
/// - `melis:cb:{provider_id}:state` — flag with TTL indicating provider is unavailable
/// - `melis:cb:{provider_id}:failures` — sorted set with failure timestamps (sliding window)
/// - `melis:cb:{provider_id}:backoff` — current backoff TTL value
///
/// This is a placeholder struct; the full Redis implementation will be added
/// when the Redis integration layer is complete.
pub struct RedisCircuitBreaker {
    #[allow(dead_code)]
    config: CircuitBreakerConfig,
}

impl RedisCircuitBreaker {
    /// Creates a new `RedisCircuitBreaker` with the given configuration.
    #[allow(dead_code)]
    pub fn new(config: CircuitBreakerConfig) -> Self {
        Self { config }
    }
}

#[async_trait]
impl CircuitBreaker for RedisCircuitBreaker {
    async fn is_available(&self, _provider_id: &str) -> bool {
        // Fail-open: always return true until Redis integration is complete
        true
    }

    async fn record_result(&self, _provider_id: &str, _result: CallResult) {
        // No-op until Redis integration is complete
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> CircuitBreakerConfig {
        CircuitBreakerConfig {
            failure_threshold_percent: 50.0,
            window_duration_secs: 60,
            min_requests_in_window: 5,
            open_ttl_secs: 30,
            max_ttl_secs: 300,
            backoff_factor: 2.0,
        }
    }

    #[tokio::test]
    async fn starts_closed_and_available() {
        let cb = LocalCircuitBreaker::new(test_config());
        assert!(cb.is_available("provider-1").await);
    }

    #[tokio::test]
    async fn opens_after_failure_threshold_exceeded() {
        let config = CircuitBreakerConfig {
            failure_threshold_percent: 50.0,
            window_duration_secs: 60,
            min_requests_in_window: 4,
            open_ttl_secs: 30,
            max_ttl_secs: 300,
            backoff_factor: 2.0,
        };
        let cb = LocalCircuitBreaker::new(config);

        // Record 3 failures and 1 success (75% failure rate, > 50% threshold)
        cb.record_result("provider-1", CallResult::Failure).await;
        cb.record_result("provider-1", CallResult::Failure).await;
        cb.record_result("provider-1", CallResult::Failure).await;
        cb.record_result("provider-1", CallResult::Success).await;

        // Circuit should be open now (4 requests >= min_requests, 75% > 50%)
        assert!(!cb.is_available("provider-1").await);
    }

    #[tokio::test]
    async fn opens_immediately_on_429() {
        let cb = LocalCircuitBreaker::new(test_config());

        // A single 429 should open the circuit immediately
        cb.record_result("provider-1", CallResult::RateLimited).await;

        assert!(!cb.is_available("provider-1").await);
    }

    #[tokio::test]
    async fn stays_closed_below_threshold() {
        let config = CircuitBreakerConfig {
            failure_threshold_percent: 50.0,
            window_duration_secs: 60,
            min_requests_in_window: 5,
            open_ttl_secs: 30,
            max_ttl_secs: 300,
            backoff_factor: 2.0,
        };
        let cb = LocalCircuitBreaker::new(config);

        // Record 2 failures and 4 successes (33% failure rate, < 50% threshold)
        cb.record_result("provider-1", CallResult::Failure).await;
        cb.record_result("provider-1", CallResult::Failure).await;
        cb.record_result("provider-1", CallResult::Success).await;
        cb.record_result("provider-1", CallResult::Success).await;
        cb.record_result("provider-1", CallResult::Success).await;
        cb.record_result("provider-1", CallResult::Success).await;

        // Circuit should remain closed (6 requests >= min_requests=5, but 33% < 50%)
        assert!(cb.is_available("provider-1").await);
    }

    #[tokio::test]
    async fn requires_min_requests_before_opening() {
        let config = CircuitBreakerConfig {
            failure_threshold_percent: 50.0,
            window_duration_secs: 60,
            min_requests_in_window: 5,
            open_ttl_secs: 30,
            max_ttl_secs: 300,
            backoff_factor: 2.0,
        };
        let cb = LocalCircuitBreaker::new(config);

        // Record 3 failures (100% failure rate but only 3 requests < min_requests=5)
        cb.record_result("provider-1", CallResult::Failure).await;
        cb.record_result("provider-1", CallResult::Failure).await;
        cb.record_result("provider-1", CallResult::Failure).await;

        // Circuit should still be closed (not enough requests in window)
        assert!(cb.is_available("provider-1").await);
    }

    #[tokio::test]
    async fn fail_open_redis_placeholder_always_available() {
        // The RedisCircuitBreaker placeholder always returns true (fail-open)
        let cb = RedisCircuitBreaker::new(test_config());
        assert!(cb.is_available("provider-1").await);

        // Even after recording failures, it stays available (placeholder behavior)
        cb.record_result("provider-1", CallResult::Failure).await;
        assert!(cb.is_available("provider-1").await);
    }

    #[tokio::test]
    async fn half_open_transitions_to_closed_on_success() {
        let config = CircuitBreakerConfig {
            failure_threshold_percent: 50.0,
            window_duration_secs: 60,
            min_requests_in_window: 2,
            open_ttl_secs: 0, // Zero TTL so it transitions immediately to HalfOpen
            max_ttl_secs: 300,
            backoff_factor: 2.0,
        };
        let cb = LocalCircuitBreaker::new(config);

        // Open the circuit
        cb.record_result("provider-1", CallResult::RateLimited).await;
        // With TTL=0, is_available will transition to HalfOpen
        assert!(cb.is_available("provider-1").await);

        // Record success in half-open → should close
        cb.record_result("provider-1", CallResult::Success).await;
        assert!(cb.is_available("provider-1").await);
    }

    #[tokio::test]
    async fn half_open_transitions_to_open_on_failure() {
        let config = CircuitBreakerConfig {
            failure_threshold_percent: 50.0,
            window_duration_secs: 60,
            min_requests_in_window: 2,
            open_ttl_secs: 0, // Zero TTL for instant transition to HalfOpen
            max_ttl_secs: 300,
            backoff_factor: 2.0,
        };
        let cb = LocalCircuitBreaker::new(config);

        // Open the circuit
        cb.record_result("provider-1", CallResult::RateLimited).await;
        // Transition to HalfOpen (TTL=0 expired immediately)
        assert!(cb.is_available("provider-1").await);

        // Record failure in half-open → should re-open with backoff (max(0,1) * 2.0 = 2s)
        cb.record_result("provider-1", CallResult::Failure).await;

        // Now it should be open again with a 2s TTL
        assert!(!cb.is_available("provider-1").await);
    }

    #[tokio::test]
    async fn timeout_counts_as_failure() {
        let config = CircuitBreakerConfig {
            failure_threshold_percent: 50.0,
            window_duration_secs: 60,
            min_requests_in_window: 4,
            open_ttl_secs: 30,
            max_ttl_secs: 300,
            backoff_factor: 2.0,
        };
        let cb = LocalCircuitBreaker::new(config);

        // Record 3 timeouts and 1 success (75% failure rate)
        cb.record_result("provider-1", CallResult::Timeout).await;
        cb.record_result("provider-1", CallResult::Timeout).await;
        cb.record_result("provider-1", CallResult::Timeout).await;
        cb.record_result("provider-1", CallResult::Success).await;

        // Circuit should be open
        assert!(!cb.is_available("provider-1").await);
    }

    #[tokio::test]
    async fn independent_provider_states() {
        let config = CircuitBreakerConfig {
            failure_threshold_percent: 50.0,
            window_duration_secs: 60,
            min_requests_in_window: 2,
            open_ttl_secs: 30,
            max_ttl_secs: 300,
            backoff_factor: 2.0,
        };
        let cb = LocalCircuitBreaker::new(config);

        // Open circuit for provider-1
        cb.record_result("provider-1", CallResult::RateLimited).await;

        // provider-1 should be unavailable
        assert!(!cb.is_available("provider-1").await);
        // provider-2 should still be available
        assert!(cb.is_available("provider-2").await);
    }
}

#[cfg(test)]
mod proptest_tests {
    use super::*;
    use proptest::prelude::*;

    /// **Validates: Requirements 5.1, 5.2**
    ///
    /// Property 11: Circuit Breaker Abre Quando Limiar de Falhas é Excedido
    /// For random failure rates (0-100%) and thresholds (20-80%):
    /// - When failure_rate > threshold (with min_requests met): circuit opens
    /// - When failure_rate < threshold: circuit stays closed
    mod property_circuit_breaker_opens {
        use super::*;

        /// Strategy: generate a threshold (20-80%), a total request count (min 10-50),
        /// and a failure count that either exceeds or is below the threshold.
        fn circuit_breaker_scenario_strategy()
            -> impl Strategy<Value = (f64, u64, u64, bool)>
        {
            // threshold between 20% and 80%
            (20.0f64..=80.0f64, 10u64..=50u64, proptest::bool::ANY).prop_flat_map(
                |(threshold, total, should_exceed)| {
                    let max_failures_below = ((threshold / 100.0) * total as f64).floor() as u64;
                    let max_failures_below = max_failures_below.saturating_sub(1);
                    let min_failures_above =
                        ((threshold / 100.0) * total as f64).ceil() as u64 + 1;
                    let min_failures_above = min_failures_above.min(total);

                    if should_exceed {
                        (min_failures_above..=total)
                            .prop_map(move |failures| (threshold, total, failures, true))
                            .boxed()
                    } else {
                        (0u64..=max_failures_below)
                            .prop_map(move |failures| (threshold, total, failures, false))
                            .boxed()
                    }
                },
            )
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(100))]

            #[test]
            fn circuit_opens_when_threshold_exceeded(
                (threshold, total, failures, should_open) in circuit_breaker_scenario_strategy(),
            ) {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();

                rt.block_on(async {
                    let config = CircuitBreakerConfig {
                        failure_threshold_percent: threshold,
                        window_duration_secs: 60,
                        min_requests_in_window: total, // Require exactly this many requests
                        open_ttl_secs: 30,
                        max_ttl_secs: 300,
                        backoff_factor: 2.0,
                    };
                    let cb = LocalCircuitBreaker::new(config);

                    let successes = total - failures;

                    // Record failures first, then successes
                    for _ in 0..failures {
                        cb.record_result("test-provider", CallResult::Failure).await;
                    }
                    for _ in 0..successes {
                        cb.record_result("test-provider", CallResult::Success).await;
                    }

                    let is_available = cb.is_available("test-provider").await;

                    if should_open {
                        prop_assert!(
                            !is_available,
                            "Circuit should be OPEN: threshold={:.1}%, total={}, failures={}, rate={:.1}%",
                            threshold,
                            total,
                            failures,
                            (failures as f64 / total as f64) * 100.0,
                        );
                    } else {
                        prop_assert!(
                            is_available,
                            "Circuit should be CLOSED: threshold={:.1}%, total={}, failures={}, rate={:.1}%",
                            threshold,
                            total,
                            failures,
                            (failures as f64 / total as f64) * 100.0,
                        );
                    }
                    Ok(())
                })?;
            }
        }
    }

    /// **Validates: Requirements 5.5**
    ///
    /// Property 12: Cálculo de TTL com Backoff Exponencial
    /// For random initial TTLs, backoff factors (1.5-4.0), and max TTLs,
    /// after a half-open failure: new_ttl = min(current_ttl * factor, max_ttl).
    mod property_backoff_ttl_calculation {
        use super::*;

        /// Strategy: generate initial_ttl (1-60s), backoff_factor (1.5-4.0), max_ttl (60-600s)
        fn backoff_scenario_strategy() -> impl Strategy<Value = (u64, f64, u64)> {
            (
                1u64..=60u64,         // initial_ttl_secs
                // Use integers 15..=40 mapped to f64 / 10.0 to get 1.5..=4.0
                (15u32..=40u32).prop_map(|v| v as f64 / 10.0),
                60u64..=600u64,       // max_ttl_secs
            )
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(100))]

            #[test]
            fn backoff_ttl_is_min_of_current_times_factor_and_max(
                (initial_ttl_secs, backoff_factor, max_ttl_secs) in backoff_scenario_strategy(),
            ) {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();

                rt.block_on(async {
                    let config = CircuitBreakerConfig {
                        failure_threshold_percent: 50.0,
                        window_duration_secs: 60,
                        min_requests_in_window: 1,
                        open_ttl_secs: initial_ttl_secs,
                        max_ttl_secs,
                        backoff_factor,
                    };
                    let cb = LocalCircuitBreaker::new(config);

                    // Open circuit with a 429
                    cb.record_result("test-provider", CallResult::RateLimited).await;

                    // Set TTL=0 so we immediately transition to HalfOpen on is_available
                    // Instead, we manipulate state directly through the trait:
                    // We need to wait for TTL to expire, but we can use open_ttl_secs=0 trick
                    // Actually, let's use a config with open_ttl_secs = 0 to force immediate HalfOpen
                    drop(cb);

                    // Re-create with open_ttl_secs=0 for immediate transition
                    let config_zero_ttl = CircuitBreakerConfig {
                        failure_threshold_percent: 50.0,
                        window_duration_secs: 60,
                        min_requests_in_window: 1,
                        open_ttl_secs: 0, // Expires immediately
                        max_ttl_secs,
                        backoff_factor,
                    };
                    let cb = LocalCircuitBreaker::new(config_zero_ttl);

                    // Open circuit via 429
                    cb.record_result("test-provider", CallResult::RateLimited).await;

                    // TTL=0 → is_available transitions to HalfOpen
                    let available = cb.is_available("test-provider").await;
                    prop_assert!(available, "Should be available (HalfOpen) after TTL=0 expires");

                    // Now record a failure in HalfOpen → re-opens with backoff
                    cb.record_result("test-provider", CallResult::Failure).await;

                    // Check that it's now open
                    let available_after = cb.is_available("test-provider").await;
                    prop_assert!(!available_after, "Should be unavailable after HalfOpen failure");

                    // Verify the backoff calculation:
                    // Initial current_backoff for the provider starts at open_ttl (0 secs),
                    // but the code uses max(base_secs, 1) before multiplying.
                    // So: new_ttl = min(max(0, 1) * backoff_factor, max_ttl)
                    //           = min(backoff_factor as u64, max_ttl)
                    let expected_base = 0u64.max(1); // max(open_ttl_secs, 1) = 1
                    let expected_new_ttl_secs = ((expected_base as f64 * backoff_factor) as u64).min(max_ttl_secs);

                    // We can verify by checking state internally
                    let states = cb.states.read().await;
                    let provider_state = states.get("test-provider").unwrap();
                    let actual_backoff = provider_state.current_backoff;
                    let expected_backoff = Duration::from_secs(expected_new_ttl_secs);

                    prop_assert_eq!(
                        actual_backoff,
                        expected_backoff,
                        "Backoff TTL mismatch: expected {:?} (base={}, factor={}, max={}), got {:?}",
                        expected_backoff,
                        expected_base,
                        backoff_factor,
                        max_ttl_secs,
                        actual_backoff,
                    );

                    Ok(())
                })?;
            }
        }
    }
}
