//! Shared Redis client (Phase 4).
//!
//! A single fred client built once at boot from `config.redis`, reused by the
//! semantic cache (Phase 2) and the Redis circuit breaker (Phase 4).
//!
//! Fail-open: if the connection cannot be established, `client()` returns `None`
//! and every Redis-backed component degrades gracefully. The gateway never fails
//! to boot because Redis is unavailable.

use std::sync::Arc;

use fred::prelude::*;

use crate::config::RedisConfig;

/// A process-wide shared Redis handle.
pub struct SharedRedis {
    client: Option<fred::clients::RedisClient>,
}

impl SharedRedis {
    /// Connect to Redis using the first configured cluster URL.
    ///
    /// Returns a handle whose `client()` is `Some` only if the connection was
    /// established. Any failure (no URL, invalid URL, connect/handshake error)
    /// yields a `None` client (fail-open) with a warning.
    pub async fn connect(config: &RedisConfig) -> Arc<SharedRedis> {
        let url = match config.cluster_urls.first() {
            Some(u) if !u.trim().is_empty() => u.clone(),
            _ => {
                tracing::warn!("shared redis: no Redis URL configured; Redis features disabled");
                return Arc::new(SharedRedis { client: None });
            }
        };

        let redis_config = match fred::types::RedisConfig::from_url(&url) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("shared redis: invalid Redis URL '{}': {}; disabled", url, e);
                return Arc::new(SharedRedis { client: None });
            }
        };

        let client = match Builder::from_config(redis_config).build() {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("shared redis: failed to build client: {}; disabled", e);
                return Arc::new(SharedRedis { client: None });
            }
        };

        if client.connect().await.is_err() || client.wait_for_connect().await.is_err() {
            tracing::warn!(
                "shared redis: could not connect to Redis at {}; disabled (fail-open)",
                url
            );
            return Arc::new(SharedRedis { client: None });
        }

        tracing::info!("shared redis: connected to Redis at {}", url);
        Arc::new(SharedRedis {
            client: Some(client),
        })
    }

    /// Returns the underlying client if connected.
    pub fn client(&self) -> Option<&fred::clients::RedisClient> {
        self.client.as_ref()
    }

    /// Lightweight health check (PING) used by the readiness probe.
    pub async fn is_healthy(&self) -> bool {
        match &self.client {
            Some(c) => {
                use fred::interfaces::ClientLike;
                c.ping::<()>().await.is_ok()
            }
            None => false,
        }
    }
}
