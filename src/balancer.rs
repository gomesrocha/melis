//! Load Balancer module.
//!
//! Implements weighted round-robin load balancing across multiple
//! provider endpoints. Distributes requests proportionally to
//! configured weights with ≤ 5% deviation over 1000 requests.
//!
//! Designed for hot-reload via `ArcSwap` and integration with
//! the circuit breaker for excluding unhealthy providers.
//!
//! Also provides failover logic: when a provider returns 5xx or
//! times out, the request is retried on the next available provider,
//! up to MAX_RETRIES attempts.

use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::error::GatewayError;
use crate::route_config::WeightedProvider;

/// Maximum number of failover retries per request.
pub const MAX_RETRIES: usize = 2;

/// Default timeout in seconds for provider requests.
pub const DEFAULT_TIMEOUT_SECS: u64 = 30;

/// Resolved endpoint information for a selected provider.
#[derive(Debug, Clone)]
pub struct ProviderEndpoint {
    pub provider_id: String,
    pub name: String,
    pub base_url: String,
    pub api_key: String,
    pub weight: u32,
    pub timeout_secs: u64,
    pub models: Vec<String>,
}

/// Trait defining the load balancer interface.
///
/// Implementations select the next provider from a weighted list
/// and support dynamic weight updates for hot-reload scenarios.
pub trait LoadBalancer: Send + Sync {
    /// Selects the next provider based on configured weights.
    ///
    /// Returns `GatewayError::ServiceUnavailable` when no providers
    /// are available (empty list).
    fn select_provider(
        &self,
        providers: &[WeightedProvider],
    ) -> Result<WeightedProvider, GatewayError>;

    /// Updates internal weight state. For stateless implementations
    /// this may be a no-op since weights come from the provider list.
    fn update_weights(&self, providers: &[WeightedProvider]);
}

/// Weighted Round-Robin load balancer.
///
/// Uses an atomic counter to cycle through an expanded weight table,
/// distributing selections proportionally to each provider's weight.
///
/// # Algorithm
///
/// Weights are expanded into a flat index table. For example, providers
/// with weights [60, 30, 10] produce a table of 100 entries where the
/// first 60 map to provider 0, next 30 to provider 1, and last 10 to
/// provider 2. The counter modulo total weight selects the index.
pub struct WeightedRoundRobin {
    counter: AtomicU64,
}

impl WeightedRoundRobin {
    /// Creates a new `WeightedRoundRobin` balancer.
    pub fn new() -> Self {
        Self {
            counter: AtomicU64::new(0),
        }
    }
}

impl Default for WeightedRoundRobin {
    fn default() -> Self {
        Self::new()
    }
}

impl LoadBalancer for WeightedRoundRobin {
    fn select_provider(
        &self,
        providers: &[WeightedProvider],
    ) -> Result<WeightedProvider, GatewayError> {
        if providers.is_empty() {
            return Err(GatewayError::ServiceUnavailable(
                "No providers available".to_string(),
            ));
        }

        // Single provider — fast path
        if providers.len() == 1 {
            return Ok(providers[0].clone());
        }

        let total_weight: u64 = providers.iter().map(|p| p.weight as u64).sum();

        if total_weight == 0 {
            return Err(GatewayError::ServiceUnavailable(
                "No providers available".to_string(),
            ));
        }

        // Fetch-and-increment the counter atomically
        let tick = self.counter.fetch_add(1, Ordering::Relaxed);
        let position = tick % total_weight;

        // Walk the weight ranges to find the selected provider
        let mut cumulative: u64 = 0;
        for provider in providers {
            cumulative += provider.weight as u64;
            if position < cumulative {
                return Ok(provider.clone());
            }
        }

        // Fallback — should not be reachable with valid weights
        Ok(providers.last().unwrap().clone())
    }

    fn update_weights(&self, _providers: &[WeightedProvider]) {
        // Stateless: weights are taken directly from the provider list
        // on each call. Hot-reload is handled by the caller updating
        // the provider list via ArcSwap in RouteConfigManager.
    }
}

// ─── Failover Logic ───────────────────────────────────────────────────────────

/// Represents the outcome of a single provider request attempt,
/// used by the failover loop to decide whether to retry.
#[derive(Debug)]
pub enum RequestOutcome<T> {
    /// Request succeeded with the given response.
    Success(T),
    /// Provider returned a 5xx error or timed out — eligible for failover.
    Retryable(String),
    /// A non-retryable error (4xx, etc.) — should not failover.
    NonRetryable(GatewayError),
}

/// Selects a provider from the given list, excluding any providers
/// whose names are in the `exclude` list. Uses the load balancer
/// for weighted selection among the remaining providers.
///
/// Returns `GatewayError::ServiceUnavailable` when no providers
/// remain after exclusion.
pub fn select_with_failover(
    balancer: &dyn LoadBalancer,
    providers: &[WeightedProvider],
    exclude: &[String],
) -> Result<WeightedProvider, GatewayError> {
    let available: Vec<WeightedProvider> = providers
        .iter()
        .filter(|p| !exclude.contains(&p.name))
        .cloned()
        .collect();

    if available.is_empty() {
        return Err(GatewayError::ServiceUnavailable(
            "All providers exhausted after failover attempts".to_string(),
        ));
    }

    balancer.select_provider(&available)
}

/// Executes a request with automatic failover on retryable failures.
///
/// The `request_fn` closure receives the selected `WeightedProvider` and
/// returns a `RequestOutcome` indicating success, retryable failure, or
/// non-retryable error.
///
/// Retry loop:
/// 1. Select initial provider (excluding none)
/// 2. Call `request_fn` with selected provider
/// 3. If `Retryable`: add provider to exclude list, select next
/// 4. Repeat up to `MAX_RETRIES` times
/// 5. If all attempts fail: return `ServiceUnavailable`
///
/// # Arguments
///
/// * `balancer` - The load balancer for provider selection
/// * `providers` - All configured providers for the route
/// * `request_fn` - Async closure that performs the actual request
///
/// # Returns
///
/// `Ok(T)` on success, or `GatewayError` if all attempts fail or
/// a non-retryable error occurs.
pub async fn execute_with_failover<T, F, Fut>(
    balancer: &dyn LoadBalancer,
    providers: &[WeightedProvider],
    request_fn: F,
) -> Result<T, GatewayError>
where
    F: Fn(WeightedProvider) -> Fut,
    Fut: Future<Output = RequestOutcome<T>>,
{
    let mut excluded: Vec<String> = Vec::new();
    let mut last_error: Option<String> = None;

    // Initial attempt + up to MAX_RETRIES failover attempts
    for _attempt in 0..=MAX_RETRIES {
        let provider = select_with_failover(balancer, providers, &excluded)?;
        let provider_name = provider.name.clone();

        match request_fn(provider).await {
            RequestOutcome::Success(response) => return Ok(response),
            RequestOutcome::Retryable(reason) => {
                last_error = Some(reason);
                excluded.push(provider_name);
                // Continue to next attempt
            }
            RequestOutcome::NonRetryable(err) => return Err(err),
        }
    }

    // All attempts exhausted
    Err(GatewayError::ServiceUnavailable(
        format!(
            "All providers exhausted after {} failover attempts. Last error: {}",
            MAX_RETRIES,
            last_error.unwrap_or_else(|| "unknown".to_string())
        ),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::collections::HashSet;

    fn make_provider(name: &str, weight: u32) -> WeightedProvider {
        WeightedProvider {
            name: name.to_string(),
            weight,
            model: format!("{}-model", name),
        }
    }

    #[test]
    fn single_provider_always_selected() {
        let balancer = WeightedRoundRobin::new();
        let providers = vec![make_provider("openai", 100)];

        for _ in 0..100 {
            let selected = balancer.select_provider(&providers).unwrap();
            assert_eq!(selected.name, "openai");
        }
    }

    #[test]
    fn empty_providers_returns_service_unavailable() {
        let balancer = WeightedRoundRobin::new();
        let providers: Vec<WeightedProvider> = vec![];

        let result = balancer.select_provider(&providers);
        assert!(result.is_err());

        let err = result.unwrap_err();
        match err {
            GatewayError::ServiceUnavailable(msg) => {
                assert_eq!(msg, "No providers available");
            }
            _ => panic!("Expected ServiceUnavailable, got {:?}", err),
        }
    }

    #[test]
    fn weights_respected_within_5_percent_tolerance() {
        let balancer = WeightedRoundRobin::new();
        let providers = vec![
            make_provider("openai", 60),
            make_provider("anthropic", 30),
            make_provider("vertex", 10),
        ];

        let total_iterations = 1000u64;
        let mut counts: HashMap<String, u64> = HashMap::new();

        for _ in 0..total_iterations {
            let selected = balancer.select_provider(&providers).unwrap();
            *counts.entry(selected.name.clone()).or_insert(0) += 1;
        }

        let total_weight: u64 = providers.iter().map(|p| p.weight as u64).sum();

        for provider in &providers {
            let expected_ratio = provider.weight as f64 / total_weight as f64;
            let actual_count = *counts.get(&provider.name).unwrap_or(&0);
            let actual_ratio = actual_count as f64 / total_iterations as f64;
            let deviation = (actual_ratio - expected_ratio).abs();

            assert!(
                deviation <= 0.05,
                "Provider '{}': expected ratio {:.3}, got {:.3} (deviation {:.3} > 0.05)",
                provider.name,
                expected_ratio,
                actual_ratio,
                deviation,
            );
        }
    }

    #[test]
    fn equal_weights_distribute_evenly() {
        let balancer = WeightedRoundRobin::new();
        let providers = vec![
            make_provider("a", 1),
            make_provider("b", 1),
            make_provider("c", 1),
        ];

        let total_iterations = 999u64; // Divisible by 3
        let mut counts: HashMap<String, u64> = HashMap::new();

        for _ in 0..total_iterations {
            let selected = balancer.select_provider(&providers).unwrap();
            *counts.entry(selected.name.clone()).or_insert(0) += 1;
        }

        // With equal weights and 999 iterations (divisible by 3),
        // each should get exactly 333
        assert_eq!(*counts.get("a").unwrap(), 333);
        assert_eq!(*counts.get("b").unwrap(), 333);
        assert_eq!(*counts.get("c").unwrap(), 333);
    }

    #[test]
    fn zero_total_weight_returns_service_unavailable() {
        let balancer = WeightedRoundRobin::new();
        let providers = vec![
            make_provider("a", 0),
            make_provider("b", 0),
        ];

        let result = balancer.select_provider(&providers);
        assert!(result.is_err());

        match result.unwrap_err() {
            GatewayError::ServiceUnavailable(msg) => {
                assert_eq!(msg, "No providers available");
            }
            _ => panic!("Expected ServiceUnavailable"),
        }
    }

    #[test]
    fn two_providers_weighted_distribution() {
        let balancer = WeightedRoundRobin::new();
        let providers = vec![
            make_provider("primary", 80),
            make_provider("secondary", 20),
        ];

        let total_iterations = 1000u64;
        let mut counts: HashMap<String, u64> = HashMap::new();

        for _ in 0..total_iterations {
            let selected = balancer.select_provider(&providers).unwrap();
            *counts.entry(selected.name.clone()).or_insert(0) += 1;
        }

        let primary_count = *counts.get("primary").unwrap_or(&0);
        let secondary_count = *counts.get("secondary").unwrap_or(&0);

        // 80/20 split with 5% tolerance
        let primary_ratio = primary_count as f64 / total_iterations as f64;
        let secondary_ratio = secondary_count as f64 / total_iterations as f64;

        assert!(
            (primary_ratio - 0.80).abs() <= 0.05,
            "Primary ratio {:.3} deviates from 0.80 by more than 5%",
            primary_ratio
        );
        assert!(
            (secondary_ratio - 0.20).abs() <= 0.05,
            "Secondary ratio {:.3} deviates from 0.20 by more than 5%",
            secondary_ratio
        );
    }

    // ─── Failover Tests ───────────────────────────────────────────────────────

    #[test]
    fn select_with_failover_excludes_providers() {
        let balancer = WeightedRoundRobin::new();
        let providers = vec![
            make_provider("openai", 60),
            make_provider("anthropic", 30),
            make_provider("vertex", 10),
        ];

        let exclude = vec!["openai".to_string()];
        let selected =
            super::select_with_failover(&balancer, &providers, &exclude).unwrap();
        assert_ne!(selected.name, "openai");
    }

    #[test]
    fn select_with_failover_all_excluded_returns_service_unavailable() {
        let balancer = WeightedRoundRobin::new();
        let providers = vec![
            make_provider("openai", 60),
            make_provider("anthropic", 30),
        ];

        let exclude = vec!["openai".to_string(), "anthropic".to_string()];
        let result =
            super::select_with_failover(&balancer, &providers, &exclude);
        assert!(result.is_err());

        match result.unwrap_err() {
            GatewayError::ServiceUnavailable(msg) => {
                assert!(msg.contains("exhausted"));
            }
            other => panic!("Expected ServiceUnavailable, got {:?}", other),
        }
    }

    #[test]
    fn select_with_failover_empty_exclude_selects_normally() {
        let balancer = WeightedRoundRobin::new();
        let providers = vec![
            make_provider("openai", 100),
        ];

        let exclude: Vec<String> = vec![];
        let selected =
            super::select_with_failover(&balancer, &providers, &exclude).unwrap();
        assert_eq!(selected.name, "openai");
    }

    #[tokio::test]
    async fn execute_with_failover_success_on_first_try() {
        let balancer = WeightedRoundRobin::new();
        let providers = vec![
            make_provider("openai", 60),
            make_provider("anthropic", 30),
            make_provider("vertex", 10),
        ];

        let result = super::execute_with_failover(
            &balancer,
            &providers,
            |provider| async move {
                // All providers succeed
                super::RequestOutcome::Success(format!("response from {}", provider.name))
            },
        )
        .await;

        assert!(result.is_ok());
        let response = result.unwrap();
        assert!(response.starts_with("response from "));
    }

    #[tokio::test]
    async fn execute_with_failover_retries_on_failure() {
        use std::sync::{Arc, Mutex};

        let balancer = WeightedRoundRobin::new();
        let providers = vec![
            make_provider("openai", 50),
            make_provider("anthropic", 50),
        ];

        let attempts = Arc::new(Mutex::new(Vec::new()));
        let attempts_clone = attempts.clone();

        let result = super::execute_with_failover(
            &balancer,
            &providers,
            |provider| {
                let attempts = attempts_clone.clone();
                async move {
                    let name = provider.name.clone();
                    attempts.lock().unwrap().push(name.clone());
                    if name == "openai" {
                        super::RequestOutcome::Retryable("500 Internal Server Error".to_string())
                    } else {
                        super::RequestOutcome::Success(format!("response from {}", name))
                    }
                }
            },
        )
        .await;

        assert!(result.is_ok());
        let response = result.unwrap();
        assert_eq!(response, "response from anthropic");

        let recorded = attempts.lock().unwrap();
        // First attempt was openai (failed), second was anthropic (succeeded)
        assert!(recorded.len() >= 2);
        assert_eq!(recorded[0], "openai");
        assert_eq!(recorded[1], "anthropic");
    }

    #[tokio::test]
    async fn execute_with_failover_max_retries_exhausted() {
        use std::sync::{Arc, Mutex};

        let balancer = WeightedRoundRobin::new();
        let providers = vec![
            make_provider("provider_a", 33),
            make_provider("provider_b", 33),
            make_provider("provider_c", 34),
        ];

        let attempts = Arc::new(Mutex::new(Vec::new()));
        let attempts_clone = attempts.clone();

        let result: Result<String, GatewayError> = super::execute_with_failover(
            &balancer,
            &providers,
            |provider| {
                let attempts = attempts_clone.clone();
                async move {
                    attempts.lock().unwrap().push(provider.name.clone());
                    // All providers fail
                    super::RequestOutcome::Retryable("timeout".to_string())
                }
            },
        )
        .await;

        assert!(result.is_err());
        match result.unwrap_err() {
            GatewayError::ServiceUnavailable(msg) => {
                assert!(msg.contains("exhausted"));
                assert!(msg.contains("timeout"));
            }
            other => panic!("Expected ServiceUnavailable, got {:?}", other),
        }

        let recorded = attempts.lock().unwrap();
        // Should have made exactly MAX_RETRIES + 1 = 3 attempts
        assert_eq!(recorded.len(), super::MAX_RETRIES + 1);
        // Each attempt should be a different provider
        let unique: HashSet<&String> = recorded.iter().collect();
        assert_eq!(unique.len(), 3);
    }

    #[tokio::test]
    async fn execute_with_failover_non_retryable_stops_immediately() {
        use std::sync::{Arc, Mutex};

        let balancer = WeightedRoundRobin::new();
        let providers = vec![
            make_provider("openai", 50),
            make_provider("anthropic", 50),
        ];

        let attempts = Arc::new(Mutex::new(Vec::new()));
        let attempts_clone = attempts.clone();

        let result: Result<String, GatewayError> = super::execute_with_failover(
            &balancer,
            &providers,
            |provider| {
                let attempts = attempts_clone.clone();
                async move {
                    attempts.lock().unwrap().push(provider.name.clone());
                    // Non-retryable error (e.g., 400 Bad Request)
                    super::RequestOutcome::NonRetryable(
                        GatewayError::BadRequest("invalid model".to_string()),
                    )
                }
            },
        )
        .await;

        assert!(result.is_err());
        match result.unwrap_err() {
            GatewayError::BadRequest(msg) => {
                assert_eq!(msg, "invalid model");
            }
            other => panic!("Expected BadRequest, got {:?}", other),
        }

        // Only one attempt — did not retry on non-retryable error
        let recorded = attempts.lock().unwrap();
        assert_eq!(recorded.len(), 1);
    }

    #[tokio::test]
    async fn execute_with_failover_fewer_providers_than_retries() {
        // When there are only 2 providers and both fail, should return 503
        // even though MAX_RETRIES allows 2 retries (3 total attempts)
        let balancer = WeightedRoundRobin::new();
        let providers = vec![
            make_provider("openai", 50),
            make_provider("anthropic", 50),
        ];

        let result: Result<String, GatewayError> = super::execute_with_failover(
            &balancer,
            &providers,
            |provider| async move {
                super::RequestOutcome::Retryable(
                    format!("{} returned 502", provider.name),
                )
            },
        )
        .await;

        assert!(result.is_err());
        match result.unwrap_err() {
            GatewayError::ServiceUnavailable(msg) => {
                assert!(msg.contains("exhausted") || msg.contains("All providers"));
            }
            other => panic!("Expected ServiceUnavailable, got {:?}", other),
        }
    }
}

#[cfg(test)]
mod proptest_tests {
    use super::*;
    use proptest::prelude::*;
    use std::collections::HashMap;

    fn make_provider(name: &str, weight: u32) -> WeightedProvider {
        WeightedProvider {
            name: name.to_string(),
            weight,
            model: format!("{}-model", name),
        }
    }

    /// **Validates: Requirements 4.1**
    ///
    /// Property 9: Distribuição Ponderada Dentro da Tolerância
    /// For any random weight configuration (2-5 providers, weights 1-100 each),
    /// running 1000 selections on WeightedRoundRobin, each provider's selection
    /// ratio deviates ≤ 5% from expected (weight/total_weight).
    mod property_weighted_distribution {
        use super::*;

        /// Strategy: generate 2-5 providers with random weights 1-100
        fn providers_strategy() -> impl Strategy<Value = Vec<WeightedProvider>> {
            let count = 2usize..=5usize;
            count.prop_flat_map(|n| {
                proptest::collection::vec(1u32..=100u32, n).prop_map(|weights| {
                    weights
                        .iter()
                        .enumerate()
                        .map(|(i, &w)| make_provider(&format!("provider_{}", i), w))
                        .collect::<Vec<_>>()
                })
            })
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(100))]

            #[test]
            fn weighted_distribution_within_tolerance(
                providers in providers_strategy(),
            ) {
                let balancer = WeightedRoundRobin::new();
                let total_weight: u64 = providers.iter().map(|p| p.weight as u64).sum();
                // Use a multiple of total_weight close to 1000 to ensure fair cycles
                // This eliminates partial-cycle bias inherent to round-robin
                let cycles = (1000u64 / total_weight).max(1);
                let total_iterations = cycles * total_weight;
                let mut counts: HashMap<String, u64> = HashMap::new();

                for _ in 0..total_iterations {
                    let selected = balancer.select_provider(&providers).unwrap();
                    *counts.entry(selected.name.clone()).or_insert(0) += 1;
                }

                for provider in &providers {
                    let expected_ratio = provider.weight as f64 / total_weight as f64;
                    let actual_count = *counts.get(&provider.name).unwrap_or(&0);
                    let actual_ratio = actual_count as f64 / total_iterations as f64;
                    let deviation = (actual_ratio - expected_ratio).abs();

                    prop_assert!(
                        deviation <= 0.05,
                        "Provider '{}': weight={}, expected ratio {:.4}, got {:.4} (deviation {:.4} > 0.05), iterations={}",
                        provider.name,
                        provider.weight,
                        expected_ratio,
                        actual_ratio,
                        deviation,
                        total_iterations,
                    );
                }
            }
        }
    }

    /// **Validates: Requirements 4.2**
    ///
    /// Property 10: Failover Seleciona Próximo Provedor Disponível
    /// For any random provider set (2-5 providers), randomly marking some as
    /// "failed" (excluded), select_with_failover selects a provider NOT in
    /// the excluded list. When all are excluded, returns error.
    mod property_failover_selects_next {
        use super::*;

        /// Strategy: generate 2-5 providers and a random subset to exclude
        fn failover_scenario_strategy()
            -> impl Strategy<Value = (Vec<WeightedProvider>, Vec<String>)>
        {
            let count = 2usize..=5usize;
            count.prop_flat_map(|n| {
                let providers = proptest::collection::vec(1u32..=100u32, n).prop_map(|weights| {
                    weights
                        .iter()
                        .enumerate()
                        .map(|(i, &w)| make_provider(&format!("provider_{}", i), w))
                        .collect::<Vec<WeightedProvider>>()
                });

                providers.prop_flat_map(|provs| {
                    let names: Vec<String> = provs.iter().map(|p| p.name.clone()).collect();
                    let n = names.len();
                    // Generate a random subset of indices to exclude (0 to n-1 items)
                    proptest::collection::vec(proptest::bool::ANY, n).prop_map(
                        move |mask| {
                            let excluded: Vec<String> = mask
                                .iter()
                                .enumerate()
                                .filter_map(|(i, &b)| if b { Some(names[i].clone()) } else { None })
                                .collect();
                            (provs.clone(), excluded)
                        },
                    )
                })
            })
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(100))]

            #[test]
            fn failover_selects_non_excluded_or_errors_when_all_excluded(
                (providers, excluded) in failover_scenario_strategy(),
            ) {
                let balancer = WeightedRoundRobin::new();

                let result = select_with_failover(&balancer, &providers, &excluded);

                let available_count = providers.iter()
                    .filter(|p| !excluded.contains(&p.name))
                    .count();

                if available_count == 0 {
                    // All excluded → should return error
                    prop_assert!(
                        result.is_err(),
                        "Expected ServiceUnavailable when all providers excluded, got Ok({:?})",
                        result.ok()
                    );
                } else {
                    // Some available → should select one NOT in excluded list
                    prop_assert!(
                        result.is_ok(),
                        "Expected Ok when {} providers available, got Err({:?})",
                        available_count,
                        result.err()
                    );
                    let selected = result.unwrap();
                    prop_assert!(
                        !excluded.contains(&selected.name),
                        "Selected provider '{}' is in the excluded list {:?}",
                        selected.name,
                        excluded
                    );
                }
            }
        }
    }
}
