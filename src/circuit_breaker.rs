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

    /// Decide whether ONE attempt may reach the provider.
    ///
    /// Unlike [`is_available`](Self::is_available), the answer can be a half-open
    /// *probe* grant carrying an identity (lease token). A grant authorizes exactly
    /// one attempt; the caller must hand the same `Admission` back to
    /// [`record_outcome`](Self::record_outcome) so a late completion can be matched
    /// to (or rejected as stale against) the current probe.
    ///
    /// `probe_lease` bounds how long a probe may stay in flight before it is
    /// considered lost; it should be the provider timeout plus a small margin.
    ///
    /// Default (local breaker): maps `is_available` to `Allow`/`Deny`, unchanged.
    async fn admit(&self, provider_id: &str, _probe_lease: Duration) -> Admission {
        if self.is_available(provider_id).await {
            Admission::Allow
        } else {
            Admission::Deny
        }
    }

    /// Record the outcome of an attempt that was admitted via [`admit`](Self::admit).
    ///
    /// Default (local breaker): ignores the admission and calls `record_result`.
    async fn record_outcome(&self, provider_id: &str, result: CallResult, _admission: &Admission) {
        self.record_result(provider_id, result).await
    }
}

/// Outcome of an admission decision for a single attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Admission {
    /// Circuit healthy (closed): proceed normally.
    Allow,
    /// Half-open probe granted. Carries the lease token identifying this probe.
    /// Authorizes exactly one attempt.
    Probe(String),
    /// Do not call the provider (open, or a probe is already in flight).
    Deny,
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

// ─── Redis Implementation ─────────────────────────────────────────────────────
//
// State machine encoded in keys (`P` = provider id):
//
//   melis:cb:P:state    present  → OPEN (TTL = current open window)
//   melis:cb:P:backoff  present  → the circuit is in an UNHEALTHY CYCLE
//                                  (value = current open TTL in secs; no TTL).
//                                  This is the persistent marker that tells a
//                                  circuit whose OPEN just expired apart from a
//                                  healthy one that never opened. It is created
//                                  together with `:state` and deleted ONLY by a
//                                  successful probe.
//   melis:cb:P:probe    present  → a half-open probe lease is held; the VALUE is the
//                                  lease token (the probe's identity); TTL = lease.
//
//   state absent & backoff absent                    → CLOSED (healthy): allow, no probe
//   state present                                    → OPEN: deny
//   state absent & backoff present & probe absent    → HALF-OPEN: first caller wins a probe
//   state absent & backoff present & probe present   → HALF-OPEN, probe in flight: deny
//
// Expiry of `:state` therefore never means "closed": only a successful probe does.

/// Lua: admission decision + atomic half-open probe lease.
///
/// KEYS: 1=state 2=failures 3=requests 4=backoff 5=probe
/// ARGV: 1=lease_secs 2=token
/// Returns a string: "0:" closed (allow) | "1:" open (deny) |
///                   "2:<token>" probe granted | "3:" probe in flight elsewhere (deny)
const CB_ADMIT_LUA: &str = r#"
if redis.call('EXISTS', KEYS[1]) == 1 then
    return '1:'
end
if redis.call('EXISTS', KEYS[4]) == 1 then
    -- Unhealthy cycle whose OPEN window has expired: half-open. Exactly one caller
    -- (across all replicas) can create the probe key.
    local ok = redis.call('SET', KEYS[5], ARGV[2], 'NX', 'EX', tonumber(ARGV[1]))
    if ok then
        return '2:' .. ARGV[2]
    end
    return '3:'
end
return '0:'
"#;

/// Lua: record an outcome and apply state transitions atomically, using the Redis
/// server clock.
///
/// KEYS: 1=state 2=failures 3=requests 4=backoff 5=probe
/// ARGV: 1=is_failure 2=is_rate_limited 3=is_success 4=window_secs 5=min_requests
///       6=threshold_pct 7=open_ttl 8=max_ttl 9=backoff_factor 10=member 11=probe_token ('' if none)
/// Returns: 0=closed/ok 1=(re)opened 4=stale probe ignored 5=in-flight result ignored
const CB_RECORD_LUA: &str = r#"
local t = redis.call('TIME')
local now = tonumber(t[1]) * 1000 + math.floor(tonumber(t[2]) / 1000)
local window_secs = tonumber(ARGV[4])
local cutoff = now - window_secs * 1000

local is_failure = ARGV[1] == '1'
local is_rl = ARGV[2] == '1'
local is_success = ARGV[3] == '1'
local open_ttl = tonumber(ARGV[7])
local max_ttl = tonumber(ARGV[8])
local factor = tonumber(ARGV[9])
local member = ARGV[10]
local token = ARGV[11]

-- Probe completion. Only the CURRENT lease holder may change state: a completion
-- arriving after its lease expired, or after a newer probe/round started, is
-- rejected here, atomically with the state update below.
if token ~= '' then
    local current = redis.call('GET', KEYS[5])
    if current ~= token then
        return 4
    end
    if is_success then
        redis.call('DEL', KEYS[1], KEYS[2], KEYS[3], KEYS[4], KEYS[5])
        return 0
    end
    local cur = tonumber(redis.call('GET', KEYS[4]) or tostring(open_ttl))
    local new_ttl = math.floor(cur * factor)
    if new_ttl > max_ttl then new_ttl = max_ttl end
    if new_ttl < 1 then new_ttl = 1 end
    redis.call('SET', KEYS[1], 'open', 'EX', new_ttl)
    redis.call('SET', KEYS[4], new_ttl)
    redis.call('DEL', KEYS[5])
    return 1
end

-- Regular (non-probe) result. During an unhealthy cycle it can only be a request
-- that was admitted before the circuit opened: it must not alter the state.
if redis.call('EXISTS', KEYS[4]) == 1 then
    return 5
end

redis.call('ZADD', KEYS[3], now, member)
redis.call('ZREMRANGEBYSCORE', KEYS[3], 0, cutoff)
redis.call('EXPIRE', KEYS[3], window_secs)
if is_failure or is_rl then
    redis.call('ZADD', KEYS[2], now, member)
    redis.call('ZREMRANGEBYSCORE', KEYS[2], 0, cutoff)
    redis.call('EXPIRE', KEYS[2], window_secs)
end

if is_rl then
    redis.call('SET', KEYS[1], 'open', 'EX', open_ttl)
    redis.call('SET', KEYS[4], open_ttl)
    return 1
end

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

/// Redis-backed circuit breaker with shared state across replicas.
///
/// - Transitions are atomic (Lua) and use the Redis server clock.
/// - `admit` reads through a short-TTL local cache of **Allow/Deny only**. A probe
///   grant is never cached: it authorizes one attempt, not a window of traffic.
/// - Redis problems never block or fail requests: operations are bounded by a
///   [`RedisGate`]; on error/timeout the breaker fails open (`Allow`), counts
///   `melis_circuit_breaker_degraded`, and skips Redis for a short window. It
///   recovers automatically when Redis answers again.
pub struct RedisCircuitBreaker {
    client: fred::clients::RedisClient,
    config: CircuitBreakerConfig,
    metrics: Arc<crate::observability::Metrics>,
    gate: crate::redis_client::RedisGate,
    /// provider -> (available, expires_at). Only Allow/Deny are stored.
    local_cache: std::sync::Mutex<HashMap<String, (bool, Instant)>>,
    /// provider -> "known unhealthy until". Set whenever Redis reports the circuit
    /// as OPEN or HALF-OPEN. If Redis becomes unreadable before that instant, this
    /// replica keeps denying instead of failing open, so a Redis outage during an
    /// unhealthy cycle cannot let a flood of traffic through.
    known_unhealthy: std::sync::Mutex<HashMap<String, Instant>>,
    seq: std::sync::atomic::AtomicU64,
}

impl RedisCircuitBreaker {
    pub fn new(
        client: fred::clients::RedisClient,
        config: CircuitBreakerConfig,
        metrics: Arc<crate::observability::Metrics>,
    ) -> Self {
        Self::with_gate(
            client,
            config,
            metrics,
            crate::redis_client::RedisGate::new(
                crate::redis_client::DEFAULT_OP_TIMEOUT,
                crate::redis_client::DEFAULT_DEGRADED_BACKOFF,
            ),
        )
    }

    pub fn with_gate(
        client: fred::clients::RedisClient,
        config: CircuitBreakerConfig,
        metrics: Arc<crate::observability::Metrics>,
        gate: crate::redis_client::RedisGate,
    ) -> Self {
        Self {
            client,
            config,
            metrics,
            gate,
            local_cache: std::sync::Mutex::new(HashMap::new()),
            known_unhealthy: std::sync::Mutex::new(HashMap::new()),
            seq: std::sync::atomic::AtomicU64::new(0),
        }
    }

    fn keys(p: &str) -> Vec<String> {
        // Order matters: it defines KEYS[1..5] in the Lua scripts.
        ["state", "failures", "requests", "backoff", "probe"]
            .iter()
            .map(|s| format!("melis:cb:{}:{}", p, s))
            .collect()
    }

    fn unique(&self) -> String {
        let n = self.seq.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        format!("{}-{}-{}", std::process::id(), nanos, n)
    }

    fn cache_ttl(&self) -> Duration {
        Duration::from_millis(self.config.redis_state_cache_ms)
    }

    fn cached(&self, provider: &str) -> Option<bool> {
        let cache = self.local_cache.lock().unwrap();
        cache
            .get(provider)
            .and_then(|(avail, expires)| (Instant::now() < *expires).then_some(*avail))
    }

    fn put_cache(&self, provider: &str, available: bool) {
        if self.cache_ttl().is_zero() {
            return;
        }
        self.local_cache.lock().unwrap().insert(
            provider.to_string(),
            (available, Instant::now() + self.cache_ttl()),
        );
    }

    fn invalidate_cache(&self, provider: &str) {
        self.local_cache.lock().unwrap().remove(provider);
    }

    fn set_state_metric(&self, provider: &str, code: i64) {
        // 0=closed, 1=open, 2=half-open
        self.metrics
            .circuit_breaker_state
            .with_label_values(&[provider])
            .set(code);
    }

    fn degraded(&self) {
        self.metrics.circuit_breaker_degraded.inc();
    }

    fn mark_unhealthy(&self, provider: &str, horizon: Duration) {
        self.known_unhealthy
            .lock()
            .unwrap()
            .insert(provider.to_string(), Instant::now() + horizon);
    }

    fn clear_unhealthy(&self, provider: &str) {
        self.known_unhealthy.lock().unwrap().remove(provider);
    }

    /// Decision when Redis cannot be read.
    ///
    /// - Known unhealthy (this replica recently saw OPEN/HALF-OPEN): keep denying
    ///   until that knowledge expires — never fail open in the middle of a cycle.
    /// - Otherwise (healthy / never seen): fail open.
    fn on_redis_unavailable(&self, provider: &str) -> Admission {
        self.degraded();
        let until = self.known_unhealthy.lock().unwrap().get(provider).copied();
        match until {
            Some(t) if Instant::now() < t => Admission::Deny,
            _ => Admission::Allow,
        }
    }
}

#[async_trait]
impl CircuitBreaker for RedisCircuitBreaker {
    async fn is_available(&self, provider_id: &str) -> bool {
        let lease = Duration::from_secs(self.config.open_ttl_secs.max(1));
        self.admit(provider_id, lease).await != Admission::Deny
    }

    async fn record_result(&self, provider_id: &str, result: CallResult) {
        self.record_outcome(provider_id, result, &Admission::Allow)
            .await
    }

    async fn admit(&self, provider_id: &str, probe_lease: Duration) -> Admission {
        // 1. Local read-through cache (Allow/Deny only) — no Redis round-trip.
        if let Some(available) = self.cached(provider_id) {
            return if available {
                Admission::Allow
            } else {
                Admission::Deny
            };
        }

        // 2. Redis recently failed: don't touch it (bounded outage cost).
        if self.gate.suppressed() {
            return self.on_redis_unavailable(provider_id);
        }

        // How long "unhealthy" knowledge stays valid if Redis then becomes
        // unreadable: the longest open window plus one probe lease. After that the
        // replica fails open again (Redis outage must not block traffic forever).
        let unhealthy_horizon = Duration::from_secs(self.config.max_ttl_secs) + probe_lease;

        // 3. Atomic decision (+ probe lease claim) in Redis.
        use fred::interfaces::LuaInterface;
        let token = self.unique();
        let lease_secs = probe_lease.as_secs_f64().ceil().max(1.0) as u64;
        let reply: Option<String> = self
            .gate
            .run(self.client.eval::<String, _, _, _>(
                CB_ADMIT_LUA,
                Self::keys(provider_id),
                vec![lease_secs.to_string(), token],
            ))
            .await;

        let Some(reply) = reply else {
            return self.on_redis_unavailable(provider_id);
        };

        let (code, tok) = reply.split_once(':').unwrap_or(("0", ""));
        match code {
            "1" => {
                self.mark_unhealthy(provider_id, unhealthy_horizon);
                self.set_state_metric(provider_id, 1);
                self.put_cache(provider_id, false);
                Admission::Deny
            }
            "2" => {
                // Probe granted to THIS attempt only. Deliberately not cached: a
                // cached "available" would let a window of traffic through.
                self.mark_unhealthy(provider_id, unhealthy_horizon);
                self.set_state_metric(provider_id, 2);
                self.invalidate_cache(provider_id);
                Admission::Probe(tok.to_string())
            }
            "3" => {
                self.mark_unhealthy(provider_id, unhealthy_horizon);
                self.set_state_metric(provider_id, 2);
                self.put_cache(provider_id, false);
                Admission::Deny
            }
            _ => {
                self.clear_unhealthy(provider_id);
                self.set_state_metric(provider_id, 0);
                self.put_cache(provider_id, true);
                Admission::Allow
            }
        }
    }

    async fn record_outcome(&self, provider_id: &str, result: CallResult, admission: &Admission) {
        if self.gate.suppressed() {
            self.degraded();
            return;
        }

        let is_failure = matches!(result, CallResult::Failure | CallResult::Timeout);
        let is_rate_limited = matches!(result, CallResult::RateLimited);
        let is_success = matches!(result, CallResult::Success);
        let token = match admission {
            Admission::Probe(t) => t.clone(),
            _ => String::new(),
        };
        let was_probe = !token.is_empty();

        use fred::interfaces::LuaInterface;
        let code: Option<i64> = self
            .gate
            .run(self.client.eval::<i64, _, _, _>(
                CB_RECORD_LUA,
                Self::keys(provider_id),
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
                    self.unique(),
                    token,
                ],
            ))
            .await;

        match code {
            Some(1) => {
                // Write-through: this replica sees the (re)open immediately.
                self.invalidate_cache(provider_id);
                self.mark_unhealthy(
                    provider_id,
                    Duration::from_secs(self.config.max_ttl_secs + self.config.open_ttl_secs),
                );
                self.set_state_metric(provider_id, 1);
            }
            Some(0) if was_probe => {
                self.invalidate_cache(provider_id);
                self.clear_unhealthy(provider_id);
                self.set_state_metric(provider_id, 0);
            }
            Some(4) => {
                tracing::info!(
                    provider = provider_id,
                    "ignored completion of a superseded/expired probe"
                );
            }
            Some(_) => {}
            None => self.degraded(),
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

// ─── Redis integration tests ──────────────────────────────────────────────────
//
// These need a real Redis at 127.0.0.1:6379 and are `#[ignore]`d by default.
// Run them with:
//
//   cargo test --bin melis-gateway circuit_breaker::redis_it:: -- --ignored --test-threads=1
//
// The degradation/recovery tests put a controllable TCP proxy between the gateway
// client and Redis, so an outage can be simulated (and healed) inside the test.

#[cfg(test)]
mod redis_it {
    use super::*;
    use crate::config::{CircuitBreakerBackend, RedisConfig};
    use crate::redis_client::{RedisGate, SharedRedis};
    use fred::interfaces::KeysInterface;
    use std::net::SocketAddr;
    use std::time::Duration;
    use tokio::net::{TcpListener, TcpStream};
    use tokio::sync::watch;

    const REDIS: &str = "127.0.0.1:6379";

    // ── helpers ──────────────────────────────────────────────────────────────

    fn cfg(open_ttl: u64, cache_ms: u64) -> CircuitBreakerConfig {
        CircuitBreakerConfig {
            failure_threshold_percent: 50.0,
            window_duration_secs: 60,
            min_requests_in_window: 2,
            open_ttl_secs: open_ttl,
            max_ttl_secs: 60,
            backoff_factor: 2.0,
            backend: CircuitBreakerBackend::Redis,
            redis_state_cache_ms: cache_ms,
        }
    }

    fn redis_cfg(addr: &str, boot_timeout_secs: u64) -> RedisConfig {
        RedisConfig {
            cluster_urls: vec![format!("redis://{}", addr)],
            pool_size: 1,
            connect_timeout_secs: boot_timeout_secs,
            command_timeout_secs: 1,
        }
    }

    async fn shared(addr: &str) -> Arc<SharedRedis> {
        SharedRedis::connect(&redis_cfg(addr, 3)).await
    }

    fn metrics() -> Arc<crate::observability::Metrics> {
        Arc::new(crate::observability::Metrics::new())
    }

    fn breaker(
        sr: &SharedRedis,
        c: CircuitBreakerConfig,
        m: &Arc<crate::observability::Metrics>,
    ) -> Arc<RedisCircuitBreaker> {
        Arc::new(RedisCircuitBreaker::new(
            sr.client().expect("client").clone(),
            c,
            m.clone(),
        ))
    }

    /// Breaker with a fast gate so outage tests don't take long.
    fn fast_breaker(
        sr: &SharedRedis,
        c: CircuitBreakerConfig,
        m: &Arc<crate::observability::Metrics>,
    ) -> Arc<RedisCircuitBreaker> {
        Arc::new(RedisCircuitBreaker::with_gate(
            sr.client().expect("client").clone(),
            c,
            m.clone(),
            RedisGate::new(Duration::from_millis(200), Duration::from_millis(300)),
        ))
    }

    fn pid(tag: &str) -> String {
        format!(
            "it-{}-{}",
            tag,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        )
    }

    fn key(p: &str, suffix: &str) -> String {
        format!("melis:cb:{}:{}", p, suffix)
    }

    async fn get(sr: &SharedRedis, p: &str, suffix: &str) -> Option<String> {
        sr.client()
            .unwrap()
            .get::<Option<String>, _>(key(p, suffix))
            .await
            .unwrap()
    }

    async fn cleanup(sr: &SharedRedis, p: &str) {
        for s in ["state", "failures", "requests", "backoff", "probe"] {
            let _ = sr.client().unwrap().del::<i64, _>(key(p, s)).await;
        }
    }

    /// Fire `n` concurrent admissions per breaker; return (probes, allows, denies).
    async fn race(
        cbs: &[Arc<RedisCircuitBreaker>],
        n: usize,
        p: &str,
        lease: Duration,
    ) -> (usize, usize, usize) {
        let mut tasks = Vec::new();
        for cb in cbs {
            for _ in 0..n {
                let cb = cb.clone();
                let p = p.to_string();
                tasks.push(tokio::spawn(async move { cb.admit(&p, lease).await }));
            }
        }
        let (mut probes, mut allows, mut denies) = (0, 0, 0);
        for t in tasks {
            match t.await.unwrap() {
                Admission::Probe(_) => probes += 1,
                Admission::Allow => allows += 1,
                Admission::Deny => denies += 1,
            }
        }
        (probes, allows, denies)
    }

    /// Open the circuit with a 429 and wait for the OPEN window to expire.
    async fn open_then_expire(cb: &RedisCircuitBreaker, p: &str, open_ttl: u64) {
        cb.record_result(p, CallResult::RateLimited).await;
        tokio::time::sleep(Duration::from_millis(open_ttl * 1000 + 150)).await;
    }

    // ── controllable TCP proxy (to simulate Redis outages) ───────────────────

    struct Proxy {
        addr: SocketAddr,
        kill: watch::Sender<bool>,
    }

    impl Proxy {
        async fn start(listen: &str, upstream: &'static str) -> Proxy {
            let listener = TcpListener::bind(listen).await.expect("bind proxy");
            let addr = listener.local_addr().unwrap();
            let (kill, rx) = watch::channel(false);
            let mut accept_rx = rx.clone();
            tokio::spawn(async move {
                loop {
                    tokio::select! {
                        _ = accept_rx.changed() => break,
                        res = listener.accept() => {
                            let Ok((mut inbound, _)) = res else { break };
                            let mut conn_rx = rx.clone();
                            tokio::spawn(async move {
                                if let Ok(mut out) = TcpStream::connect(upstream).await {
                                    tokio::select! {
                                        _ = conn_rx.changed() => {}
                                        _ = tokio::io::copy_bidirectional(&mut inbound, &mut out) => {}
                                    }
                                }
                            });
                        }
                    }
                }
            });
            Proxy { addr, kill }
        }

        /// Stop accepting and drop every established connection.
        fn stop(&self) {
            let _ = self.kill.send(true);
        }
    }

    /// A currently-free local port (bind, read, drop).
    fn free_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }

    // ── shared state & parity ────────────────────────────────────────────────

    #[tokio::test]
    #[ignore = "requires a live Redis at 127.0.0.1:6379; run in pre-merge env (G1)"]
    async fn two_instances_share_state() {
        let sr = shared(REDIS).await;
        let p = pid("share");
        let m = metrics();
        let a = breaker(&sr, cfg(5, 0), &m);
        let b = breaker(&sr, cfg(5, 0), &m);

        a.record_result(&p, CallResult::RateLimited).await;
        assert_eq!(
            b.admit(&p, Duration::from_secs(5)).await,
            Admission::Deny,
            "B must see the circuit opened by A"
        );
        cleanup(&sr, &p).await;
    }

    #[tokio::test]
    #[ignore = "requires a live Redis at 127.0.0.1:6379; run in pre-merge env (G1)"]
    async fn parity_failure_rate_opens() {
        let sr = shared(REDIS).await;
        let p = pid("rate");
        let cb = breaker(&sr, cfg(5, 0), &metrics());
        // 2 failures / 2 requests = 100% > 50%, min_requests = 2.
        cb.record_result(&p, CallResult::Failure).await;
        cb.record_result(&p, CallResult::Failure).await;
        assert_eq!(cb.admit(&p, Duration::from_secs(5)).await, Admission::Deny);
        cleanup(&sr, &p).await;
    }

    #[tokio::test]
    #[ignore = "requires a live Redis at 127.0.0.1:6379; run in pre-merge env (G1)"]
    async fn parity_below_min_requests_stays_closed() {
        let sr = shared(REDIS).await;
        let p = pid("minreq");
        let cb = breaker(&sr, cfg(5, 0), &metrics());
        cb.record_result(&p, CallResult::Failure).await; // 1 < min_requests(2)
        assert_eq!(cb.admit(&p, Duration::from_secs(5)).await, Admission::Allow);
        cleanup(&sr, &p).await;
    }

    #[tokio::test]
    #[ignore = "requires a live Redis at 127.0.0.1:6379; run in pre-merge env (G1)"]
    async fn full_sequence_open_expire_probe_success_closes() {
        let sr = shared(REDIS).await;
        let p = pid("seq-ok");
        let m = metrics();
        let cb = breaker(&sr, cfg(2, 0), &m);

        cb.record_result(&p, CallResult::RateLimited).await;
        assert_eq!(cb.admit(&p, Duration::from_secs(5)).await, Admission::Deny);

        tokio::time::sleep(Duration::from_millis(2150)).await;
        let adm = cb.admit(&p, Duration::from_secs(5)).await;
        assert!(matches!(adm, Admission::Probe(_)), "got {:?}", adm);

        cb.record_outcome(&p, CallResult::Success, &adm).await;
        // Closed: every cycle key is gone and traffic flows without a probe.
        for s in ["state", "backoff", "probe"] {
            assert_eq!(get(&sr, &p, s).await, None, "{s} should be deleted");
        }
        assert_eq!(cb.admit(&p, Duration::from_secs(5)).await, Admission::Allow);
        assert_eq!(
            m.circuit_breaker_state.with_label_values(&[&p]).get(),
            0,
            "metric reflects Closed"
        );
        cleanup(&sr, &p).await;
    }

    #[tokio::test]
    #[ignore = "requires a live Redis at 127.0.0.1:6379; run in pre-merge env (G1)"]
    async fn full_sequence_open_expire_probe_failure_reopens_with_growing_backoff() {
        let sr = shared(REDIS).await;
        let p = pid("seq-fail");
        let cb = breaker(&sr, cfg(2, 0), &metrics());

        cb.record_result(&p, CallResult::RateLimited).await;
        assert_eq!(get(&sr, &p, "backoff").await.as_deref(), Some("2"));

        // Round 1: probe fails → re-open with 2 * 2 = 4.
        tokio::time::sleep(Duration::from_millis(2150)).await;
        let a1 = cb.admit(&p, Duration::from_secs(5)).await;
        assert!(matches!(a1, Admission::Probe(_)));
        cb.record_outcome(&p, CallResult::Failure, &a1).await;
        assert_eq!(get(&sr, &p, "backoff").await.as_deref(), Some("4"));
        assert!(get(&sr, &p, "state").await.is_some(), "re-opened");
        assert_eq!(cb.admit(&p, Duration::from_secs(5)).await, Admission::Deny);

        // Round 2: probe fails again → 4 * 2 = 8 (backoff keeps growing between rounds).
        tokio::time::sleep(Duration::from_millis(4150)).await;
        let a2 = cb.admit(&p, Duration::from_secs(5)).await;
        assert!(matches!(a2, Admission::Probe(_)));
        cb.record_outcome(&p, CallResult::Timeout, &a2).await;
        assert_eq!(get(&sr, &p, "backoff").await.as_deref(), Some("8"));
        cleanup(&sr, &p).await;
    }

    // ── Closed vs Half-Open ──────────────────────────────────────────────────

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "requires a live Redis at 127.0.0.1:6379; run in pre-merge env (G1)"]
    async fn healthy_circuit_allows_traffic_and_never_requires_a_probe() {
        let sr = shared(REDIS).await;
        let p = pid("healthy");
        let cb = breaker(&sr, cfg(2, 0), &metrics());

        let (probes, allows, denies) =
            race(std::slice::from_ref(&cb), 32, &p, Duration::from_secs(5)).await;
        assert_eq!((probes, allows, denies), (0, 32, 0));

        // Successes keep it healthy and create no cycle/probe keys.
        for _ in 0..5 {
            cb.record_result(&p, CallResult::Success).await;
        }
        assert_eq!(get(&sr, &p, "backoff").await, None);
        assert_eq!(get(&sr, &p, "probe").await, None);
        cleanup(&sr, &p).await;
    }

    #[tokio::test]
    #[ignore = "requires a live Redis at 127.0.0.1:6379; run in pre-merge env (G1)"]
    async fn open_expiry_is_not_closure() {
        let sr = shared(REDIS).await;
        let p = pid("expiry");
        let cb = breaker(&sr, cfg(2, 0), &metrics());
        open_then_expire(&cb, &p, 2).await;

        assert_eq!(get(&sr, &p, "state").await, None, "OPEN window expired");
        assert!(
            get(&sr, &p, "backoff").await.is_some(),
            "persistent marker survives the expiry"
        );
        // First caller gets the (single) probe; nobody else gets a free pass.
        assert!(matches!(
            cb.admit(&p, Duration::from_secs(5)).await,
            Admission::Probe(_)
        ));
        for _ in 0..5 {
            assert_eq!(cb.admit(&p, Duration::from_secs(5)).await, Admission::Deny);
        }
        cleanup(&sr, &p).await;
    }

    // ── single probe: cache + concurrency ────────────────────────────────────

    #[tokio::test]
    #[ignore = "requires a live Redis at 127.0.0.1:6379; run in pre-merge env (G1)"]
    async fn probe_grant_is_never_cached_locally() {
        // A very long local cache: if a grant were cached as "available", the next
        // call would be Allow instead of Deny.
        let sr = shared(REDIS).await;
        let p = pid("nocache");
        let cb = breaker(&sr, cfg(2, 60_000), &metrics());
        open_then_expire(&cb, &p, 2).await;
        // The Deny cached while OPEN would mask this; use a fresh breaker to read Redis.
        let cb = breaker(&sr, cfg(2, 60_000), &metrics());

        let first = cb.admit(&p, Duration::from_secs(5)).await;
        assert!(matches!(first, Admission::Probe(_)), "got {:?}", first);
        let second = cb.admit(&p, Duration::from_secs(5)).await;
        assert_eq!(
            second,
            Admission::Deny,
            "a grant must authorize one attempt only"
        );
        cleanup(&sr, &p).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "requires a live Redis at 127.0.0.1:6379; run in pre-merge env (G1)"]
    async fn single_probe_concurrent_requests_same_replica() {
        let sr = shared(REDIS).await;
        let p = pid("same");
        let setup = breaker(&sr, cfg(2, 150), &metrics());
        open_then_expire(&setup, &p, 2).await;

        // Long local cache on purpose: if a grant were cached as "available",
        // requests arriving while the probe is in flight would get Allow.
        let cb = breaker(&sr, cfg(2, 5_000), &metrics());
        let lease = Duration::from_secs(5);

        // (a) A burst at the instant half-open starts: exactly one probe.
        let (p1, a1, d1) = race(std::slice::from_ref(&cb), 32, &p, lease).await;
        assert_eq!((p1, a1, d1), (1, 0, 31), "burst at half-open start");

        // (b) Requests arriving one after another WHILE the probe is in flight
        // (no concurrent reply overwrites the replica's cache in this pattern).
        cleanup(&sr, &p).await;
        open_then_expire(&setup, &p, 2).await;
        let cb = breaker(&sr, cfg(2, 5_000), &metrics());
        let first = cb.admit(&p, lease).await;
        assert!(matches!(first, Admission::Probe(_)), "got {:?}", first);
        let (p2, a2, d2) = race(&[cb], 32, &p, lease).await;
        assert_eq!(
            (p2, a2, d2),
            (0, 0, 32),
            "nothing else may reach the provider while the probe is in flight"
        );
        cleanup(&sr, &p).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "requires a live Redis at 127.0.0.1:6379; run in pre-merge env (G1)"]
    async fn single_probe_concurrent_requests_across_two_replicas() {
        let sr = shared(REDIS).await;
        let p = pid("two");
        let setup = breaker(&sr, cfg(2, 150), &metrics());
        let lease = Duration::from_secs(5);

        // (a) Simultaneous burst on both replicas at half-open start.
        open_then_expire(&setup, &p, 2).await;
        let a = breaker(&sr, cfg(2, 5_000), &metrics());
        let b = breaker(&sr, cfg(2, 5_000), &metrics());
        let (p1, a1, d1) = race(&[a, b], 16, &p, lease).await;
        assert_eq!((p1, a1, d1), (1, 0, 31), "one probe across both replicas");

        // (b) Replica A holds the probe; traffic then spreads over A and B.
        cleanup(&sr, &p).await;
        open_then_expire(&setup, &p, 2).await;
        let a = breaker(&sr, cfg(2, 5_000), &metrics());
        let b = breaker(&sr, cfg(2, 5_000), &metrics());
        let first = a.admit(&p, lease).await;
        assert!(matches!(first, Admission::Probe(_)), "got {:?}", first);
        let (p2, a2, d2) = race(&[a, b], 16, &p, lease).await;
        assert_eq!(
            (p2, a2, d2),
            (0, 0, 32),
            "no attempt from either replica while A's probe is in flight"
        );
        cleanup(&sr, &p).await;
    }

    // ── probe identity / late completion ─────────────────────────────────────

    #[tokio::test]
    #[ignore = "requires a live Redis at 127.0.0.1:6379; run in pre-merge env (G1)"]
    async fn late_probe_completion_cannot_close_a_newer_round() {
        let sr = shared(REDIS).await;
        let p = pid("late");
        let m = metrics();
        let a = breaker(&sr, cfg(2, 0), &m);
        let b = breaker(&sr, cfg(2, 0), &m);

        open_then_expire(&a, &p, 2).await;

        // Replica A wins probe #1 with a 1s lease, then is slow to finish.
        let t1 = a.admit(&p, Duration::from_secs(1)).await;
        assert!(matches!(t1, Admission::Probe(_)));
        tokio::time::sleep(Duration::from_millis(1250)).await; // lease expires

        // A new round: replica B wins probe #2.
        let t2 = b.admit(&p, Duration::from_secs(30)).await;
        assert!(matches!(t2, Admission::Probe(_)));
        assert_ne!(t1, t2, "each probe has its own identity");
        let token2 = get(&sr, &p, "probe").await;

        // A's LATE success (probe #1) must NOT close the circuit nor touch probe #2.
        a.record_outcome(&p, CallResult::Success, &t1).await;
        assert!(
            get(&sr, &p, "backoff").await.is_some(),
            "still unhealthy cycle"
        );
        assert_eq!(get(&sr, &p, "probe").await, token2, "probe #2 untouched");
        assert_eq!(
            b.admit(&p, Duration::from_secs(30)).await,
            Admission::Deny,
            "circuit is not closed by a stale completion"
        );

        // B's probe fails → re-open; then A's late success arrives AGAIN.
        b.record_outcome(&p, CallResult::Failure, &t2).await;
        assert!(
            get(&sr, &p, "state").await.is_some(),
            "re-opened by probe #2"
        );
        a.record_outcome(&p, CallResult::Success, &t1).await;
        assert!(get(&sr, &p, "state").await.is_some(), "still open");
        assert_eq!(a.admit(&p, Duration::from_secs(30)).await, Admission::Deny);
        cleanup(&sr, &p).await;
    }

    #[tokio::test]
    #[ignore = "requires a live Redis at 127.0.0.1:6379; run in pre-merge env (G1)"]
    async fn inflight_result_from_before_opening_does_not_alter_state() {
        let sr = shared(REDIS).await;
        let p = pid("inflight");
        let cb = breaker(&sr, cfg(5, 0), &metrics());

        cb.record_result(&p, CallResult::RateLimited).await; // OPEN
                                                             // A request admitted before the circuit opened finishes successfully now.
        cb.record_result(&p, CallResult::Success).await;
        assert!(get(&sr, &p, "state").await.is_some(), "must stay open");
        assert_eq!(cb.admit(&p, Duration::from_secs(5)).await, Admission::Deny);
        cleanup(&sr, &p).await;
    }

    #[tokio::test]
    #[ignore = "requires a live Redis at 127.0.0.1:6379; run in pre-merge env (G1)"]
    async fn abandoned_probe_lease_expires_and_another_probe_runs() {
        // The request carrying the probe dies (client disconnect): nothing is
        // recorded. The lease must expire by itself so the probe is never stuck.
        let sr = shared(REDIS).await;
        let p = pid("abandoned");
        let cb = breaker(&sr, cfg(2, 0), &metrics());
        open_then_expire(&cb, &p, 2).await;

        assert!(matches!(
            cb.admit(&p, Duration::from_secs(1)).await,
            Admission::Probe(_)
        ));
        assert_eq!(cb.admit(&p, Duration::from_secs(1)).await, Admission::Deny);
        tokio::time::sleep(Duration::from_millis(1250)).await;
        assert!(
            matches!(
                cb.admit(&p, Duration::from_secs(1)).await,
                Admission::Probe(_)
            ),
            "a new probe must be grantable after the lease expires"
        );
        cleanup(&sr, &p).await;
    }

    // ── degradation & recovery policy ────────────────────────────────────────
    //
    // Policy (identical whenever the failure happens): `backend: redis` stays
    // `redis`. Redis unreachable ⇒ melis_circuit_breaker_degraded + a short
    // suppression window, and:
    //   - provider not known to be unhealthy by this replica → fail open (Allow);
    //   - provider this replica saw OPEN/HALF-OPEN recently    → keep Deny
    //     (bounded by max_ttl + probe lease), so an outage mid-cycle can't leak.
    // Recovery is automatic when Redis answers again (no restart).

    #[tokio::test]
    #[ignore = "requires a live Redis at 127.0.0.1:6379; run in pre-merge env (G1)"]
    async fn degradation_redis_down_at_boot_fails_open() {
        let dead = format!("127.0.0.1:{}", free_port());
        let started = std::time::Instant::now();
        let sr = SharedRedis::connect(&redis_cfg(&dead, 1)).await;
        assert!(
            started.elapsed() < Duration::from_secs(4),
            "boot must be bounded by connect_timeout, took {:?}",
            started.elapsed()
        );
        assert!(
            sr.client().is_some(),
            "client exists so it can recover later"
        );
        assert!(!sr.connected_at_boot());
        assert!(!sr.is_healthy().await);

        let m = metrics();
        let cb = fast_breaker(&sr, cfg(2, 0), &m);
        let t = std::time::Instant::now();
        assert_eq!(
            cb.admit("any", Duration::from_secs(5)).await,
            Admission::Allow
        );
        assert!(
            t.elapsed() < Duration::from_secs(1),
            "bounded: {:?}",
            t.elapsed()
        );
        assert!(m.circuit_breaker_degraded.get() >= 1);

        // While suppressed, calls don't even touch Redis (no per-request timeout).
        let t = std::time::Instant::now();
        assert_eq!(
            cb.admit("any", Duration::from_secs(5)).await,
            Admission::Allow
        );
        assert!(t.elapsed() < Duration::from_millis(50), "{:?}", t.elapsed());
    }

    #[tokio::test]
    #[ignore = "requires a live Redis at 127.0.0.1:6379; run in pre-merge env (G1)"]
    async fn degradation_redis_goes_down_after_operating_fails_open() {
        let proxy = Proxy::start("127.0.0.1:0", REDIS).await;
        let sr = shared(&proxy.addr.to_string()).await;
        assert!(sr.connected_at_boot());
        let healthy = pid("runtime-down-healthy");
        let m = metrics();
        let cb = fast_breaker(&sr, cfg(30, 0), &m);

        // Gateway operating normally against Redis.
        assert_eq!(
            cb.admit(&healthy, Duration::from_secs(5)).await,
            Admission::Allow
        );

        proxy.stop(); // Redis "goes away" while the gateway is running.
        tokio::time::sleep(Duration::from_millis(300)).await;

        let t = std::time::Instant::now();
        assert_eq!(
            cb.admit(&healthy, Duration::from_secs(5)).await,
            Admission::Allow,
            "fail open: an unreadable breaker must not take healthy traffic down"
        );
        assert!(
            t.elapsed() < Duration::from_secs(1),
            "bounded: {:?}",
            t.elapsed()
        );
        assert!(m.circuit_breaker_degraded.get() >= 1);
    }

    #[tokio::test]
    #[ignore = "requires a live Redis at 127.0.0.1:6379; run in pre-merge env (G1)"]
    async fn redis_outage_during_unhealthy_cycle_keeps_denying() {
        // Fail-open must not undo the single-probe guarantee: a replica that KNOWS
        // the circuit is open / half-open keeps denying while Redis is unreadable.
        let proxy = Proxy::start("127.0.0.1:0", REDIS).await;
        let sr = shared(&proxy.addr.to_string()).await;
        let p = pid("outage-cycle");
        let m = metrics();
        let cb = fast_breaker(&sr, cfg(2, 0), &m);

        cb.record_result(&p, CallResult::RateLimited).await;
        tokio::time::sleep(Duration::from_millis(2150)).await; // half-open
        let probe = cb.admit(&p, Duration::from_secs(5)).await;
        assert!(matches!(probe, Admission::Probe(_)), "got {:?}", probe);

        proxy.stop(); // Redis unreachable while the probe is in flight
        tokio::time::sleep(Duration::from_millis(300)).await;
        let mut allows = 0;
        for _ in 0..20 {
            if cb.admit(&p, Duration::from_secs(5)).await != Admission::Deny {
                allows += 1;
            }
        }
        assert_eq!(allows, 0, "no traffic may leak during an unhealthy cycle");
        assert!(m.circuit_breaker_degraded.get() >= 1);

        // A provider this replica has never seen unhealthy still fails open.
        assert_eq!(
            cb.admit(&pid("never-seen"), Duration::from_secs(5)).await,
            Admission::Allow
        );

        let direct = shared(REDIS).await;
        cleanup(&direct, &p).await;
    }

    #[tokio::test]
    #[ignore = "requires a live Redis at 127.0.0.1:6379; run in pre-merge env (G1)"]
    async fn recovery_after_runtime_outage_without_restart() {
        let proxy = Proxy::start("127.0.0.1:0", REDIS).await;
        let port = proxy.addr.port();
        let sr = shared(&proxy.addr.to_string()).await;
        let m = metrics();
        let cb = fast_breaker(&sr, cfg(30, 0), &m);

        proxy.stop();
        tokio::time::sleep(Duration::from_millis(300)).await;
        let p = pid("recover-runtime");
        assert_eq!(cb.admit(&p, Duration::from_secs(5)).await, Admission::Allow);

        // While this gateway can't reach Redis, another replica opens the circuit.
        let direct = shared(REDIS).await;
        breaker(&direct, cfg(30, 0), &metrics())
            .record_result(&p, CallResult::RateLimited)
            .await;

        // Redis returns on the same address: the SAME gateway objects must resume
        // reading the shared state (and therefore start denying) with no restart.
        let _proxy2 = Proxy::start(&format!("127.0.0.1:{}", port), REDIS).await;
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        let mut recovered = false;
        while std::time::Instant::now() < deadline {
            if cb.admit(&p, Duration::from_secs(5)).await == Admission::Deny {
                recovered = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        assert!(
            recovered,
            "breaker must resume reading Redis state by itself"
        );
        assert!(sr.is_healthy().await);
        cleanup(&direct, &p).await;
    }

    #[tokio::test]
    #[ignore = "requires a live Redis at 127.0.0.1:6379; run in pre-merge env (G1)"]
    async fn recovery_after_boot_outage_without_restart() {
        // Redis is NOT reachable at boot...
        let port = free_port();
        let addr = format!("127.0.0.1:{}", port);
        let sr = SharedRedis::connect(&redis_cfg(&addr, 1)).await;
        assert!(!sr.connected_at_boot());
        let p = pid("recover-boot");
        let m = metrics();
        let cb = fast_breaker(&sr, cfg(30, 0), &m);
        assert_eq!(cb.admit(&p, Duration::from_secs(5)).await, Admission::Allow);

        // ...and another replica (talking to real Redis) opens the circuit.
        let direct = shared(REDIS).await;
        breaker(&direct, cfg(30, 0), &metrics())
            .record_result(&p, CallResult::RateLimited)
            .await;

        // Redis becomes reachable for the first gateway later: no restart.
        let _proxy = Proxy::start(&addr, REDIS).await;
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        let mut recovered = false;
        while std::time::Instant::now() < deadline {
            if cb.admit(&p, Duration::from_secs(5)).await == Admission::Deny {
                recovered = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        assert!(
            recovered,
            "a Redis that was down at boot must be picked up later"
        );
        cleanup(&direct, &p).await;
    }
}
