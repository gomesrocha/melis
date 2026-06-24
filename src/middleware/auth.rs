//! Authentication middleware.
//!
//! Extracts and validates credentials from the Authorization header.
//! Implements Bearer token authentication with API key lookup from configuration.

use async_trait::async_trait;
use axum::{
    extract::Request,
    http::header::AUTHORIZATION,
    middleware::Next,
    response::Response,
};
use std::sync::Arc;

use crate::config::{AuthConfig, ClientRateLimit};
use crate::error::GatewayError;

/// Identity of an authenticated client, inserted as a request extension.
#[derive(Debug, Clone)]
pub struct ClientIdentity {
    pub client_id: String,
    pub rate_limit_config: Option<ClientRateLimit>,
    pub allowed_models: Vec<String>,
}

/// Trait for credential validation.
#[async_trait]
pub trait AuthValidator: Send + Sync {
    /// Validates a bearer token and returns the associated client identity.
    async fn validate(&self, token: &str) -> Result<ClientIdentity, GatewayError>;
}

/// Auth validator backed by the gateway's static configuration (API keys in config file).
#[derive(Debug, Clone)]
pub struct ConfigAuthValidator {
    config: AuthConfig,
}

impl ConfigAuthValidator {
    /// Creates a new ConfigAuthValidator from the given AuthConfig.
    pub fn new(config: AuthConfig) -> Self {
        Self { config }
    }
}

#[async_trait]
impl AuthValidator for ConfigAuthValidator {
    async fn validate(&self, token: &str) -> Result<ClientIdentity, GatewayError> {
        let entry = self
            .config
            .api_keys
            .iter()
            .find(|entry| entry.key == token)
            .ok_or_else(|| GatewayError::Unauthorized("Invalid API key".to_string()))?;

        Ok(ClientIdentity {
            client_id: entry.client_id.clone(),
            rate_limit_config: entry.rate_limit.clone(),
            allowed_models: entry.allowed_models.clone(),
        })
    }
}

/// Axum middleware function that extracts the Bearer token from the Authorization header,
/// validates it using the provided `AuthValidator`, and inserts `ClientIdentity` as a
/// request extension on success.
#[tracing::instrument(skip(request, next), fields(middleware = "auth"))]
pub async fn auth_middleware(
    request: Request,
    next: Next,
) -> Result<Response, GatewayError> {
    let validator = request
        .extensions()
        .get::<Arc<dyn AuthValidator>>()
        .cloned()
        .expect("AuthValidator must be added as a request extension before auth_middleware");

    let auth_header = request
        .headers()
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok());

    let token = match auth_header {
        None => {
            return Err(GatewayError::Unauthorized(
                "Missing Authorization header".to_string(),
            ));
        }
        Some(header_value) => {
            let parts: Vec<&str> = header_value.splitn(2, ' ').collect();
            if parts.len() != 2 || !parts[0].eq_ignore_ascii_case("Bearer") || parts[1].is_empty()
            {
                return Err(GatewayError::Unauthorized(
                    "Invalid Authorization format. Expected 'Bearer <token>'".to_string(),
                ));
            }
            parts[1].to_string()
        }
    };

    let identity = validator.validate(&token).await?;

    let mut request = request;
    request.extensions_mut().insert(identity);

    Ok(next.run(request).await)
}

#[cfg(test)]
mod proptest_tests {
    use super::*;
    use proptest::prelude::*;

    use crate::config::{ApiKeyEntry, AuthConfig, ClientRateLimit};

    /// **Validates: Requirements 7.2, 7.3**
    ///
    /// Property 14: Corretude da Autenticação
    /// For any random token, ConfigAuthValidator with a known set of keys:
    /// - known keys → Ok(ClientIdentity with correct client_id)
    /// - unknown keys → Err(Unauthorized)
    mod property_auth_correctness {
        use super::*;

        fn test_auth_config_with_keys(keys: &[(&str, &str)]) -> AuthConfig {
            AuthConfig {
                enabled: true,
                api_keys: keys
                    .iter()
                    .map(|(key, client_id)| ApiKeyEntry {
                        key: key.to_string(),
                        client_id: client_id.to_string(),
                        allowed_models: vec![],
                        rate_limit: None,
                    })
                    .collect(),
            }
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(100))]

            #[test]
            fn known_keys_authenticate_correctly(
                idx in 0usize..3,
            ) {
                let known_keys = vec![
                    ("sk-key-alpha-001", "client-alpha"),
                    ("sk-key-beta-002", "client-beta"),
                    ("sk-key-gamma-003", "client-gamma"),
                ];
                let config = test_auth_config_with_keys(&known_keys);
                let validator = ConfigAuthValidator::new(config);

                let (key, expected_client) = known_keys[idx];
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                let result = rt.block_on(validator.validate(key));

                prop_assert!(result.is_ok(), "Expected Ok for known key '{}', got: {:?}", key, result);
                let identity = result.unwrap();
                prop_assert_eq!(identity.client_id, expected_client.to_string());
            }

            #[test]
            fn unknown_keys_return_unauthorized(
                random_token in "[a-zA-Z0-9_-]{5,40}",
            ) {
                // Known keys that definitely won't collide with random alphanumeric tokens
                let known_keys = vec![
                    ("sk-EXACT-key-alpha-001!@#", "client-alpha"),
                    ("sk-EXACT-key-beta-002!@#", "client-beta"),
                ];
                let config = test_auth_config_with_keys(&known_keys);
                let validator = ConfigAuthValidator::new(config);

                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                let result = rt.block_on(validator.validate(&random_token));

                prop_assert!(result.is_err(), "Expected Err for unknown token '{}', got: {:?}", random_token, result);
                match result.unwrap_err() {
                    GatewayError::Unauthorized(_) => {} // expected
                    other => prop_assert!(false, "Expected Unauthorized, got: {:?}", other),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::Body,
        http::{Request as HttpRequest, StatusCode},
        middleware,
        routing::get,
        Router,
    };
    use tower::ServiceExt;

    use crate::config::{ApiKeyEntry, AuthConfig, ClientRateLimit};

    fn test_auth_config() -> AuthConfig {
        AuthConfig {
            enabled: true,
            api_keys: vec![
                ApiKeyEntry {
                    key: "sk-valid-key-123".to_string(),
                    client_id: "client-alpha".to_string(),
                    allowed_models: vec!["gpt-4o".to_string(), "claude-sonnet-4-20250514".to_string()],
                    rate_limit: Some(ClientRateLimit {
                        burst_capacity: 50,
                        refill_rate: 5.0,
                    }),
                },
                ApiKeyEntry {
                    key: "sk-another-key-456".to_string(),
                    client_id: "client-beta".to_string(),
                    allowed_models: vec![],
                    rate_limit: None,
                },
            ],
        }
    }

    fn build_test_app() -> Router {
        let validator: Arc<dyn AuthValidator> =
            Arc::new(ConfigAuthValidator::new(test_auth_config()));

        Router::new()
            .route("/protected", get(handler))
            .layer(middleware::from_fn(auth_middleware))
            .layer(axum::Extension(validator))
    }

    async fn handler(
        axum::Extension(identity): axum::Extension<ClientIdentity>,
    ) -> String {
        identity.client_id
    }

    #[tokio::test]
    async fn test_missing_authorization_header() {
        let app = build_test_app();

        let request = HttpRequest::builder()
            .uri("/protected")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body_str = String::from_utf8_lossy(&body);
        assert!(body_str.contains("Missing Authorization header"));
    }

    #[tokio::test]
    async fn test_invalid_authorization_format_no_bearer() {
        let app = build_test_app();

        let request = HttpRequest::builder()
            .uri("/protected")
            .header("Authorization", "Basic abc123")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body_str = String::from_utf8_lossy(&body);
        assert!(body_str.contains("Invalid Authorization format"));
    }

    #[tokio::test]
    async fn test_invalid_authorization_format_empty_token() {
        let app = build_test_app();

        let request = HttpRequest::builder()
            .uri("/protected")
            .header("Authorization", "Bearer ")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body_str = String::from_utf8_lossy(&body);
        assert!(body_str.contains("Invalid Authorization format"));
    }

    #[tokio::test]
    async fn test_invalid_api_key() {
        let app = build_test_app();

        let request = HttpRequest::builder()
            .uri("/protected")
            .header("Authorization", "Bearer sk-nonexistent-key")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body_str = String::from_utf8_lossy(&body);
        assert!(body_str.contains("Invalid API key"));
    }

    #[tokio::test]
    async fn test_valid_api_key() {
        let app = build_test_app();

        let request = HttpRequest::builder()
            .uri("/protected")
            .header("Authorization", "Bearer sk-valid-key-123")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body_str = String::from_utf8_lossy(&body);
        assert_eq!(body_str, "client-alpha");
    }

    #[tokio::test]
    async fn test_valid_api_key_second_client() {
        let app = build_test_app();

        let request = HttpRequest::builder()
            .uri("/protected")
            .header("Authorization", "Bearer sk-another-key-456")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body_str = String::from_utf8_lossy(&body);
        assert_eq!(body_str, "client-beta");
    }
}
