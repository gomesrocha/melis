//! Shared Redis client (Phase 4).
//!
//! A single fred client built once at boot from `config.redis`, reused by the
//! semantic cache (Phase 2) and the Redis circuit breaker (Phase 4).
//!
//! # Availability policy (identical regardless of *when* Redis fails)
//!
//! - The client is created whenever a valid Redis URL is configured, **even if
//!   Redis is unreachable at boot**. It reconnects in the background with an
//!   unbounded exponential-backoff policy (`fail_fast = false`), so a Redis that
//!   comes up later (or returns after an outage) is picked up with no restart.
//! - Boot waits at most `redis.connect_timeout_secs` for the first connection and
//!   then continues; the gateway never fails to boot because of Redis.
//! - Components never block the request path on Redis: every operation goes
//!   through a [`RedisGate`], which bounds it with a short timeout and, after a
//!   failure, suppresses Redis calls for a short window (so an outage costs at
//!   most one timeout per window, not one per request).
//!
//! `client()` is `None` only when no usable Redis URL is configured at all.

use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use fred::prelude::*;

use crate::config::RedisConfig;

/// Per-operation timeout on the request path. Redis is expected to answer in
/// well under a millisecond; this only bounds the outage case.
pub const DEFAULT_OP_TIMEOUT: Duration = Duration::from_millis(100);

/// How long Redis calls are suppressed after a failure.
pub const DEFAULT_DEGRADED_BACKOFF: Duration = Duration::from_secs(1);

/// Bounds Redis operations on the request path and suppresses calls briefly
/// after a failure.
pub struct RedisGate {
    op_timeout: Duration,
    backoff: Duration,
    suppressed_until: Mutex<Option<Instant>>,
}

impl RedisGate {
    pub fn new(op_timeout: Duration, backoff: Duration) -> Self {
        Self {
            op_timeout,
            backoff,
            suppressed_until: Mutex::new(None),
        }
    }

    /// True while Redis calls are being suppressed after a recent failure.
    pub fn suppressed(&self) -> bool {
        match *self.suppressed_until.lock().unwrap() {
            Some(until) => Instant::now() < until,
            None => false,
        }
    }

    fn trip(&self) {
        *self.suppressed_until.lock().unwrap() = Some(Instant::now() + self.backoff);
    }

    fn clear(&self) {
        *self.suppressed_until.lock().unwrap() = None;
    }

    /// Run a Redis operation with a timeout. Returns `Some` on success (clearing
    /// any suppression) and `None` on error/timeout (tripping suppression).
    /// Callers translate `None` into their own fail-open behavior and metric.
    pub async fn run<T, F>(&self, fut: F) -> Option<T>
    where
        F: Future<Output = Result<T, RedisError>>,
    {
        match tokio::time::timeout(self.op_timeout, fut).await {
            Ok(Ok(v)) => {
                self.clear();
                Some(v)
            }
            Ok(Err(e)) => {
                tracing::warn!(error = %e, "redis operation failed; suppressing calls briefly");
                self.trip();
                None
            }
            Err(_) => {
                tracing::warn!(
                    timeout_ms = self.op_timeout.as_millis() as u64,
                    "redis operation timed out; suppressing calls briefly"
                );
                self.trip();
                None
            }
        }
    }
}

/// A process-wide shared Redis handle.
pub struct SharedRedis {
    client: Option<fred::clients::RedisClient>,
    /// Keeps the background connection task handle alive for the process lifetime.
    _conn: Option<fred::types::ConnectHandle>,
    #[cfg(test)]
    connected_at_boot: bool,
}

impl SharedRedis {
    /// Build the shared client from the first configured cluster URL.
    pub async fn connect(config: &RedisConfig) -> Arc<SharedRedis> {
        let url = match config.cluster_urls.first() {
            Some(u) if !u.trim().is_empty() => u.clone(),
            _ => {
                tracing::warn!("shared redis: no Redis URL configured; Redis features disabled");
                return Arc::new(SharedRedis::none());
            }
        };

        let mut redis_config = match fred::types::RedisConfig::from_url(&url) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("shared redis: invalid Redis URL '{}': {}; disabled", url, e);
                return Arc::new(SharedRedis::none());
            }
        };
        // Retry the *initial* connection in the background instead of failing.
        redis_config.fail_fast = false;

        let mut builder = Builder::from_config(redis_config);
        // max_attempts = 0 → retry forever: Redis returning later needs no restart.
        builder.set_policy(ReconnectPolicy::new_exponential(0, 100, 5_000, 2));
        let client = match builder.build() {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("shared redis: failed to build client: {}; disabled", e);
                return Arc::new(SharedRedis::none());
            }
        };

        // NOTE: `connect()` returns the handle of the task that *drives* the
        // connection and only completes when it closes. It must never be awaited
        // here; we only wait for the first connection to be established.
        let handle = client.connect();
        let connected = matches!(
            tokio::time::timeout(config.connect_timeout(), client.wait_for_connect()).await,
            Ok(Ok(()))
        );

        if connected {
            tracing::info!("shared redis: connected to Redis at {}", url);
        } else {
            tracing::warn!(
                "shared redis: Redis at {} not reachable at boot; continuing and retrying in \
                 the background (Redis-backed components fail open until it is reachable)",
                url
            );
        }

        Arc::new(SharedRedis {
            client: Some(client),
            _conn: Some(handle),
            #[cfg(test)]
            connected_at_boot: connected,
        })
    }

    fn none() -> SharedRedis {
        SharedRedis {
            client: None,
            _conn: None,
            #[cfg(test)]
            connected_at_boot: false,
        }
    }

    /// The underlying client; `None` only if no usable Redis URL is configured.
    pub fn client(&self) -> Option<&fred::clients::RedisClient> {
        self.client.as_ref()
    }

    /// Whether the first connection succeeded within the boot timeout.
    #[cfg(test)]
    pub fn connected_at_boot(&self) -> bool {
        self.connected_at_boot
    }

    /// Lightweight health check (PING), bounded by a short timeout.
    pub async fn is_healthy(&self) -> bool {
        match &self.client {
            Some(c) => matches!(
                tokio::time::timeout(Duration::from_millis(500), c.ping::<()>()).await,
                Ok(Ok(()))
            ),
            None => false,
        }
    }
}
