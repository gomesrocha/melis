//! Model-to-provider routing validation.
//!
//! Provides utilities to validate that a requested model is configured
//! in the gateway's provider list or route definitions, and to resolve
//! which provider serves that model.

use crate::config::ProviderConfig;
use crate::error::GatewayError;
use crate::route_config::RouteConfigFile;

/// Validates that the given model is configured and resolves the provider that serves it.
///
/// Resolution order:
/// 1. Check if the model appears in any provider's `models` list (from global config).
/// 2. Check if the model is configured in a route definition (single-provider `model` field
///    or multi-provider `providers[].model` field).
///
/// Returns the provider id/name if found.
/// Returns `GatewayError::BadRequest` if the model is not configured anywhere.
pub fn validate_model_routing(
    model: &str,
    providers: &[ProviderConfig],
    route_config: &RouteConfigFile,
) -> Result<String, GatewayError> {
    // 1. Check provider models list in global config
    if let Some(provider) = find_provider_by_model(model, providers) {
        return Ok(provider);
    }

    // 2. Check route definitions
    if let Some(provider) = find_provider_in_routes(model, route_config) {
        return Ok(provider);
    }

    Err(GatewayError::BadRequest(format!(
        "model '{}' is not configured in any provider",
        model
    )))
}

/// Searches the global providers list for one that supports the given model.
/// Returns the provider `id` if found.
fn find_provider_by_model(model: &str, providers: &[ProviderConfig]) -> Option<String> {
    providers
        .iter()
        .find(|p| p.models.iter().any(|m| m == model))
        .map(|p| p.id.clone())
}

/// Searches route definitions for one that references the given model.
/// Returns the provider name associated with that route if found.
fn find_provider_in_routes(model: &str, route_config: &RouteConfigFile) -> Option<String> {
    for route in &route_config.routes {
        // Check single-provider route with model override
        if let Some(ref route_model) = route.model {
            if route_model == model {
                if let Some(ref provider) = route.provider {
                    return Some(provider.clone());
                }
            }
        }

        // Check multi-provider route
        if let Some(ref providers) = route.providers {
            for wp in providers {
                if wp.model == model {
                    return Some(wp.name.clone());
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod proptest_tests {
    use super::*;
    use proptest::prelude::*;
    use crate::config::ProviderConfig;
    use crate::route_config::RouteConfigFile;

    /// **Validates: Requirements 7.4**
    ///
    /// Property 15: Roteamento Modelo-para-Provedor
    /// For any model name and provider configurations:
    /// - models in a provider's list → Ok(provider_id)
    /// - models not in any provider → Err(BadRequest)
    mod property_model_routing {
        use super::*;

        /// Generate a random provider configuration with known models.
        fn providers_strategy() -> impl Strategy<Value = Vec<ProviderConfig>> {
            proptest::collection::vec(
                (
                    "[a-z]{3,8}-[0-9]{1,2}",  // provider id
                    proptest::collection::vec("[a-z]{2,6}-[0-9]\\.[0-9]", 1..4), // models
                ),
                1..4,
            )
            .prop_map(|items| {
                items
                    .into_iter()
                    .map(|(id, models)| ProviderConfig {
                        id: id.clone(),
                        provider_type: "openai".to_string(),
                        base_url: "https://example.com".to_string(),
                        api_key: "sk-test".to_string(),
                        weight: 1,
                        timeout_secs: 30,
                        models,
                    })
                    .collect()
            })
        }

        fn empty_route_config() -> RouteConfigFile {
            RouteConfigFile {
                routes: vec![],
                custom_providers: vec![],
            }
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(100))]

            #[test]
            fn model_in_provider_list_resolves_correctly(
                providers in providers_strategy(),
                provider_idx in 0usize..4,
                model_idx in 0usize..4,
            ) {
                // Pick a valid provider and model from the generated data
                let provider_idx = provider_idx % providers.len();
                let provider = &providers[provider_idx];
                let model_idx = model_idx % provider.models.len();
                let model = &provider.models[model_idx];

                let route_config = empty_route_config();
                let result = validate_model_routing(model, &providers, &route_config);

                prop_assert!(result.is_ok(), "Expected Ok for model '{}' in provider '{}', got: {:?}", model, provider.id, result);
                // The returned provider should be the one containing this model
                let resolved = result.unwrap();
                // Find which provider actually contains this model (first match wins)
                let expected_provider = providers.iter()
                    .find(|p| p.models.contains(&model.to_string()))
                    .unwrap();
                prop_assert_eq!(&resolved, &expected_provider.id);
            }

            #[test]
            fn model_not_in_any_provider_returns_bad_request(
                providers in providers_strategy(),
                unknown_model in "unknown-model-[a-z]{3,8}",
            ) {
                let route_config = empty_route_config();
                let result = validate_model_routing(&unknown_model, &providers, &route_config);

                prop_assert!(result.is_err(), "Expected Err for unknown model '{}', got: {:?}", unknown_model, result);
                match result.unwrap_err() {
                    GatewayError::BadRequest(msg) => {
                        prop_assert!(msg.contains(&unknown_model));
                    }
                    other => prop_assert!(false, "Expected BadRequest, got: {:?}", other),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ProviderConfig;
    use crate::route_config::RouteConfigFile;

    fn sample_providers() -> Vec<ProviderConfig> {
        vec![
            ProviderConfig {
                id: "openai-1".to_string(),
                provider_type: "openai".to_string(),
                base_url: "https://api.openai.com".to_string(),
                api_key: "sk-test".to_string(),
                weight: 1,
                timeout_secs: 30,
                models: vec!["gpt-4o".to_string(), "gpt-4o-mini".to_string()],
            },
            ProviderConfig {
                id: "anthropic-1".to_string(),
                provider_type: "anthropic".to_string(),
                base_url: "https://api.anthropic.com".to_string(),
                api_key: "sk-ant-test".to_string(),
                weight: 1,
                timeout_secs: 30,
                models: vec!["claude-sonnet-4-20250514".to_string()],
            },
        ]
    }

    fn sample_route_config() -> RouteConfigFile {
        let yaml = r#"
custom_providers:
  - name: "internal_llm"
    base_url: "http://internal-llm.corp:8080"
    api_format: "openai_compatible"

routes:
  - path: "/v1/chat/agent"
    method: "POST"
    provider: "openai"
    model: "gpt-4o"

  - path: "/v1/chat/support"
    method: "POST"
    provider: "anthropic"
    model: "claude-sonnet-4-20250514"

  - path: "/v1/chat/completions"
    method: "POST"
    providers:
      - name: "openai"
        weight: 60
        model: "gpt-4o"
      - name: "anthropic"
        weight: 30
        model: "claude-sonnet-4-20250514"
      - name: "google_vertex_ai"
        weight: 10
        model: "gemini-pro"

  - path: "/v1/chat/internal"
    method: "POST"
    provider: "internal_llm"
    model: "custom-fine-tuned-v2"
"#;
        serde_yaml::from_str(yaml).unwrap()
    }

    #[test]
    fn test_valid_model_found_in_provider_models_list() {
        let providers = sample_providers();
        let route_config = sample_route_config();

        let result = validate_model_routing("gpt-4o", &providers, &route_config);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "openai-1");
    }

    #[test]
    fn test_valid_model_found_in_second_provider() {
        let providers = sample_providers();
        let route_config = sample_route_config();

        let result = validate_model_routing("claude-sonnet-4-20250514", &providers, &route_config);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "anthropic-1");
    }

    #[test]
    fn test_model_found_in_route_definition_single_provider() {
        // Use empty providers list to force route lookup
        let providers: Vec<ProviderConfig> = vec![];
        let route_config = sample_route_config();

        let result = validate_model_routing("custom-fine-tuned-v2", &providers, &route_config);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "internal_llm");
    }

    #[test]
    fn test_model_found_in_route_multi_provider() {
        // Use empty providers list to force route lookup
        let providers: Vec<ProviderConfig> = vec![];
        let route_config = sample_route_config();

        let result = validate_model_routing("gemini-pro", &providers, &route_config);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "google_vertex_ai");
    }

    #[test]
    fn test_invalid_model_returns_bad_request() {
        let providers = sample_providers();
        let route_config = sample_route_config();

        let result = validate_model_routing("nonexistent-model", &providers, &route_config);
        assert!(result.is_err());
        match result.unwrap_err() {
            GatewayError::BadRequest(msg) => {
                assert!(msg.contains("nonexistent-model"));
                assert!(msg.contains("is not configured in any provider"));
            }
            other => panic!("Expected BadRequest, got {:?}", other),
        }
    }

    #[test]
    fn test_empty_model_returns_bad_request() {
        let providers = sample_providers();
        let route_config = sample_route_config();

        let result = validate_model_routing("", &providers, &route_config);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), GatewayError::BadRequest(_)));
    }

    #[test]
    fn test_provider_models_list_takes_priority_over_routes() {
        // When a model exists in both a provider's models list AND in routes,
        // the provider id from the global config should be returned (resolution order)
        let providers = sample_providers();
        let route_config = sample_route_config();

        // "gpt-4o" is in openai-1's models list AND in route definitions
        let result = validate_model_routing("gpt-4o", &providers, &route_config);
        assert!(result.is_ok());
        // Should return the provider id from global config, not route name
        assert_eq!(result.unwrap(), "openai-1");
    }

    #[test]
    fn test_no_providers_and_no_routes_returns_error() {
        let providers: Vec<ProviderConfig> = vec![];
        let route_config = RouteConfigFile {
            routes: vec![],
            custom_providers: vec![],
        };

        let result = validate_model_routing("any-model", &providers, &route_config);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), GatewayError::BadRequest(_)));
    }
}
