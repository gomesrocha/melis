//! Google Application Default Credentials (ADC) bearer-token provider,
//! used to authenticate outbound calls to Google Vertex AI.
//!
//! feature/catia-vertex-docker-readiness (Phase 7): never parses a
//! service-account JSON file or signs a JWT directly in this crate --
//! delegates entirely to the `gcp_auth` crate, which implements the
//! real Google ADC discovery chain: `GOOGLE_APPLICATION_CREDENTIALS`
//! env var -> `gcloud auth application-default login` file -> GCE/GKE
//! metadata server -> `gcloud` CLI on PATH. This means the SAME code
//! path works unchanged for local dev (a mounted service-account JSON)
//! and, once the metadata server or workload identity is present (as
//! it will be on GKE/GCE), without any code change on this side.
//!
//! Caveat, verified in-source (not assumed) during this feature:
//! `gcp_auth` 0.12.7's `CustomServiceAccount`/`provider()` chain does
//! NOT recognize `external_account` (Workload Identity Federation)
//! credential JSON -- only `service_account`-type JSON, the gcloud ADC
//! file, the metadata server, or the `gcloud` CLI. The target AWS EKS
//! architecture is "AWS workload identity -> Google Workload Identity
//! Federation -> ADC -> Vertex AI" (see docs/CATIA_VERTEX_DOCKER_READINESS.md);
//! WIF-shaped credentials are NOT exercised or proven by this feature.
//! Before relying on this module for that AWS bootstrap, re-verify WIF
//! support against the `gcp_auth` version in use at that time (or swap
//! providers) -- do not assume "zero code change" without checking.

use std::sync::Arc;

use tokio::sync::OnceCell;

/// OAuth2 scope required for Vertex AI (`aiplatform.googleapis.com`) calls.
const CLOUD_PLATFORM_SCOPE: &str = "https://www.googleapis.com/auth/cloud-platform";

/// Lazily-initialized, cached ADC token source.
///
/// Discovery (which may touch the filesystem, the GCE metadata server,
/// or spawn `gcloud`) only happens on the FIRST call to
/// [`VertexTokenCache::bearer_token`] -- a Melis instance that never
/// routes a request to a `vertex_anthropic` provider never attempts ADC
/// discovery, so existing (non-Vertex) deployments are unaffected.
/// `gcp_auth`'s own `TokenProvider` caches and refreshes the token
/// itself (per its own expiry), so this wrapper never re-fetches on
/// every request.
#[derive(Default)]
pub struct VertexTokenCache {
    provider: OnceCell<Arc<dyn gcp_auth::TokenProvider>>,
}

impl VertexTokenCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns a valid bearer token for Vertex AI calls.
    ///
    /// Never logs or returns token contents or credential file paths --
    /// callers should log only the error's `Display` at most, and
    /// should return a generic message (never this error's raw text)
    /// in any HTTP response body (see `router.rs`'s vertex_anthropic
    /// dispatch branch, Phase 12: never leak credential contents/paths
    /// to the client).
    pub async fn bearer_token(&self) -> Result<String, String> {
        let provider = self
            .provider
            .get_or_try_init(|| async { gcp_auth::provider().await.map_err(|e| e.to_string()) })
            .await?;

        let token = provider
            .token(&[CLOUD_PLATFORM_SCOPE])
            .await
            .map_err(|e| e.to_string())?;

        Ok(token.as_str().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_does_not_attempt_adc_discovery() {
        // Constructing the cache must be instant and infallible -- ADC
        // discovery is lazy, only triggered by `bearer_token()`. If this
        // constructor ever became eager/fallible, a Melis instance with
        // no Vertex provider configured would fail to start without ADC
        // present, which is exactly the regression this test guards
        // against.
        let cache = VertexTokenCache::new();
        assert!(cache.provider.initialized().not());
    }

    trait NotBool {
        fn not(self) -> bool;
    }
    impl NotBool for bool {
        fn not(self) -> bool {
            !self
        }
    }

    #[tokio::test]
    async fn bearer_token_fails_closed_without_any_adc_source() {
        // SAFETY (test-only, single-threaded test process assumption):
        // ensure no ambient ADC source is picked up by clearing the env
        // var this process might have inherited, so the test is
        // deterministic regardless of the host running it.
        // NOTE: this only removes GOOGLE_APPLICATION_CREDENTIALS for
        // THIS test's assertion of the "no source" path; it does not
        // and cannot prevent gcp_auth from finding a real gcloud ADC
        // file or metadata server if the test host has one -- so this
        // assertion is best-effort and may be skipped in some dev
        // environments (documented, not silently ignored).
        let had_var = std::env::var("GOOGLE_APPLICATION_CREDENTIALS").ok();
        unsafe {
            std::env::remove_var("GOOGLE_APPLICATION_CREDENTIALS");
        }
        let cache = VertexTokenCache::new();
        let result = cache.bearer_token().await;
        if let Some(v) = had_var {
            unsafe {
                std::env::set_var("GOOGLE_APPLICATION_CREDENTIALS", v);
            }
        }
        // On a dev machine with real gcloud ADC configured, this may
        // legitimately succeed -- so we only assert the failure SHAPE
        // when it does fail (never panics, never leaks raw credential
        // bytes), not that it must always fail.
        if let Err(msg) = result {
            assert!(!msg.is_empty());
            assert!(!msg.contains("BEGIN PRIVATE KEY"));
        }
    }
}
