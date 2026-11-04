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

    let base_url = provider_config.base_url.clone();
    let timeout = provider_config.timeout();
    let http_client = state.http_client.clone();

    // Try modern /api/embed first
    let embeddings =
        match call_ollama_embed(http_client.as_ref(), &base_url, &model, &inputs, timeout).await {
            Ok(embs) => embs,
            Err(EmbeddingProviderError::NotFound) => {
                // Fallback to legacy /api/embeddings
                tracing::info!("Ollama /api/embed returned 404, falling back to /api/embeddings");
                match call_ollama_embeddings_legacy(
                    http_client.as_ref(),
                    &base_url,
                    &model,
                    &inputs,
                    timeout,
                )
                .await
                {
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

// ─── Injectable embedding provider (Phase 4) ──────────────────────────────────

/// Abstraction over the embedding backend used by the semantic cache.
///
/// Injected into `AppState` so tests can supply a deterministic stub (enabling
/// end-to-end cache/guardrail tests without a live Ollama), while production uses
/// the Ollama-backed implementation.
#[async_trait::async_trait]
pub trait EmbeddingProvider: Send + Sync {
    /// Compute the embedding for `text` using `model`. Returns `None` on failure
    /// (callers fail open).
    async fn embed(&self, model: &str, text: &str) -> Option<Vec<f64>>;

    /// Embed several texts, preserving input order. `None` if ANY text fails.
    ///
    /// Default: one `embed` call per text, at most `max_concurrency` in flight
    /// (`batch_size` is ignored). Backends with native batching override this.
    /// Callers bound the total time (e.g. `tokio::time::timeout`).
    async fn embed_batch(
        &self,
        model: &str,
        texts: &[String],
        _batch_size: usize,
        max_concurrency: usize,
    ) -> Option<Vec<Vec<f64>>> {
        use futures_util::stream::{self, StreamExt};
        // Owned indices (not `&String`) keep the stream future `Send` without
        // higher-ranked lifetime issues.
        let results: Vec<Option<Vec<f64>>> = stream::iter(0..texts.len())
            .map(|i| self.embed(model, &texts[i]))
            .buffered(max_concurrency.max(1))
            .collect()
            .await;
        results.into_iter().collect()
    }
}

/// `[start, end)` ranges splitting `len` items into chunks of `batch_size` (≥ 1).
pub(crate) fn chunk_ranges(len: usize, batch_size: usize) -> Vec<(usize, usize)> {
    let size = batch_size.max(1);
    (0..len)
        .step_by(size)
        .map(|s| (s, (s + size).min(len)))
        .collect()
}

/// Production embedding provider backed by Ollama via the gateway HTTP client.
///
/// Holds only what embedding needs (provider config + HTTP client), not the full
/// `AppState`, to avoid an `AppState` ⇄ provider reference cycle.
pub struct OllamaEmbeddingProvider {
    providers: Vec<crate::config::ProviderConfig>,
    http_client: std::sync::Arc<dyn crate::client::LlmHttpClient>,
}

impl OllamaEmbeddingProvider {
    pub fn new(
        providers: Vec<crate::config::ProviderConfig>,
        http_client: std::sync::Arc<dyn crate::client::LlmHttpClient>,
    ) -> Self {
        Self {
            providers,
            http_client,
        }
    }
}

#[async_trait::async_trait]
impl EmbeddingProvider for OllamaEmbeddingProvider {
    async fn embed(&self, model: &str, text: &str) -> Option<Vec<f64>> {
        embed_single_with(&self.providers, self.http_client.as_ref(), model, text).await
    }

    /// Native batching: chunks of `batch_size` texts per `/api/embed` call (falls
    /// back to `/api/embeddings`, one text at a time, on 404), at most
    /// `max_concurrency` chunks in flight. Order is preserved.
    async fn embed_batch(
        &self,
        model: &str,
        texts: &[String],
        batch_size: usize,
        max_concurrency: usize,
    ) -> Option<Vec<Vec<f64>>> {
        use futures_util::stream::{self, StreamExt};
        let provider = self
            .providers
            .iter()
            .find(|p| p.provider_type == "ollama")?;
        let base_url = provider.base_url.as_str();
        let timeout = provider.timeout();
        let client = self.http_client.as_ref();

        let ranges = chunk_ranges(texts.len(), batch_size);
        let results: Vec<Option<Vec<Vec<f64>>>> = stream::iter(ranges)
            .map(|(start, end)| async move {
                let chunk = &texts[start..end];
                let r = match call_ollama_embed(client, base_url, model, chunk, timeout).await {
                    Err(EmbeddingProviderError::NotFound) => {
                        call_ollama_embeddings_legacy(client, base_url, model, chunk, timeout).await
                    }
                    other => other,
                };
                match r {
                    Ok(v) if v.len() == chunk.len() => Some(v),
                    _ => None,
                }
            })
            .buffered(max_concurrency.max(1))
            .collect()
            .await;

        let mut out = Vec::with_capacity(texts.len());
        for r in results {
            out.extend(r?);
        }
        Some(out)
    }
}

/// Deterministic embedding provider for tests.
///
/// Maps text to a fixed-dimension vector seeded by a content hash, so equivalent
/// inputs embed identically and distinct inputs embed differently — enough to
/// drive cache hit/miss deterministically without a live Ollama.
#[cfg(test)]
pub struct StubEmbeddingProvider {
    /// If set, always returns this vector regardless of input (for forced hits).
    pub fixed: Option<Vec<f64>>,
    /// If true, `embed` returns `None` (simulates embeddings backend down).
    pub unavailable: bool,
}

#[cfg(test)]
impl StubEmbeddingProvider {
    pub fn deterministic() -> Self {
        Self {
            fixed: None,
            unavailable: false,
        }
    }
}

#[cfg(test)]
#[async_trait::async_trait]
impl EmbeddingProvider for StubEmbeddingProvider {
    async fn embed(&self, _model: &str, text: &str) -> Option<Vec<f64>> {
        if self.unavailable {
            return None;
        }
        if let Some(v) = &self.fixed {
            return Some(v.clone());
        }
        // 8-dim vector seeded by an FNV-1a hash of the text; deterministic.
        let mut hash: u64 = 0xcbf29ce484222325;
        for b in text.as_bytes() {
            hash ^= *b as u64;
            hash = hash.wrapping_mul(0x100000001b3);
        }
        let mut v = vec![0.0f64; 8];
        for (i, slot) in v.iter_mut().enumerate() {
            let byte = (hash >> (i * 8)) & 0xff;
            *slot = byte as f64 / 255.0;
        }
        Some(v)
    }
}

/// Scripted, observable embedding provider for scoring tests.
///
/// - `rules`: the first `(substring, vector)` whose substring occurs in the text
///   wins; otherwise `default` is returned.
/// - `fail`: every call returns `None` (provider error).
/// - `delay`: sleeps before answering each batch chunk (timeouts).
/// - `override_output`: replaces the whole batch result (invalid vectors, wrong
///   counts).
/// - Counters record every call so tests can prove "no embedding happened".
#[cfg(test)]
pub struct ScriptedEmbeddingProvider {
    pub rules: Vec<(String, Vec<f64>)>,
    pub default: Vec<f64>,
    pub fail: bool,
    pub delay: Option<Duration>,
    pub override_output: Option<Vec<Vec<f64>>>,
    pub embed_calls: std::sync::atomic::AtomicUsize,
    pub batch_calls: std::sync::atomic::AtomicUsize,
    pub chunks: std::sync::atomic::AtomicUsize,
    pub texts_seen: std::sync::Mutex<Vec<String>>,
    pub in_flight: std::sync::atomic::AtomicUsize,
    pub max_in_flight: std::sync::atomic::AtomicUsize,
    pub last_batch_size: std::sync::atomic::AtomicUsize,
}

#[cfg(test)]
impl ScriptedEmbeddingProvider {
    pub fn new(rules: Vec<(&str, Vec<f64>)>, default: Vec<f64>) -> Self {
        Self {
            rules: rules.into_iter().map(|(k, v)| (k.to_string(), v)).collect(),
            default,
            fail: false,
            delay: None,
            override_output: None,
            embed_calls: Default::default(),
            batch_calls: Default::default(),
            chunks: Default::default(),
            texts_seen: Default::default(),
            in_flight: Default::default(),
            max_in_flight: Default::default(),
            last_batch_size: Default::default(),
        }
    }

    /// Total number of calls of any kind (single or batch).
    pub fn total_calls(&self) -> usize {
        use std::sync::atomic::Ordering::SeqCst;
        self.embed_calls.load(SeqCst) + self.batch_calls.load(SeqCst)
    }

    fn vector_for(&self, text: &str) -> Vec<f64> {
        self.rules
            .iter()
            .find(|(k, _)| text.contains(k.as_str()))
            .map(|(_, v)| v.clone())
            .unwrap_or_else(|| self.default.clone())
    }
}

#[cfg(test)]
#[async_trait::async_trait]
impl EmbeddingProvider for ScriptedEmbeddingProvider {
    async fn embed(&self, _model: &str, text: &str) -> Option<Vec<f64>> {
        self.embed_calls
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.texts_seen.lock().unwrap().push(text.to_string());
        if self.fail {
            return None;
        }
        Some(self.vector_for(text))
    }

    async fn embed_batch(
        &self,
        _model: &str,
        texts: &[String],
        batch_size: usize,
        max_concurrency: usize,
    ) -> Option<Vec<Vec<f64>>> {
        use futures_util::stream::{self, StreamExt};
        use std::sync::atomic::Ordering::SeqCst;
        self.batch_calls.fetch_add(1, SeqCst);
        self.last_batch_size.store(batch_size, SeqCst);
        self.texts_seen
            .lock()
            .unwrap()
            .extend(texts.iter().cloned());

        let results: Vec<Vec<Vec<f64>>> = stream::iter(chunk_ranges(texts.len(), batch_size))
            .map(|(start, end)| async move {
                let chunk = &texts[start..end];
                self.chunks.fetch_add(1, SeqCst);
                let now = self.in_flight.fetch_add(1, SeqCst) + 1;
                self.max_in_flight.fetch_max(now, SeqCst);
                if let Some(d) = self.delay {
                    tokio::time::sleep(d).await;
                }
                self.in_flight.fetch_sub(1, SeqCst);
                chunk.iter().map(|t| self.vector_for(t)).collect()
            })
            .buffered(max_concurrency.max(1))
            .collect()
            .await;

        if self.fail {
            return None;
        }
        if let Some(o) = &self.override_output {
            return Some(o.clone());
        }
        Some(results.into_iter().flatten().collect())
    }
}

// ─── Reusable single-text embedding (used by the semantic cache) ──────────────

/// Compute the embedding of a single text using the configured Ollama provider.
///
/// Returns `None` on any failure (no Ollama provider configured, timeout, or error)
/// so callers can fail open. Reuses the same `/api/embed` → `/api/embeddings`
/// fallback path as the public embeddings endpoint.
pub async fn embed_single(state: &AppState, model: &str, text: &str) -> Option<Vec<f64>> {
    embed_single_with(
        &state.gateway_config.providers,
        state.http_client.as_ref(),
        model,
        text,
    )
    .await
}

/// Like [`embed_single`] but takes the provider list and HTTP client directly,
/// so it can be used by the injectable `OllamaEmbeddingProvider` without holding
/// a full `AppState`.
pub async fn embed_single_with(
    providers: &[crate::config::ProviderConfig],
    http_client: &dyn crate::client::LlmHttpClient,
    model: &str,
    text: &str,
) -> Option<Vec<f64>> {
    let provider_config = providers.iter().find(|p| p.provider_type == "ollama")?;

    let base_url = &provider_config.base_url;
    let timeout = provider_config.timeout();
    let inputs = vec![text.to_string()];

    let result = match call_ollama_embed(http_client, base_url, model, &inputs, timeout).await {
        Ok(embs) => Ok(embs),
        Err(EmbeddingProviderError::NotFound) => {
            call_ollama_embeddings_legacy(http_client, base_url, model, &inputs, timeout).await
        }
        Err(e) => Err(e),
    };

    match result {
        Ok(mut embs) if !embs.is_empty() && !embs[0].is_empty() => Some(embs.remove(0)),
        _ => None,
    }
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
    http_client: &dyn crate::client::LlmHttpClient,
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

    match http_client
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
    http_client: &dyn crate::client::LlmHttpClient,
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

        match http_client
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

    /// Simulated Ollama `/api/embed`: each input "t<N>" embeds to `[N, 1.0]`.
    struct EchoEmbed;
    impl wiremock::Respond for EchoEmbed {
        fn respond(&self, req: &wiremock::Request) -> wiremock::ResponseTemplate {
            let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
            let embs: Vec<Vec<f64>> = body["input"]
                .as_array()
                .unwrap()
                .iter()
                .map(|t| {
                    let n: f64 = t.as_str().unwrap()[1..].parse().unwrap();
                    vec![n, 1.0]
                })
                .collect();
            wiremock::ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"model": "m", "embeddings": embs}))
        }
    }

    fn ollama_provider(uri: &str) -> OllamaEmbeddingProvider {
        let cfg = crate::config::GatewayConfig::from_yaml(&format!(
            "server:\n  host: \"0.0.0.0\"\n  port: 8080\nredis:\n  cluster_urls: [\"redis://localhost:6379\"]\nproviders:\n  - id: ollama\n    provider_type: ollama\n    api_key: ollama\n    base_url: \"{uri}\"\n"
        ))
        .unwrap();
        OllamaEmbeddingProvider::new(
            cfg.providers,
            std::sync::Arc::new(crate::client::ReqwestLlmClient::new()),
        )
    }

    #[test]
    fn chunk_ranges_cover_all_items_in_order() {
        assert_eq!(chunk_ranges(5, 2), vec![(0, 2), (2, 4), (4, 5)]);
        assert_eq!(chunk_ranges(0, 3), vec![]);
        assert_eq!(chunk_ranges(3, 0), vec![(0, 1), (1, 2), (2, 3)]);
    }

    #[tokio::test]
    async fn ollama_embed_batch_chunks_and_preserves_order() {
        use wiremock::matchers::{method, path};
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(method("POST"))
            .and(path("/api/embed"))
            .respond_with(EchoEmbed)
            .mount(&server)
            .await;
        let p = ollama_provider(&server.uri());
        let texts: Vec<String> = (0..7).map(|i| format!("t{i}")).collect();
        let out = p.embed_batch("m", &texts, 3, 2).await.expect("batch ok");
        let firsts: Vec<f64> = out.iter().map(|v| v[0]).collect();
        assert_eq!(firsts, vec![0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0]);
        // 7 texts / batch 3 → 3 HTTP calls (not 7).
        assert_eq!(server.received_requests().await.unwrap().len(), 3);
    }

    #[tokio::test]
    async fn ollama_embed_batch_falls_back_to_legacy_endpoint_on_404() {
        use wiremock::matchers::{method, path};
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(method("POST"))
            .and(path("/api/embed"))
            .respond_with(wiremock::ResponseTemplate::new(404))
            .mount(&server)
            .await;
        wiremock::Mock::given(method("POST"))
            .and(path("/api/embeddings"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"embedding": [0.5, 0.5]})),
            )
            .mount(&server)
            .await;
        let p = ollama_provider(&server.uri());
        let texts = vec!["a".to_string(), "b".to_string()];
        let out = p.embed_batch("m", &texts, 8, 1).await.expect("legacy ok");
        assert_eq!(out.len(), 2);
    }

    #[tokio::test]
    async fn ollama_embed_batch_error_returns_none() {
        use wiremock::matchers::method;
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(method("POST"))
            .respond_with(wiremock::ResponseTemplate::new(500))
            .mount(&server)
            .await;
        let p = ollama_provider(&server.uri());
        assert!(p.embed_batch("m", &["x".to_string()], 4, 1).await.is_none());
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
