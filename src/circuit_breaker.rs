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
    Open {
        until: Instant,
        backoff_ttl: Duration,
    },
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
                    let new_ttl_secs = (base_secs as f64 * self.config.backoff_factor) as u64;
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

// ─── Redis Implementation ─────────────────────────────────────────────────────

/// Lua: record a call result and update state atomically using the server clock.
///
/// KEYS[1]=state KEYS[2]=failures KEYS[3]=requests KEYS[4]=backoff KEYS[5]=probe
/// ARGV: 1=is_failure(0/1) 2=is_rate_limited(0/1) 3=is_success(0/1)
///       4=window_secs 5=min_requests 6=threshold_pct 7=open_ttl 8=max_ttl 9=backoff_factor
///       10=member(unique)
/// Returns resulting state code: 0=closed 1=open 2=half-open(not represented here; see is_available)
const CB_RECORD_LUA: &str = r#"
local t = redis.call('TIME')
local now = tonumber(t[1]) * 1000 + math.floor(tonumber(t[2]) / 1000)
local window_ms = tonumber(ARGV[4]) * 1000
local cutoff = now - window_ms

local is_failure = ARGV[1] == '1'
local is_rl = ARGV[2] == '1'
local is_success = ARGV[3] == '1'
local open_ttl = tonumber(ARGV[7])
local max_ttl = tonumber(ARGV[8])
local factor = tonumber(ARGV[9])
local member = ARGV[10]

-- Record request in window.
redis.call('ZADD', KEYS[3], now, member)
redis.call('ZREMRANGEBYSCORE', KEYS[3], 0, cutoff)
redis.call('EXPIRE', KEYS[3], tonumber(ARGV[4]))

if is_failure or is_rl then
    redis.call('ZADD', KEYS[2], now, member .. 'f')
    redis.call('ZREMRANGEBYSCORE', KEYS[2], 0, cutoff)
    redis.call('EXPIRE', KEYS[2], tonumber(ARGV[4]))
end

-- Half-open detection: a probe claim exists → this result is the probe outcome.
local probe = redis.call('GET', KEYS[5])
if probe then
    if is_success then
        redis.call('DEL', KEYS[1], KEYS[2], KEYS[3], KEYS[4], KEYS[5])
        return 0
    else
        local cur = tonumber(redis.call('GET', KEYS[4]) or tostring(open_ttl))
        local new_ttl = math.floor(cur * factor)
        if new_ttl > max_ttl then new_ttl = max_ttl end
        if new_ttl < 1 then new_ttl = 1 end
        redis.call('SET', KEYS[1], 'open', 'EX', new_ttl)
        redis.call('SET', KEYS[4], new_ttl)
        redis.call('DEL', KEYS[5])
        return 1
    end
end

-- If already open, nothing more to do.
if redis.call('EXISTS', KEYS[1]) == 1 then
    return 1
end

-- 429 opens immediately.
if is_rl then
    redis.call('SET', KEYS[1], 'open', 'EX', open_ttl)
    redis.call('SET', KEYS[4], open_ttl)
    return 1
end

-- Failure-rate threshold.
local total = redis.call('ZCARD', KEYS[3])
local fails = redis.call('ZCARD', KEYS[2])
if total >= tonumber(ARGV[5]) then
    local rate = (fails / total) * 100.0
    if rate > tonumber(ARGV[6]) then
        redis.call('SET', KEYS[1], 'open', 'EX', open_ttl)
        redis.call('SET', KEYS[4], open_ttl)
        return 1
    end
end
return 0
"#;

/// Lua: compute availability and claim the half-open probe atomically.
///
/// KEYS[1]=state KEYS[5]=probe
/// ARGV: 1=probe_ttl_secs
/// Returns: 0=closed(available) 1=open(unavailable) 2=half-open-probe-claimed(available)
///          3=half-open-probe-in-flight-by-other(unavailable)
const CB_AVAILABLE_LUA: &str = r#"
if redis.call('EXISTS', KEYS[1]) == 1 then
    return 1  -- open
end
-- state absent: closed, UNLESS a probe cycle is active.
if redis.call('EXISTS', KEYS[5]) == 1 then
    return 3  -- another replica owns the probe
end
-- Try to claim the probe. If state recently expired, the first caller probes.
-- We only create a probe claim if there is evidence a cycle was open: a backoff key.
if redis.call('EXISTS', KEYS[2]) == 1 then
    local ok = redis.call('SET', KEYS[5], '1', 'NX', 'EX', tonumber(ARGV[1]))
    if ok then
        return 2  -- we are the probe
    else
        return 3  -- lost the race
    end
end
return 0  -- fully closed
"#;

/// Redis-backed circuit breaker with shared state across replicas.
///
/// State is encoded in keys (see design): `:state` present = Open, `:probe` present
/// = half-open probe in flight, both absent = Closed. All transitions are atomic
/// via Lua using the Redis server clock (no replica clock skew).
///
/// `is_available` uses a short-TTL local read-through cache to avoid a Redis
/// round-trip per request; writes (`record_result`) always go through Redis and
/// invalidate the local copy immediately on a 429 (write-through).
pub struct RedisCircuitBreaker {
    client: fred::clients::RedisClient,
    config: CircuitBreakerConfig,
    metrics: Arc<crate::observability::Metrics>,
    /// Local read-through cache: provider -> (available, cached_at).
    local_cache: std::sync::Mutex<HashMap<String, (bool, Instant)>>,
}

impl RedisCircuitBreaker {
    pub fn new(
        client: fred::clients::RedisClient,
        config: CircuitBreakerConfig,
        metrics: Arc<crate::observability::Metrics>,
    ) -> Self {
        Self {
            client,
            config,
            metrics,
            local_cache: std::sync::Mutex::new(HashMap::new()),
        }
    }

    fn state_key(p: &str) -> String {
        format!("melis:cb:{}:state", p)
    }
    fn failures_key(p: &str) -> String {
        format!("melis:cb:{}:failures", p)
    }
    fn requests_key(p: &str) -> String {
        format!("melis:cb:{}:requests", p)
    }
    fn backoff_key(p: &str) -> String {
        format!("melis:cb:{}:backoff", p)
    }
    fn probe_key(p: &str) -> String {
        format!("melis:cb:{}:probe", p)
    }

    fn cache_ttl(&self) -> Duration {
        Duration::from_millis(self.config.redis_state_cache_ms)
    }

    fn cached(&self, provider: &str) -> Option<bool> {
        let cache = self.local_cache.lock().unwrap();
        cache.get(provider).and_then(|(avail, at)| {
            if at.elapsed() < self.cache_ttl() {
                Some(*avail)
            } else {
                None
            }
        })
    }

    fn put_cache(&self, provider: &str, available: bool) {
        let mut cache = self.local_cache.lock().unwrap();
        cache.insert(provider.to_string(), (available, Instant::now()));
    }

    fn invalidate_cache(&self, provider: &str) {
        let mut cache = self.local_cache.lock().unwrap();
        cache.remove(provider);
    }

    fn degrade(&self, err: &str) {
        self.metrics.circuit_breaker_degraded.inc();
        tracing::warn!(error = %err, "redis circuit breaker degraded; fail-open");
    }

    fn set_state_metric(&self, provider: &str, code: i64) {
        // 0=closed,1=open,2=half-open
        self.metrics
            .circuit_breaker_state
            .with_label_values(&[provider])
            .set(code);
    }
}

#[async_trait]
impl CircuitBreaker for RedisCircuitBreaker {
    async fn is_available(&self, provider_id: &str) -> bool {
        // 1. Local read-through cache (no Redis round-trip on hit).
        if let Some(avail) = self.cached(provider_id) {
            return avail;
        }

        use fred::interfaces::LuaInterface;
        let probe_ttl = self.config.open_ttl_secs.max(1);
        let result: Result<i64, _> = self
            .client
            .eval(
                CB_AVAILABLE_LUA,
                vec![
                    Self::state_key(provider_id),
                    Self::failures_key(provider_id),
                    Self::requests_key(provider_id),
                    Self::backoff_key(provider_id),
                    Self::probe_key(provider_id),
                ],
                vec![probe_ttl.to_string()],
            )
            .await;

        match result {
            Ok(code) => {
                let available = code == 0 || code == 2;
                let state_metric = match code {
                    0 => 0,     // closed
                    2 | 3 => 2, // half-open (probe cycle)
                    _ => 1,     // open
                };
                self.set_state_metric(provider_id, state_metric);
                self.put_cache(provider_id, available);
                available
            }
            Err(e) => {
                // Fail-open: Redis unavailable must never take all traffic down.
                self.degrade(&e.to_string());
                true
            }
        }
    }

    async fn record_result(&self, provider_id: &str, result: CallResult) {
        use fred::interfaces::LuaInterface;

        let is_failure = matches!(result, CallResult::Failure | CallResult::Timeout);
        let is_rate_limited = matches!(result, CallResult::RateLimited);
        let is_success = matches!(result, CallResult::Success);

        // Unique member for the sliding-window sorted sets.
        let member = format!(
            "{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );

        let eval: Result<i64, _> = self
            .client
            .eval(
                CB_RECORD_LUA,
                vec![
                    Self::state_key(provider_id),
                    Self::failures_key(provider_id),
                    Self::requests_key(provider_id),
                    Self::backoff_key(provider_id),
                    Self::probe_key(provider_id),
                ],
                vec![
                    (is_failure as u8).to_string(),
                    (is_rate_limited as u8).to_string(),
                    (is_success as u8).to_string(),
                    self.config.window_duration_secs.to_string(),
                    self.config.min_requests_in_window.to_string(),
                    self.config.failure_threshold_percent.to_string(),
                    self.config.open_ttl_secs.to_string(),
                    self.config.max_ttl_secs.to_string(),
                    self.config.backoff_factor.to_string(),
                    member,
                ],
            )
            .await;

        match eval {
            Ok(state_code) => {
                // Write-through: a result that (re)opens the circuit invalidates our
                // own local cache immediately so this replica converges without waiting
                // for the TTL; other replicas converge via their own short TTLs.
                if state_code == 1 {
                    self.invalidate_cache(provider_id);
                    self.set_state_metric(provider_id, 1);
                } else if state_code == 0 && is_success {
                    // Closed (possibly after a successful probe).
                    self.invalidate_cache(provider_id);
                    self.set_state_metric(provider_id, 0);
                }
            }
            Err(e) => self.degrade(&e.to_string()),
        }
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
            backend: crate::config::CircuitBreakerBackend::Local,
            redis_state_cache_ms: 150,
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
            backend: crate::config::CircuitBreakerBackend::Local,
            redis_state_cache_ms: 150,
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
        cb.record_result("provider-1", CallResult::RateLimited)
            .await;

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
            backend: crate::config::CircuitBreakerBackend::Local,
            redis_state_cache_ms: 150,
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
            backend: crate::config::CircuitBreakerBackend::Local,
            redis_state_cache_ms: 150,
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
    async fn half_open_transitions_to_closed_on_success() {
        let config = CircuitBreakerConfig {
            failure_threshold_percent: 50.0,
            window_duration_secs: 60,
            min_requests_in_window: 2,
            open_ttl_secs: 0, // Zero TTL so it transitions immediately to HalfOpen
            max_ttl_secs: 300,
            backoff_factor: 2.0,
            backend: crate::config::CircuitBreakerBackend::Local,
            redis_state_cache_ms: 150,
        };
        let cb = LocalCircuitBreaker::new(config);

        // Open the circuit
        cb.record_result("provider-1", CallResult::RateLimited)
            .await;
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
            backend: crate::config::CircuitBreakerBackend::Local,
            redis_state_cache_ms: 150,
        };
        let cb = LocalCircuitBreaker::new(config);

        // Open the circuit
        cb.record_result("provider-1", CallResult::RateLimited)
            .await;
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
            backend: crate::config::CircuitBreakerBackend::Local,
            redis_state_cache_ms: 150,
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
            backend: crate::config::CircuitBreakerBackend::Local,
            redis_state_cache_ms: 150,
        };
        let cb = LocalCircuitBreaker::new(config);

        // Open circuit for provider-1
        cb.record_result("provider-1", CallResult::RateLimited)
            .await;

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
        fn circuit_breaker_scenario_strategy() -> impl Strategy<Value = (f64, u64, u64, bool)> {
            // threshold between 20% and 80%
            (20.0f64..=80.0f64, 10u64..=50u64, proptest::bool::ANY).prop_flat_map(
                |(threshold, total, should_exceed)| {
                    let max_failures_below = ((threshold / 100.0) * total as f64).floor() as u64;
                    let max_failures_below = max_failures_below.saturating_sub(1);
                    let min_failures_above = ((threshold / 100.0) * total as f64).ceil() as u64 + 1;
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
                        backend: crate::config::CircuitBreakerBackend::Local,
                        redis_state_cache_ms: 150,
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
                1u64..=60u64, // initial_ttl_secs
                // Use integers 15..=40 mapped to f64 / 10.0 to get 1.5..=4.0
                (15u32..=40u32).prop_map(|v| v as f64 / 10.0),
                60u64..=600u64, // max_ttl_secs
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
                        backend: crate::config::CircuitBreakerBackend::Local,
                        redis_state_cache_ms: 150,
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
                        backend: crate::config::CircuitBreakerBackend::Local,
                        redis_state_cache_ms: 150,
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

// ─── Redis integration tests (require a live Redis at localhost:6379) ──────────

#[cfg(test)]
mod redis_it {
    use super::*;
    use std::sync::Arc;

    /// Connect a fred client to a local Redis. Returns `None` if unavailable so the
    /// (ignored) tests can be skipped gracefully when run without a server.
    async fn connect() -> Option<fred::clients::RedisClient> {
        use fred::prelude::*;
        let cfg = fred::types::RedisConfig::from_url("redis://localhost:6379").ok()?;
        let client = Builder::from_config(cfg).build().ok()?;
        if client.connect().await.is_err() || client.wait_for_connect().await.is_err() {
            return None;
        }
        Some(client)
    }

    fn cfg() -> CircuitBreakerConfig {
        CircuitBreakerConfig {
            failure_threshold_percent: 50.0,
            window_duration_secs: 60,
            min_requests_in_window: 2,
            open_ttl_secs: 2,
            max_ttl_secs: 30,
            backoff_factor: 2.0,
            backend: crate::config::CircuitBreakerBackend::Redis,
            redis_state_cache_ms: 0, // disable local cache so reads hit Redis directly
        }
    }

    /// Unique provider id per test run to avoid cross-test key collisions.
    fn provider_id(tag: &str) -> String {
        format!(
            "it-{}-{}",
            tag,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        )
    }

    async fn cleanup(client: &fred::clients::RedisClient, provider: &str) {
        use fred::interfaces::KeysInterface;
        for suffix in ["state", "failures", "requests", "backoff", "probe"] {
            let _: Result<(), _> = client
                .del::<i64, _>(format!("melis:cb:{}:{}", provider, suffix))
                .await
                .map(|_| ());
        }
    }

    #[tokio::test]
    #[ignore = "requires a live Redis at localhost:6379; run in pre-merge env (G1)"]
    async fn two_instances_share_state() {
        let client = match connect().await {
            Some(c) => c,
            None => return, // Redis not available; skip.
        };
        let provider = provider_id("share");
        let metrics = Arc::new(crate::observability::Metrics::new());
        let a = RedisCircuitBreaker::new(client.clone(), cfg(), metrics.clone());
        let b = RedisCircuitBreaker::new(client.clone(), cfg(), metrics.clone());

        // A observes a 429 → opens the circuit in shared Redis state.
        a.record_result(&provider, CallResult::RateLimited).await;

        // B (a different instance) must see the provider as unavailable.
        assert!(
            !b.is_available(&provider).await,
            "B should see the circuit opened by A via shared Redis state"
        );

        cleanup(&client, &provider).await;
    }

    #[tokio::test]
    #[ignore = "requires a live Redis at localhost:6379; run in pre-merge env (G1)"]
    async fn single_probe_under_concurrent_half_open() {
        let client = match connect().await {
            Some(c) => c,
            None => return,
        };
        let provider = provider_id("probe");
        let metrics = Arc::new(crate::observability::Metrics::new());

        // Open the circuit, then wait for the state TTL to expire (half-open).
        let opener = RedisCircuitBreaker::new(client.clone(), cfg(), metrics.clone());
        opener
            .record_result(&provider, CallResult::RateLimited)
            .await;
        tokio::time::sleep(std::time::Duration::from_millis(2100)).await;

        // Many instances race to probe; only one must win the claim.
        let mut instances = Vec::new();
        for _ in 0..8 {
            instances.push(RedisCircuitBreaker::new(
                client.clone(),
                cfg(),
                metrics.clone(),
            ));
        }
        let mut probes = 0;
        for inst in &instances {
            if inst.is_available(&provider).await {
                probes += 1;
            }
        }
        assert_eq!(probes, 1, "exactly one instance should win the probe claim");

        cleanup(&client, &provider).await;
    }

    #[tokio::test]
    #[ignore = "requires a live Redis at localhost:6379; run in pre-merge env (G1)"]
    async fn parity_open_probe_success_closes() {
        let client = match connect().await {
            Some(c) => c,
            None => return,
        };
        let provider = provider_id("probe-success");
        let metrics = Arc::new(crate::observability::Metrics::new());
        let cb = RedisCircuitBreaker::new(client.clone(), cfg(), metrics.clone());

        cb.record_result(&provider, CallResult::RateLimited).await; // open
        assert!(!cb.is_available(&provider).await, "should be open");

        tokio::time::sleep(std::time::Duration::from_millis(2100)).await; // TTL expires
        assert!(
            cb.is_available(&provider).await,
            "should be half-open probe"
        );

        // Probe succeeds → circuit closes.
        cb.record_result(&provider, CallResult::Success).await;
        assert!(
            cb.is_available(&provider).await,
            "should be closed after success"
        );

        cleanup(&client, &provider).await;
    }

    #[tokio::test]
    #[ignore = "requires a live Redis at localhost:6379; run in pre-merge env (G1)"]
    async fn parity_open_probe_failure_reopens_with_larger_ttl() {
        use fred::interfaces::KeysInterface;
        let client = match connect().await {
            Some(c) => c,
            None => return,
        };
        let provider = provider_id("probe-fail");
        let metrics = Arc::new(crate::observability::Metrics::new());
        let cb = RedisCircuitBreaker::new(client.clone(), cfg(), metrics.clone());

        cb.record_result(&provider, CallResult::RateLimited).await; // open, backoff=open_ttl(2)
        tokio::time::sleep(std::time::Duration::from_millis(2100)).await; // half-open
        assert!(cb.is_available(&provider).await, "half-open probe");

        // Probe fails → re-open with backoff * factor (2 * 2 = 4).
        cb.record_result(&provider, CallResult::Failure).await;
        let backoff: Option<String> = client
            .get(format!("melis:cb:{}:backoff", provider))
            .await
            .ok()
            .flatten();
        assert_eq!(
            backoff.as_deref(),
            Some("4"),
            "backoff should grow between probe rounds (2 → 4)"
        );
        assert!(!cb.is_available(&provider).await, "should be open again");

        cleanup(&client, &provider).await;
    }

    #[tokio::test]
    #[ignore = "requires a live Redis at localhost:6379; run in pre-merge env (G1)"]
    async fn parity_failure_rate_opens() {
        let client = match connect().await {
            Some(c) => c,
            None => return,
        };
        let provider = provider_id("rate");
        let metrics = Arc::new(crate::observability::Metrics::new());
        let cb = RedisCircuitBreaker::new(client.clone(), cfg(), metrics.clone());

        // 2 failures over 2 requests = 100% > 50% threshold, min_requests=2.
        cb.record_result(&provider, CallResult::Failure).await;
        cb.record_result(&provider, CallResult::Failure).await;
        assert!(
            !cb.is_available(&provider).await,
            "circuit should open on failure-rate threshold"
        );

        cleanup(&client, &provider).await;
    }

    #[tokio::test]
    #[ignore = "timing-sensitive on fred reconnection; run in pre-merge env (G1)"]
    async fn redis_down_fails_open() {
        // A client pointed at a dead port: record/is_available must fail open and
        // bump the degraded metric.
        use fred::prelude::*;
        let cfg_bad = fred::types::RedisConfig::from_url("redis://127.0.0.1:59998").unwrap();
        let client = Builder::from_config(cfg_bad).build().unwrap();
        // Do not wait_for_connect: commands will error → fail-open path.
        let _conn = client.connect();

        let metrics = Arc::new(crate::observability::Metrics::new());
        let cb = RedisCircuitBreaker::new(client, cfg(), metrics.clone());

        // is_available must fail open (true) and increment degraded.
        let available = cb.is_available("whatever").await;
        assert!(available, "must fail open when Redis is unreachable");
        assert!(
            metrics.circuit_breaker_degraded.get() >= 1,
            "degraded metric should increment on Redis failure"
        );
    }
}
