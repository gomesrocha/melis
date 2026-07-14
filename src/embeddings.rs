//! OpenAI-compatible embeddings endpoint.
//!
//! Implements POST /v1/embeddings with Ollama backend support.
//! Supports both /api/embed (modern) and /api/embeddings (legacy) Ollama endpoints.

use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use std::time::Duration;

use crate::error::GatewayError;
use crate::state::AppState;

// ─── Request / Response Models ────────────────────────────────────────────────

/// OpenAI-compatible embeddings request.
#[derive(Debug, Deserialize)]
pub struct EmbeddingsRequest {
    pub model: Option<String>,
    pub input: Option<EmbeddingsInput>,
    /// Accepted but only "float" is supported.
    #[serde(default)]
    pub encoding_format: Option<String>,
    /// Accepted but ignored in this version.
    #[serde(default)]
    pub dimensions: Option<u32>,
    /// Accepted but ignored.
    #[serde(default)]
    pub user: Option<String>,
}

/// Input can be a single string or an array of strings.
#[derive(Debug, Deserialize, Clone)]
#[serde(untagged)]
pub enum EmbeddingsInput {
    Single(String),
    Multiple(Vec<String>),
}

impl EmbeddingsInput {
    /// Convert to a Vec<String> regardless of variant.
    pub fn into_vec(self) -> Vec<String> {
        match self {
            EmbeddingsInput::Single(s) => vec![s],
            EmbeddingsInput::Multiple(v) => v,
        }
    }

    /// Check if input is empty.
    pub fn is_empty(&self) -> bool {
        match self {
            EmbeddingsInput::Single(s) => s.is_empty(),
            EmbeddingsInput::Multiple(v) => v.is_empty() || v.iter().all(|s| s.is_empty()),
        }
    }
}

/// OpenAI-compatible embeddings response.
#[derive(Debug, Serialize)]
pub struct EmbeddingsResponse {
    pub object: &'static str,
    pub data: Vec<EmbeddingData>,
    pub model: String,
    pub usage: EmbeddingsUsage,
}

/// A single embedding entry in the response.
#[derive(Debug, Serialize)]
pub struct EmbeddingData {
    pub object: &'static str,
    pub index: usize,
    pub embedding: Vec<f64>,
}

/// Token usage for embeddings (best-effort, Ollama may not provide this).
#[derive(Debug, Serialize)]
pub struct EmbeddingsUsage {
    pub prompt_tokens: u64,
    pub total_tokens: u64,
}

// ─── Ollama request/response models ──────────────────────────────────────────

/// Ollama modern /api/embed request.
#[derive(Debug, Serialize)]
struct OllamaEmbedRequest {
    model: String,
    input: Vec<String>,
}

/// Ollama modern /api/embed response.
#[derive(Debug, Deserialize)]
struct OllamaEmbedResponse {
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    embeddings: Vec<Vec<f64>>,
}

/// Ollama legacy /api/embeddings request (single input).
#[derive(Debug, Serialize)]
struct OllamaEmbeddingsLegacyRequest {
    model: String,
    prompt: String,
}

/// Ollama legacy /api/embeddings response.
#[derive(Debug, Deserialize)]
struct OllamaEmbeddingsLegacyResponse {
    #[serde(default)]
    embedding: Vec<f64>,
}

// ─── Validation ──────────────────────────────────────────────────────────────

/// Validates the embeddings request payload.
pub fn validate_embeddings_request(req: &EmbeddingsRequest) -> Result<(), GatewayError> {
    // Validate model
    match &req.model {
        None => {
            return Err(GatewayError::BadRequest(
                "Field 'model' is required and must be a non-empty string.".to_string(),
            ));
        }
        Some(m) if m.is_empty() => {
            return Err(GatewayError::BadRequest(
                "Field 'model' must not be empty.".to_string(),
            ));
        }
        _ => {}
    }

    // Validate input
    match &req.input {
        None => {
            return Err(GatewayError::BadRequest(
                "Field 'input' is required.".to_string(),
            ));
        }
        Some(input) if input.is_empty() => {
            return Err(GatewayError::BadRequest(
                "Field 'input' must not be empty.".to_string(),
            ));
        }
        _ => {}
    }

    // Validate encoding_format if provided
    if let Some(ref fmt) = req.encoding_format {
        if fmt != "float" {
            return Err(GatewayError::BadRequest(format!(
                "encoding_format '{}' is not supported. Only 'float' is supported.",
                fmt
            )));
        }
    }

    Ok(())
}

// ─── Handler ─────────────────────────────────────────────────────────────────

/// POST /v1/embeddings handler.
///
/// Pipeline:
/// 1. Parse and validate request
/// 2. Resolve provider (Ollama) from config
/// 3. Call Ollama /api/embed (with fallback to /api/embeddings)
/// 4. Transform response to OpenAI-compatible format
#[tracing::instrument(skip(state, payload), fields(endpoint = "/v1/embeddings"))]
pub async fn embeddings_handler(
    State(state): State<AppState>,
    Json(payload): Json<EmbeddingsRequest>,
) -> Result<Response, GatewayError> {
    let request_start = std::time::Instant::now();

    // Validate request
    validate_embeddings_request(&payload)?;

    let model = payload.model.unwrap(); // Safe after validation
    let inputs = payload.input.unwrap().into_vec(); // Safe after validation
    let input_count = inputs.len();

    tracing::info!(
        model = %model,
        input_count = input_count,
        "Processing embeddings request"
    );

    // Find the Ollama provider config
    let provider_config = state
        .gateway_config
        .providers
        .iter()
        .find(|p| p.provider_type == "ollama")
        .ok_or_else(|| {
            GatewayError::ServiceUnavailable(
                "No Ollama provider configured for embeddings.".to_string(),
            )
        })?;

    let base_url = &provider_config.base_url;
    let timeout = provider_config.timeout();

    // Try modern /api/embed first
    let embeddings = match call_ollama_embed(&state, base_url, &model, &inputs, timeout).await {
        Ok(embs) => embs,
        Err(EmbeddingProviderError::NotFound) => {
            // Fallback to legacy /api/embeddings
            tracing::info!("Ollama /api/embed returned 404, falling back to /api/embeddings");
            match call_ollama_embeddings_legacy(&state, base_url, &model, &inputs, timeout).await {
                Ok(embs) => embs,
                Err(EmbeddingProviderError::NotFound) => {
                    return Err(GatewayError::ServiceUnavailable(
                        "Ollama does not support embeddings endpoints.".to_string(),
                    ));
                }
                Err(EmbeddingProviderError::Gateway(e)) => return Err(e),
            }
        }
        Err(EmbeddingProviderError::Gateway(e)) => return Err(e),
    };

    // Validate response count matches input count
    if embeddings.len() != input_count {
        tracing::error!(
            expected = input_count,
            got = embeddings.len(),
            "Ollama returned different number of embeddings than inputs"
        );
        return Err(GatewayError::ProviderError(format!(
            "Provider returned {} embeddings for {} inputs",
            embeddings.len(),
            input_count
        )));
    }

    // Check for empty embeddings
    if embeddings.iter().any(|e| e.is_empty()) {
        return Err(GatewayError::ProviderError(
            "Provider returned empty embedding vector.".to_string(),
        ));
    }

    // Build OpenAI-compatible response
    let data: Vec<EmbeddingData> = embeddings
        .into_iter()
        .enumerate()
        .map(|(idx, embedding)| EmbeddingData {
            object: "embedding",
            index: idx,
            embedding,
        })
        .collect();

    let response = EmbeddingsResponse {
        object: "list",
        data,
        model: model.clone(),
        usage: EmbeddingsUsage {
            prompt_tokens: 0,
            total_tokens: 0,
        },
    };

    let elapsed = request_start.elapsed().as_secs_f64();
    tracing::info!(
        model = %model,
        input_count = input_count,
        embeddings_count = response.data.len(),
        elapsed_secs = elapsed,
        "Embeddings request completed successfully"
    );

    // Record metrics
    state
        .metrics
        .requests_total
        .with_label_values(&["/v1/embeddings", "unknown", "200"])
        .inc();
    state
        .metrics
        .request_duration
        .with_label_values(&["/v1/embeddings", "ollama"])
        .observe(elapsed);

    Ok(Json(response).into_response())
}

// ─── Ollama Integration ──────────────────────────────────────────────────────

/// Internal error type for embedding provider calls.
enum EmbeddingProviderError {
    /// The endpoint returned 404 (use fallback).
    NotFound,
    /// A gateway error to propagate.
    Gateway(GatewayError),
}

/// Calls Ollama's modern /api/embed endpoint.
async fn call_ollama_embed(
    state: &AppState,
    base_url: &str,
    model: &str,
    inputs: &[String],
    timeout: Duration,
) -> Result<Vec<Vec<f64>>, EmbeddingProviderError> {
    let url = format!("{}/api/embed", base_url.trim_end_matches('/'));

    let request_body = OllamaEmbedRequest {
        model: model.to_string(),
        input: inputs.to_vec(),
    };

    let body_bytes = serde_json::to_vec(&request_body).map_err(|e| {
        EmbeddingProviderError::Gateway(GatewayError::Internal(format!(
            "Failed to serialize embed request: {}",
            e
        )))
    })?;

    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::CONTENT_TYPE,
        "application/json".parse().unwrap(),
    );

    match state
        .http_client
        .send(&url, headers, bytes::Bytes::from(body_bytes), timeout)
        .await
    {
        Ok(response_bytes) => {
            let response: OllamaEmbedResponse =
                serde_json::from_slice(&response_bytes).map_err(|e| {
                    EmbeddingProviderError::Gateway(GatewayError::ProviderError(format!(
                        "Failed to parse Ollama /api/embed response: {}",
                        e
                    )))
                })?;
            Ok(response.embeddings)
        }
        Err(crate::client::ClientError::HttpError { status: 404, .. }) => {
            Err(EmbeddingProviderError::NotFound)
        }
        Err(crate::client::ClientError::Timeout) => Err(EmbeddingProviderError::Gateway(
            GatewayError::ServiceUnavailable("Ollama timeout on /api/embed".to_string()),
        )),
        Err(e) => Err(EmbeddingProviderError::Gateway(
            GatewayError::ServiceUnavailable(format!("Ollama unavailable: {}", e)),
        )),
    }
}

/// Calls Ollama's legacy /api/embeddings endpoint (one input at a time).
async fn call_ollama_embeddings_legacy(
    state: &AppState,
    base_url: &str,
    model: &str,
    inputs: &[String],
    timeout: Duration,
) -> Result<Vec<Vec<f64>>, EmbeddingProviderError> {
    let url = format!("{}/api/embeddings", base_url.trim_end_matches('/'));
    let mut results: Vec<Vec<f64>> = Vec::with_capacity(inputs.len());

    for input in inputs {
        let request_body = OllamaEmbeddingsLegacyRequest {
            model: model.to_string(),
            prompt: input.clone(),
        };

        let body_bytes = serde_json::to_vec(&request_body).map_err(|e| {
            EmbeddingProviderError::Gateway(GatewayError::Internal(format!(
                "Failed to serialize embeddings request: {}",
                e
            )))
        })?;

        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::CONTENT_TYPE,
            "application/json".parse().unwrap(),
        );

        match state
            .http_client
            .send(&url, headers, bytes::Bytes::from(body_bytes), timeout)
            .await
        {
            Ok(response_bytes) => {
                let response: OllamaEmbeddingsLegacyResponse =
                    serde_json::from_slice(&response_bytes).map_err(|e| {
                        EmbeddingProviderError::Gateway(GatewayError::ProviderError(format!(
                            "Failed to parse Ollama /api/embeddings response: {}",
                            e
                        )))
                    })?;
                results.push(response.embedding);
            }
            Err(crate::client::ClientError::Timeout) => {
                return Err(EmbeddingProviderError::Gateway(
                    GatewayError::ServiceUnavailable(
                        "Ollama timeout on /api/embeddings".to_string(),
                    ),
                ));
            }
            Err(e) => {
                return Err(EmbeddingProviderError::Gateway(
                    GatewayError::ServiceUnavailable(format!("Ollama unavailable: {}", e)),
                ));
            }
        }
    }

    Ok(results)
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_request_single_input() {
        let json = r#"{
            "model": "nomic-embed-text:latest",
            "input": "Hello world"
        }"#;
        let req: EmbeddingsRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.model.unwrap(), "nomic-embed-text:latest");
        match req.input.unwrap() {
            EmbeddingsInput::Single(s) => assert_eq!(s, "Hello world"),
            _ => panic!("Expected Single input"),
        }
    }

    #[test]
    fn test_parse_request_array_input() {
        let json = r#"{
            "model": "nomic-embed-text:latest",
            "input": ["Text 1", "Text 2", "Text 3"]
        }"#;
        let req: EmbeddingsRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.model.unwrap(), "nomic-embed-text:latest");
        match req.input.unwrap() {
            EmbeddingsInput::Multiple(v) => {
                assert_eq!(v.len(), 3);
                assert_eq!(v[0], "Text 1");
                assert_eq!(v[1], "Text 2");
                assert_eq!(v[2], "Text 3");
            }
            _ => panic!("Expected Multiple input"),
        }
    }

    #[test]
    fn test_validate_missing_model() {
        let req = EmbeddingsRequest {
            model: None,
            input: Some(EmbeddingsInput::Single("test".to_string())),
            encoding_format: None,
            dimensions: None,
            user: None,
        };
        let err = validate_embeddings_request(&req).unwrap_err();
        match err {
            GatewayError::BadRequest(msg) => assert!(msg.contains("model")),
            _ => panic!("Expected BadRequest"),
        }
    }

    #[test]
    fn test_validate_empty_model() {
        let req = EmbeddingsRequest {
            model: Some("".to_string()),
            input: Some(EmbeddingsInput::Single("test".to_string())),
            encoding_format: None,
            dimensions: None,
            user: None,
        };
        let err = validate_embeddings_request(&req).unwrap_err();
        match err {
            GatewayError::BadRequest(msg) => assert!(msg.contains("model")),
            _ => panic!("Expected BadRequest"),
        }
    }

    #[test]
    fn test_validate_missing_input() {
        let req = EmbeddingsRequest {
            model: Some("nomic-embed-text:latest".to_string()),
            input: None,
            encoding_format: None,
            dimensions: None,
            user: None,
        };
        let err = validate_embeddings_request(&req).unwrap_err();
        match err {
            GatewayError::BadRequest(msg) => assert!(msg.contains("input")),
            _ => panic!("Expected BadRequest"),
        }
    }

    #[test]
    fn test_validate_empty_input_array() {
        let req = EmbeddingsRequest {
            model: Some("nomic-embed-text:latest".to_string()),
            input: Some(EmbeddingsInput::Multiple(vec![])),
            encoding_format: None,
            dimensions: None,
            user: None,
        };
        let err = validate_embeddings_request(&req).unwrap_err();
        match err {
            GatewayError::BadRequest(msg) => assert!(msg.contains("input")),
            _ => panic!("Expected BadRequest"),
        }
    }

    #[test]
    fn test_validate_unsupported_encoding_format() {
        let req = EmbeddingsRequest {
            model: Some("nomic-embed-text:latest".to_string()),
            input: Some(EmbeddingsInput::Single("test".to_string())),
            encoding_format: Some("base64".to_string()),
            dimensions: None,
            user: None,
        };
        let err = validate_embeddings_request(&req).unwrap_err();
        match err {
            GatewayError::BadRequest(msg) => assert!(msg.contains("encoding_format")),
            _ => panic!("Expected BadRequest"),
        }
    }

    #[test]
    fn test_validate_valid_request() {
        let req = EmbeddingsRequest {
            model: Some("nomic-embed-text:latest".to_string()),
            input: Some(EmbeddingsInput::Single("test".to_string())),
            encoding_format: Some("float".to_string()),
            dimensions: Some(768),
            user: Some("user-123".to_string()),
        };
        assert!(validate_embeddings_request(&req).is_ok());
    }

    #[test]
    fn test_embeddings_input_into_vec_single() {
        let input = EmbeddingsInput::Single("hello".to_string());
        let vec = input.into_vec();
        assert_eq!(vec, vec!["hello"]);
    }

    #[test]
    fn test_embeddings_input_into_vec_multiple() {
        let input = EmbeddingsInput::Multiple(vec!["a".to_string(), "b".to_string()]);
        let vec = input.into_vec();
        assert_eq!(vec, vec!["a", "b"]);
    }

    #[test]
    fn test_response_serialization_preserves_index() {
        let response = EmbeddingsResponse {
            object: "list",
            data: vec![
                EmbeddingData {
                    object: "embedding",
                    index: 0,
                    embedding: vec![0.1, 0.2, 0.3],
                },
                EmbeddingData {
                    object: "embedding",
                    index: 1,
                    embedding: vec![0.4, 0.5, 0.6],
                },
            ],
            model: "nomic-embed-text:latest".to_string(),
            usage: EmbeddingsUsage {
                prompt_tokens: 0,
                total_tokens: 0,
            },
        };

        let json = serde_json::to_value(&response).unwrap();
        assert_eq!(json["object"], "list");
        assert_eq!(json["data"][0]["object"], "embedding");
        assert_eq!(json["data"][0]["index"], 0);
        assert_eq!(json["data"][1]["index"], 1);
        assert_eq!(json["model"], "nomic-embed-text:latest");
        assert_eq!(json["usage"]["prompt_tokens"], 0);
        assert_eq!(json["usage"]["total_tokens"], 0);
    }

    #[test]
    fn test_parse_request_with_optional_fields() {
        let json = r#"{
            "model": "nomic-embed-text:latest",
            "input": "test",
            "encoding_format": "float",
            "dimensions": 768,
            "user": "user-abc"
        }"#;
        let req: EmbeddingsRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.encoding_format.unwrap(), "float");
        assert_eq!(req.dimensions.unwrap(), 768);
        assert_eq!(req.user.unwrap(), "user-abc");
    }

    #[test]
    fn test_ollama_embed_request_serialization() {
        let req = OllamaEmbedRequest {
            model: "nomic-embed-text:latest".to_string(),
            input: vec!["Hello".to_string(), "World".to_string()],
        };
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["model"], "nomic-embed-text:latest");
        assert_eq!(json["input"][0], "Hello");
        assert_eq!(json["input"][1], "World");
    }

    #[test]
    fn test_ollama_embed_response_deserialization() {
        let json = r#"{
            "model": "nomic-embed-text:latest",
            "embeddings": [
                [0.01, 0.02, 0.03],
                [0.04, 0.05, 0.06]
            ]
        }"#;
        let resp: OllamaEmbedResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.model.unwrap(), "nomic-embed-text:latest");
        assert_eq!(resp.embeddings.len(), 2);
        assert_eq!(resp.embeddings[0], vec![0.01, 0.02, 0.03]);
        assert_eq!(resp.embeddings[1], vec![0.04, 0.05, 0.06]);
    }

    #[test]
    fn test_ollama_legacy_response_deserialization() {
        let json = r#"{
            "embedding": [0.1, 0.2, 0.3, 0.4]
        }"#;
        let resp: OllamaEmbeddingsLegacyResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.embedding, vec![0.1, 0.2, 0.3, 0.4]);
    }
}
