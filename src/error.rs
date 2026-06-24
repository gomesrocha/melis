//! Error types module.
//!
//! Defines error responses and internal error types for the gateway.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;

/// Standard error response body returned to clients.
#[derive(Debug, Serialize)]
pub struct ErrorResponse {
    pub error: ErrorDetail,
}

/// Error detail within the response body.
#[derive(Debug, Serialize)]
pub struct ErrorDetail {
    pub message: String,
    pub r#type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
}

/// Internal gateway error enum.
#[derive(Debug, thiserror::Error)]
pub enum GatewayError {
    #[error("Bad request: {0}")]
    BadRequest(String),

    #[error("Unauthorized: {0}")]
    Unauthorized(String),

    #[error("Rate limit exceeded")]
    RateLimited { retry_after_secs: u64 },

    #[error("Payload too large")]
    PayloadTooLarge,

    #[error("Service unavailable: {0}")]
    ServiceUnavailable(String),

    #[error("Internal error: {0}")]
    Internal(String),

    #[error("Provider error: {0}")]
    ProviderError(String),
}

impl IntoResponse for GatewayError {
    fn into_response(self) -> Response {
        let (status, error_type, message) = match &self {
            GatewayError::BadRequest(msg) => {
                (StatusCode::BAD_REQUEST, "invalid_request_error", msg.clone())
            }
            GatewayError::Unauthorized(msg) => {
                (StatusCode::UNAUTHORIZED, "authentication_error", msg.clone())
            }
            GatewayError::RateLimited { retry_after_secs } => (
                StatusCode::TOO_MANY_REQUESTS,
                "rate_limit_error",
                format!("Rate limit exceeded. Retry after {} seconds.", retry_after_secs),
            ),
            GatewayError::PayloadTooLarge => (
                StatusCode::PAYLOAD_TOO_LARGE,
                "invalid_request_error",
                "Payload size exceeds 10MB limit.".to_string(),
            ),
            GatewayError::ServiceUnavailable(msg) => {
                (StatusCode::SERVICE_UNAVAILABLE, "service_error", msg.clone())
            }
            GatewayError::Internal(msg) => {
                (StatusCode::INTERNAL_SERVER_ERROR, "internal_error", msg.clone())
            }
            GatewayError::ProviderError(msg) => {
                (StatusCode::BAD_GATEWAY, "provider_error", msg.clone())
            }
        };

        let body = ErrorResponse {
            error: ErrorDetail {
                message,
                r#type: error_type.to_string(),
                code: None,
            },
        };

        let mut response = axum::Json(body).into_response();
        *response.status_mut() = status;

        // Add Retry-After header for rate-limited responses
        if let GatewayError::RateLimited { retry_after_secs } = &self {
            response.headers_mut().insert(
                "Retry-After",
                retry_after_secs.to_string().parse().unwrap(),
            );
        }

        response
    }
}
