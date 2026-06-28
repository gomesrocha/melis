//! HTTP Router module.
//!
//! Defines the Axum router with all gateway endpoints and the
//! chat completions handler that integrates route configuration.
//!
//! Pipeline order:
//! 1. ConcurrencyLimit (max connections)
//! 2. Auth middleware (validates credentials, sets ClientIdentity)
//! 3. Rate Limit middleware (checks token bucket, 429 if exceeded)
//! 4. Handler: route resolve → compactor → load balancer → circuit breaker → transpiler → HTTP client

use axum::extract::DefaultBodyLimit;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::Sse;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::StreamExt;
use serde_json::Value;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use std::time::{Duration, Instant};

use crate::circuit_breaker::CallResult;
use crate::compactor::{SimpleCompactor, SemanticGuardedCompactor, ContextCompactor, CompactMessage};
use crate::config::CompactorConfig;
use crate::error::GatewayError;
use crate::middleware::auth::{auth_middleware, AuthValidator, ConfigAuthValidator};
use crate::middleware::rate_limiter::rate_limit_middleware;
use crate::observability::metrics_handler;
use crate::route_config::{RouteDefinition, WeightedProvider};
use crate::state::AppState;

/// Maximum payload size: 10MB.
const MAX_PAYLOAD_SIZE: usize = 10 * 1024 * 1024;

/// Resolved route information extracted from RouteConfigManager.
///
/// This struct captures the effective configuration for a matched route,
/// ready to be consumed by downstream pipeline stages (Compactor, LoadBalancer).
#[derive(Debug)]
pub struct ResolvedRoute {
    /// The effective model to use (from route override or original payload).
    pub model: String,
    /// Effective compactor configuration (per-route or global fallback).
    pub effective_token_config: CompactorConfig,
    /// Provider list with weights for the load balancer.
    /// If the route defines multi-provider (`providers` field), this contains them.
    /// If single `provider` is defined, this contains a single entry with weight 1.
    /// Empty if no provider info is available.
    pub providers: Vec<WeightedProvider>,
}

/// Builds the main application router with all endpoints.
///
/// Middleware pipeline (applied bottom-to-top per Tower convention):
/// 1. ConcurrencyLimitLayer — enforces max concurrent connections
/// 2. Auth middleware — validates Bearer token, sets ClientIdentity extension
/// 3. Rate Limit middleware — checks per-client token bucket
///
/// Auth and Rate Limit layers are only applied when `auth.enabled` is true
/// in the gateway configuration.
pub fn build_router(state: AppState) -> Router {
    let max_connections = state.gateway_config.server.max_concurrent_connections;

    let mut router = Router::new()
        .route("/metrics", get(metrics_handler))
        .route("/healthz", get(healthz_handler))
        .route("/readyz", get(readyz_handler));

    // Dynamically register routes from RouteConfigManager
    // This ensures all paths defined in routes.yaml are reachable
    {
        let resolver = state.route_config.current();
        let mut registered_paths = std::collections::HashSet::new();
        for route in &resolver.config().routes {
            if registered_paths.insert(route.path.clone()) {
                router = router.route(&route.path, post(chat_completions_handler));
            }
        }
        // Always ensure /v1/chat/completions is registered (default fallback)
        if !registered_paths.contains("/v1/chat/completions") {
            router = router.route("/v1/chat/completions", post(chat_completions_handler));
        }
    }

    // Apply auth + rate_limit middleware when auth is enabled
    if state.gateway_config.auth.enabled {
        let validator: Arc<dyn AuthValidator> =
            Arc::new(ConfigAuthValidator::new(state.gateway_config.auth.clone()));

        router = router
            // Rate limit runs after auth (applied first = runs second due to Tower ordering)
            .layer(axum::middleware::from_fn_with_state(
                state.clone(),
                rate_limit_middleware,
            ))
            // Auth runs before rate limit (applied second = runs first)
            .layer(axum::middleware::from_fn(auth_middleware))
            // Provide the AuthValidator as an extension for auth_middleware
            .layer(axum::Extension(validator));
    }

    router
        .layer(DefaultBodyLimit::max(MAX_PAYLOAD_SIZE))
        .layer(tower::limit::ConcurrencyLimitLayer::new(max_connections))
        .with_state(state)
}

/// Liveness probe handler - returns 200 if the process is responding.
async fn healthz_handler() -> &'static str {
    "OK"
}

/// Readiness probe handler - returns 200 if dependencies are available.
///
/// Checks the `redis_available` flag in application state.
/// Returns HTTP 200 with `{"status": "ready"}` when Redis is reachable,
/// or HTTP 503 with `{"status": "not_ready", "reason": "..."}` otherwise.
async fn readyz_handler(State(state): State<AppState>) -> Response {
    // TODO: Ping Redis connection pool instead of relying on a flag.
    let redis_ok = state.redis_available.load(Ordering::Relaxed);

    if redis_ok {
        (StatusCode::OK, Json(serde_json::json!({"status": "ready"}))).into_response()
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "status": "not_ready",
                "reason": "Redis connection unavailable"
            })),
        )
            .into_response()
    }
}

/// Validates the incoming JSON payload for the chat completions endpoint.
///
/// Checks:
/// - `model` field is present and is a non-empty string
/// - `messages` field is present and is a non-empty array
/// - Each message has non-empty `role` (string) and `content` (string)
fn validate_chat_payload(payload: &Value) -> Result<(), GatewayError> {
    // Validate model field
    match payload.get("model") {
        Some(Value::String(s)) if !s.is_empty() => {}
        Some(Value::String(_)) => {
            return Err(GatewayError::BadRequest(
                "Field 'model' must not be empty.".to_string(),
            ));
        }
        _ => {
            return Err(GatewayError::BadRequest(
                "Field 'model' is required and must be a non-empty string.".to_string(),
            ));
        }
    }

    // Validate messages field
    let messages = match payload.get("messages") {
        Some(Value::Array(arr)) if !arr.is_empty() => arr,
        Some(Value::Array(_)) => {
            return Err(GatewayError::BadRequest(
                "Field 'messages' must contain at least one message.".to_string(),
            ));
        }
        _ => {
            return Err(GatewayError::BadRequest(
                "Field 'messages' is required and must be a non-empty array.".to_string(),
            ));
        }
    };

    // Validate each message
    for (i, msg) in messages.iter().enumerate() {
        match msg.get("role") {
            Some(Value::String(s)) if !s.is_empty() => {}
            _ => {
                return Err(GatewayError::BadRequest(format!(
                    "Message at index {} must have a non-empty 'role' string field.",
                    i
                )));
            }
        }
        match msg.get("content") {
            Some(Value::String(s)) if !s.is_empty() => {}
            _ => {
                return Err(GatewayError::BadRequest(format!(
                    "Message at index {} must have a non-empty 'content' string field.",
                    i
                )));
            }
        }
    }

    Ok(())
}

/// Determines if the request should be served as SSE streaming.
///
/// Returns `true` if:
/// - The `Accept` header contains `text/event-stream`, OR
/// - The payload has `"stream": true`
fn is_streaming_request(headers: &HeaderMap, payload: &Value) -> bool {
    // Check Accept header for text/event-stream
    let accept_header_streaming = headers
        .get(axum::http::header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.contains("text/event-stream"))
        .unwrap_or(false);

    // Check payload for stream: true
    let payload_streaming = payload
        .get("stream")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    accept_header_streaming || payload_streaming
}

/// Chat completions handler.
///
/// This handler implements the full pipeline:
/// 1. Parse the incoming JSON payload
/// 2. Validate required fields (model, messages with role/content)
/// 3. Detect streaming mode (Accept: text/event-stream or stream: true)
/// 4. Resolve the route via RouteConfigManager (path + method matching)
/// 5. Apply model override if defined in the route
/// 6. Run context compaction on messages
/// 7. Select provider via load balancer
/// 8. Check circuit breaker availability
/// 9. Translate payload to provider format
/// 10. Forward request to provider
/// 11. Handle errors with circuit breaker integration
/// 12. Record metrics on all paths
#[tracing::instrument(skip(state, headers, payload, uri), fields(route = "/v1/chat/*"))]
async fn chat_completions_handler(
    State(state): State<AppState>,
    uri: axum::extract::OriginalUri,
    headers: HeaderMap,
    Json(mut payload): Json<Value>,
) -> Result<Response, GatewayError> {
    let request_start = Instant::now();
    let mut backend_elapsed_time: f64 = 0.0;

    // Validate payload structure
    validate_chat_payload(&payload)?;

    // Detect streaming mode before any further processing
    let streaming = is_streaming_request(&headers, &payload);

    // Extract model from validated payload (safe to unwrap after validation)
    let model_from_payload = payload["model"].as_str().unwrap().to_string();

    // Resolve route from RouteConfigManager using the actual request path
    let request_path = uri.path();
    let request_method = "POST";

    let resolver = state.route_config.current();
    let resolved = resolve_route_config(
        &resolver,
        request_path,
        request_method,
        &model_from_payload,
        &state.gateway_config.compactor,
    );

    // Apply model override by replacing in the JSON value
    let effective_model = resolved.model.clone();
    if let Some(obj) = payload.as_object_mut() {
        obj.insert(
            "model".to_string(),
            Value::String(effective_model.clone()),
        );
    }

    tracing::debug!(
        effective_model = %effective_model,
        providers_count = resolved.providers.len(),
        token_threshold = resolved.effective_token_config.token_threshold,
        streaming = streaming,
        "Route resolved for /v1/chat/completions"
    );

    // ─── Context Compaction ───────────────────────────────────────────────────
    let compaction_start = Instant::now();
    if let Some(messages_arr) = payload.get("messages").and_then(|m| m.as_array()) {
        let compact_messages: Vec<CompactMessage> = messages_arr.iter()
            .filter_map(|m| {
                let role = m.get("role")?.as_str()?;
                let content = m.get("content")?.as_str()?;
                Some(CompactMessage { role: role.to_string(), content: content.to_string() })
            })
            .collect();

        let compactor: Box<dyn ContextCompactor> = if resolved.effective_token_config.strategy == "semantic_guarded_trimming" {
            Box::new(SemanticGuardedCompactor::new())
        } else {
            Box::new(SimpleCompactor::new())
        };
        let result = compactor.compact(compact_messages, &resolved.effective_token_config);

        if result.was_compressed {
            let new_messages: Vec<Value> = result.messages.iter()
                .map(|m| serde_json::json!({"role": m.role, "content": m.content}))
                .collect();
            payload["messages"] = Value::Array(new_messages);
            state.metrics.compaction_applied_total.inc();
            state.metrics.context_original_tokens.inc_by(result.original_tokens as u64);
            state.metrics.context_final_tokens.inc_by(result.final_tokens as u64);
            state.metrics.context_saved_tokens_total.inc_by((result.original_tokens - result.final_tokens) as u64);
            state.metrics.compression_ratio.observe(result.compression_ratio);
        } else {
            // Still record token counts and ratio even when not compressing
            state.metrics.context_original_tokens.inc_by(result.original_tokens as u64);
            state.metrics.context_final_tokens.inc_by(result.final_tokens as u64);
            state.metrics.compression_ratio.observe(1.0);
            state.metrics.compaction_skipped_total.with_label_values(&["below_threshold"]).inc();
        }
    }
    let compaction_elapsed = compaction_start.elapsed().as_secs_f64();
    state.metrics.compaction_duration.observe(compaction_elapsed);

    // ─── Provider Selection & Failover Loop ──────────────────────────────────
    let mut excluded_providers: Vec<String> = Vec::new();
    let max_attempts = resolved.providers.len().min(3);
    let mut last_error: Option<GatewayError> = None;
    let mut provider_type_used = String::from("unknown");

    let response = 'failover: {
        for _attempt in 0..max_attempts {
            // Select provider, excluding previously failed ones
            let available: Vec<WeightedProvider> = resolved.providers.iter()
                .filter(|p| !excluded_providers.contains(&p.name))
                .cloned()
                .collect();

            if available.is_empty() {
                break 'failover Err(last_error.unwrap_or(
                    GatewayError::ServiceUnavailable("All providers exhausted".to_string())
                ));
            }

            let selected = match state.load_balancer.select_provider(&available) {
                Ok(p) => p,
                Err(e) => break 'failover Err(e),
            };
            let selected_name = selected.name.clone();

            // Check circuit breaker
            if !state.circuit_breaker.is_available(&selected_name).await {
                state.metrics.failover_total
                    .with_label_values(&[&selected_name, "circuit_open"])
                    .inc();
                state.metrics.fallback_mode_total
                    .with_label_values(&[&selected_name, "next", "circuit_open"])
                    .inc();
                excluded_providers.push(selected_name);
                continue;
            }

            // Resolve provider config
            let pconfig = state.gateway_config.providers.iter()
                .find(|p| p.id == selected_name || p.provider_type == selected_name);

            let base_url = pconfig
                .map(|p| p.base_url.clone())
                .unwrap_or_else(|| "http://localhost:11434".to_string());
            let api_key = pconfig
                .map(|p| p.api_key.clone())
                .unwrap_or_default();
            let ptype = pconfig
                .map(|p| p.provider_type.clone())
                .unwrap_or_else(|| "ollama".to_string());
            let timeout = pconfig
                .map(|p| p.timeout())
                .unwrap_or(Duration::from_secs(120));
            provider_type_used = ptype.clone();

            // Build forward URL
            let url = if ptype == "anthropic" {
                format!("{}/messages", base_url)
            } else if base_url.contains("/openai") || base_url.ends_with("/v1") {
                format!("{}/chat/completions", base_url)
            } else {
                format!("{}/v1/chat/completions", base_url)
            };

            // Build headers
            let mut provider_headers = reqwest::header::HeaderMap::new();
            provider_headers.insert(
                reqwest::header::CONTENT_TYPE,
                "application/json".parse().unwrap(),
            );
            if !api_key.is_empty() && api_key != "ollama" {
                if ptype == "anthropic" {
                    provider_headers.insert(
                        reqwest::header::HeaderName::from_static("x-api-key"),
                        api_key.parse().unwrap(),
                    );
                    provider_headers.insert(
                        reqwest::header::HeaderName::from_static("anthropic-version"),
                        "2023-06-01".parse().unwrap(),
                    );
                } else {
                    provider_headers.insert(
                        reqwest::header::AUTHORIZATION,
                        format!("Bearer {}", api_key).parse().unwrap(),
                    );
                }
            }

            // Translate payload
            let translation_start = Instant::now();
            let body = if ptype == "anthropic" {
                let transpiler = crate::transpiler::get_transpiler("anthropic");
                match transpiler.to_native(&payload) {
                    Ok(native) => bytes::Bytes::from(serde_json::to_vec(&native).unwrap()),
                    Err(e) => {
                        tracing::error!(error = %e, "Failed to translate payload to Anthropic format");
                        break 'failover Err(GatewayError::Internal(
                            "Payload translation failed".to_string(),
                        ));
                    }
                }
            } else {
                bytes::Bytes::from(serde_json::to_vec(&payload).unwrap())
            };
            let translation_elapsed = translation_start.elapsed().as_secs_f64();
            state.metrics.payload_translation.observe(translation_elapsed);

            // ─── Streaming: attempt once (can't retry mid-stream) ─────────────
            if streaming {
                let provider_start = Instant::now();
                let stream = state.http_client.send_stream(
                    &url,
                    provider_headers,
                    body,
                    timeout,
                );

                backend_elapsed_time = provider_start.elapsed().as_secs_f64();
                state.metrics.backend_latency
                    .with_label_values(&[&ptype])
                    .observe(backend_elapsed_time);

                state.circuit_breaker.record_result(
                    &selected_name,
                    CallResult::Success,
                ).await;

                let transpiler = crate::transpiler::get_transpiler(&ptype);
                let transformed = crate::streaming::transform_sse_stream(
                    Box::pin(stream.map(|r| r.map_err(|e| {
                        crate::streaming::StreamError::Connection(e.to_string())
                    }))),
                    transpiler,
                );

                break 'failover Ok(Sse::new(transformed).into_response());
            }

            // ─── Non-streaming: try and handle errors ─────────────────────────
            let provider_start = Instant::now();
            match state.http_client.send(&url, provider_headers, body, timeout).await {
                Ok(response_bytes) => {
                    backend_elapsed_time = provider_start.elapsed().as_secs_f64();
                    state.metrics.backend_latency
                        .with_label_values(&[&ptype])
                        .observe(backend_elapsed_time);

                    state.circuit_breaker.record_result(
                        &selected_name,
                        CallResult::Success,
                    ).await;

                    match serde_json::from_slice::<Value>(&response_bytes) {
                        Ok(json_response) => {
                            let final_response = if ptype == "anthropic" {
                                let transpiler = crate::transpiler::get_transpiler("anthropic");
                                match transpiler.from_native(&json_response) {
                                    Ok(openai_response) => openai_response,
                                    Err(e) => {
                                        tracing::error!(error = %e, "Failed to translate Anthropic response");
                                        json_response
                                    }
                                }
                            } else {
                                json_response
                            };

                            // Record token metrics
                            if let Some(usage) = final_response.get("usage") {
                                let prompt_tokens = usage.get("prompt_tokens")
                                    .and_then(|v| v.as_u64())
                                    .unwrap_or(0);
                                let completion_tokens = usage.get("completion_tokens")
                                    .and_then(|v| v.as_u64())
                                    .unwrap_or(0);
                                let model_name = final_response.get("model")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or(&effective_model);
                                state.metrics.llm_tokens_total
                                    .with_label_values(&["input", model_name, "unknown"])
                                    .inc_by(prompt_tokens);
                                state.metrics.llm_tokens_total
                                    .with_label_values(&["output", model_name, "unknown"])
                                    .inc_by(completion_tokens);
                            }

                            break 'failover Ok(Json(final_response).into_response());
                        }
                        Err(e) => {
                            tracing::error!(error = %e, "Failed to parse provider response");
                            break 'failover Err(GatewayError::ProviderError(
                                "Invalid response from provider".to_string(),
                            ));
                        }
                    }
                }
                Err(crate::client::ClientError::HttpError { status, body: err_body }) => {
                    backend_elapsed_time = provider_start.elapsed().as_secs_f64();
                    state.metrics.backend_latency
                        .with_label_values(&[&ptype])
                        .observe(backend_elapsed_time);
                    state.metrics.provider_errors_total
                        .with_label_values(&[&ptype, &status.to_string()])
                        .inc();

                    match status {
                        // Non-retryable errors — return immediately
                        400 => {
                            break 'failover Err(GatewayError::BadRequest(
                                format!("Provider error: {}", err_body),
                            ));
                        }
                        401 | 403 => {
                            break 'failover Err(GatewayError::Unauthorized(
                                format!("Provider auth error: {}", err_body),
                            ));
                        }
                        404 => {
                            break 'failover Err(GatewayError::BadRequest(
                                format!("Model '{}' not found at provider", effective_model),
                            ));
                        }
                        // Retryable — record failure and try next provider
                        429 => {
                            state.circuit_breaker.record_result(
                                &selected_name,
                                CallResult::RateLimited,
                            ).await;
                            state.metrics.failover_total
                                .with_label_values(&[&selected_name, "429"])
                                .inc();
                            excluded_providers.push(selected_name);
                            last_error = Some(GatewayError::RateLimited { retry_after_secs: 30 });
                        }
                        s if s >= 500 => {
                            state.circuit_breaker.record_result(
                                &selected_name,
                                CallResult::Failure,
                            ).await;
                            state.metrics.failover_total
                                .with_label_values(&[&selected_name, "5xx"])
                                .inc();
                            excluded_providers.push(selected_name);
                            last_error = Some(GatewayError::ServiceUnavailable(
                                format!("Provider error {}: {}", status, err_body),
                            ));
                        }
                        _ => {
                            excluded_providers.push(selected_name);
                            last_error = Some(GatewayError::ProviderError(
                                format!("HTTP {}: {}", status, err_body),
                            ));
                        }
                    }
                }
                Err(crate::client::ClientError::Timeout) => {
                    backend_elapsed_time = provider_start.elapsed().as_secs_f64();
                    state.metrics.backend_latency
                        .with_label_values(&[&ptype])
                        .observe(backend_elapsed_time);
                    state.circuit_breaker.record_result(
                        &selected_name,
                        CallResult::Timeout,
                    ).await;
                    state.metrics.provider_errors_total
                        .with_label_values(&[&ptype, "timeout"])
                        .inc();
                    state.metrics.failover_total
                        .with_label_values(&[&selected_name, "timeout"])
                        .inc();
                    excluded_providers.push(selected_name);
                    last_error = Some(GatewayError::ServiceUnavailable(
                        "Provider timeout".to_string(),
                    ));
                }
                Err(e) => {
                    backend_elapsed_time = provider_start.elapsed().as_secs_f64();
                    state.metrics.backend_latency
                        .with_label_values(&[&ptype])
                        .observe(backend_elapsed_time);
                    state.circuit_breaker.record_result(
                        &selected_name,
                        CallResult::Failure,
                    ).await;
                    state.metrics.provider_errors_total
                        .with_label_values(&[&ptype, "network_error"])
                        .inc();
                    state.metrics.failover_total
                        .with_label_values(&[&selected_name, "network"])
                        .inc();
                    excluded_providers.push(selected_name);
                    last_error = Some(GatewayError::ServiceUnavailable(
                        format!("Provider unavailable: {}", e),
                    ));
                }
            }
        }

        // All attempts exhausted
        Err(last_error.unwrap_or(
            GatewayError::ServiceUnavailable("All providers exhausted".to_string()),
        ))
    };

    // Record fallback success metric if we had failures but ultimately succeeded
    if !excluded_providers.is_empty() {
        if response.is_ok() {
            state.metrics.fallback_mode_total
                .with_label_values(&[
                    excluded_providers.first().map(|s| s.as_str()).unwrap_or("unknown"),
                    &provider_type_used,
                    "failover_success",
                ])
                .inc();
        }
    }

    let response = response?;

    // ─── Record Metrics ───────────────────────────────────────────────────────
    let total_elapsed = request_start.elapsed().as_secs_f64();
    let internal_overhead = total_elapsed - backend_elapsed_time;
    let status = response.status().as_u16().to_string();

    state.metrics.requests_total
        .with_label_values(&[request_path, "unknown", &status])
        .inc();
    state.metrics.request_duration
        .with_label_values(&[request_path, &provider_type_used])
        .observe(total_elapsed);
    state.metrics.internal_overhead.observe(internal_overhead);
    state.metrics.gateway_overhead.observe(total_elapsed);

    Ok(response)
}

/// Resolves route configuration and builds a `ResolvedRoute` with the effective
/// model, token config, and provider list.
///
/// If no route matches, falls back to the payload model and global config.
fn resolve_route_config(
    resolver: &crate::route_config::RouteResolver,
    path: &str,
    method: &str,
    payload_model: &str,
    global_compactor: &CompactorConfig,
) -> ResolvedRoute {
    match resolver.resolve_route(path, method) {
        Some(route) => {
            // Model override: use route.model if defined, otherwise keep payload model
            let effective_model = route
                .model
                .as_deref()
                .unwrap_or(payload_model)
                .to_string();

            // Effective token config: per-route if defined, otherwise global
            let effective_token_config =
                resolver.effective_token_config(route, global_compactor);

            // Provider list for load balancer
            let providers = extract_providers(route);

            ResolvedRoute {
                model: effective_model,
                effective_token_config,
                providers,
            }
        }
        None => {
            // No matching route — use defaults
            tracing::warn!(
                path = %path,
                method = %method,
                "No matching route found in route config, using defaults"
            );
            ResolvedRoute {
                model: payload_model.to_string(),
                effective_token_config: global_compactor.clone(),
                providers: Vec::new(),
            }
        }
    }
}

/// Extracts the provider list from a route definition.
///
/// - If `providers` (multi-provider) is defined, returns that list directly.
/// - If a single `provider` is defined, wraps it as a single-entry list with weight 1.
/// - Otherwise returns an empty list.
fn extract_providers(route: &RouteDefinition) -> Vec<WeightedProvider> {
    if let Some(ref providers) = route.providers {
        providers.clone()
    } else if let Some(ref provider_name) = route.provider {
        vec![WeightedProvider {
            name: provider_name.clone(),
            weight: 1,
            model: route.model.clone().unwrap_or_default(),
        }]
    } else {
        Vec::new()
    }
}

#[cfg(test)]
mod proptest_tests {
    use super::*;
    use proptest::prelude::*;

    /// **Validates: Requirements 1.5**
    ///
    /// Property 1: Validação de Payload Rejeita Entradas Inválidas
    /// For any payload missing valid `model` or `messages`, validate_chat_payload returns BadRequest.
    /// For valid payloads, it returns Ok.
    mod property_payload_validation {
        use super::*;

        /// Strategy to generate an invalid payload (missing or empty model/messages).
        fn invalid_payload_strategy() -> impl Strategy<Value = serde_json::Value> {
            prop_oneof![
                // Case 1: missing model entirely
                Just(serde_json::json!({
                    "messages": [{"role": "user", "content": "hello"}]
                })),
                // Case 2: model is empty string
                Just(serde_json::json!({
                    "model": "",
                    "messages": [{"role": "user", "content": "hello"}]
                })),
                // Case 3: model is not a string (number)
                Just(serde_json::json!({
                    "model": 123,
                    "messages": [{"role": "user", "content": "hello"}]
                })),
                // Case 4: missing messages entirely
                Just(serde_json::json!({
                    "model": "gpt-4"
                })),
                // Case 5: messages is empty array
                Just(serde_json::json!({
                    "model": "gpt-4",
                    "messages": []
                })),
                // Case 6: messages is not an array
                Just(serde_json::json!({
                    "model": "gpt-4",
                    "messages": "not-an-array"
                })),
                // Case 7: message missing role
                Just(serde_json::json!({
                    "model": "gpt-4",
                    "messages": [{"content": "hello"}]
                })),
                // Case 8: message with empty role
                Just(serde_json::json!({
                    "model": "gpt-4",
                    "messages": [{"role": "", "content": "hello"}]
                })),
                // Case 9: message missing content
                Just(serde_json::json!({
                    "model": "gpt-4",
                    "messages": [{"role": "user"}]
                })),
                // Case 10: message with empty content
                Just(serde_json::json!({
                    "model": "gpt-4",
                    "messages": [{"role": "user", "content": ""}]
                })),
            ]
        }

        /// Strategy to generate a valid payload with random non-empty model and messages.
        fn valid_payload_strategy() -> impl Strategy<Value = serde_json::Value> {
            let model_strategy = "[a-zA-Z][a-zA-Z0-9_-]{1,20}";
            let role_strategy = prop_oneof![
                Just("system".to_string()),
                Just("user".to_string()),
                Just("assistant".to_string()),
            ];
            let content_strategy = "[a-zA-Z0-9 ]{1,50}";

            (
                model_strategy,
                proptest::collection::vec(
                    (role_strategy, content_strategy),
                    1..5,
                ),
            )
                .prop_map(|(model, messages)| {
                    let msgs: Vec<serde_json::Value> = messages
                        .into_iter()
                        .map(|(role, content)| {
                            serde_json::json!({"role": role, "content": content})
                        })
                        .collect();
                    serde_json::json!({
                        "model": model,
                        "messages": msgs
                    })
                })
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(100))]

            #[test]
            fn invalid_payloads_return_bad_request(payload in invalid_payload_strategy()) {
                let result = validate_chat_payload(&payload);
                prop_assert!(result.is_err(), "Expected error for invalid payload: {:?}", payload);
                match result.unwrap_err() {
                    GatewayError::BadRequest(_) => {} // expected
                    other => prop_assert!(false, "Expected BadRequest, got: {:?}", other),
                }
            }

            #[test]
            fn valid_payloads_pass_validation(payload in valid_payload_strategy()) {
                let result = validate_chat_payload(&payload);
                prop_assert!(result.is_ok(), "Expected Ok for valid payload: {:?}, got: {:?}", payload, result);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::route_config::{RouteConfigFile, RouteResolver};
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    fn sample_config() -> RouteConfigFile {
        let yaml = r#"
routes:
  - path: "/v1/chat/completions"
    method: "POST"
    provider: "openai"
    model: "gpt-4o"
    token_optimization:
      strategy: "adaptive_trimming"
      max_history_messages: 10
      compress_above_tokens: 4000
      local_tokenizer: "cl100k_base"
  - path: "/v1/chat/raw"
    method: "POST"
    provider: "ollama"
  - path: "/v1/chat/multi"
    method: "POST"
    providers:
      - name: "openai"
        weight: 70
        model: "gpt-4o"
      - name: "anthropic"
        weight: 30
        model: "claude-sonnet-4-20250514"
"#;
        serde_yaml::from_str(yaml).unwrap()
    }

    fn global_compactor() -> CompactorConfig {
        CompactorConfig {
            token_threshold: 4096,
            max_history_messages: 20,
            stop_words: vec!["the".to_string(), "a".to_string()],
            tokenizer_name: "cl100k_base".to_string(),
            ..Default::default()
        }
    }

    // --- validate_chat_payload unit tests ---

    #[test]
    fn test_valid_payload_passes_validation() {
        let payload = serde_json::json!({
            "model": "gpt-4",
            "messages": [{"role": "user", "content": "Hello"}]
        });
        assert!(validate_chat_payload(&payload).is_ok());
    }

    #[test]
    fn test_missing_model_fails_validation() {
        let payload = serde_json::json!({
            "messages": [{"role": "user", "content": "Hello"}]
        });
        let err = validate_chat_payload(&payload).unwrap_err();
        assert!(matches!(err, GatewayError::BadRequest(_)));
    }

    #[test]
    fn test_empty_model_fails_validation() {
        let payload = serde_json::json!({
            "model": "",
            "messages": [{"role": "user", "content": "Hello"}]
        });
        let err = validate_chat_payload(&payload).unwrap_err();
        assert!(matches!(err, GatewayError::BadRequest(_)));
    }

    #[test]
    fn test_missing_messages_fails_validation() {
        let payload = serde_json::json!({
            "model": "gpt-4"
        });
        let err = validate_chat_payload(&payload).unwrap_err();
        assert!(matches!(err, GatewayError::BadRequest(_)));
    }

    #[test]
    fn test_empty_messages_array_fails_validation() {
        let payload = serde_json::json!({
            "model": "gpt-4",
            "messages": []
        });
        let err = validate_chat_payload(&payload).unwrap_err();
        assert!(matches!(err, GatewayError::BadRequest(_)));
    }

    #[test]
    fn test_message_missing_role_fails_validation() {
        let payload = serde_json::json!({
            "model": "gpt-4",
            "messages": [{"content": "Hello"}]
        });
        let err = validate_chat_payload(&payload).unwrap_err();
        assert!(matches!(err, GatewayError::BadRequest(_)));
    }

    #[test]
    fn test_message_empty_role_fails_validation() {
        let payload = serde_json::json!({
            "model": "gpt-4",
            "messages": [{"role": "", "content": "Hello"}]
        });
        let err = validate_chat_payload(&payload).unwrap_err();
        assert!(matches!(err, GatewayError::BadRequest(_)));
    }

    #[test]
    fn test_message_missing_content_fails_validation() {
        let payload = serde_json::json!({
            "model": "gpt-4",
            "messages": [{"role": "user"}]
        });
        let err = validate_chat_payload(&payload).unwrap_err();
        assert!(matches!(err, GatewayError::BadRequest(_)));
    }

    #[test]
    fn test_message_empty_content_fails_validation() {
        let payload = serde_json::json!({
            "model": "gpt-4",
            "messages": [{"role": "user", "content": ""}]
        });
        let err = validate_chat_payload(&payload).unwrap_err();
        assert!(matches!(err, GatewayError::BadRequest(_)));
    }

    #[test]
    fn test_multiple_valid_messages_pass_validation() {
        let payload = serde_json::json!({
            "model": "gpt-4",
            "messages": [
                {"role": "system", "content": "You are helpful"},
                {"role": "user", "content": "Hi"},
                {"role": "assistant", "content": "Hello!"}
            ]
        });
        assert!(validate_chat_payload(&payload).is_ok());
    }

    #[test]
    fn test_second_message_invalid_fails_validation() {
        let payload = serde_json::json!({
            "model": "gpt-4",
            "messages": [
                {"role": "user", "content": "Hello"},
                {"role": "", "content": "Bad message"}
            ]
        });
        let err = validate_chat_payload(&payload).unwrap_err();
        match err {
            GatewayError::BadRequest(msg) => assert!(msg.contains("index 1")),
            _ => panic!("Expected BadRequest"),
        }
    }

    // --- Route resolution tests ---

    #[test]
    fn test_resolve_route_applies_model_override() {
        let resolver = RouteResolver::new(sample_config());
        let global = global_compactor();

        let resolved = resolve_route_config(
            &resolver,
            "/v1/chat/completions",
            "POST",
            "gpt-3.5-turbo", // payload model
            &global,
        );

        // Route defines model: "gpt-4o" — should override payload model
        assert_eq!(resolved.model, "gpt-4o");
    }

    #[test]
    fn test_resolve_route_keeps_payload_model_when_no_override() {
        let resolver = RouteResolver::new(sample_config());
        let global = global_compactor();

        // /v1/chat/raw has no model defined in route
        let resolved = resolve_route_config(
            &resolver,
            "/v1/chat/raw",
            "POST",
            "llama3",
            &global,
        );

        // No route model override — should keep payload model
        assert_eq!(resolved.model, "llama3");
    }

    #[test]
    fn test_resolve_route_uses_per_route_token_config() {
        let resolver = RouteResolver::new(sample_config());
        let global = global_compactor();

        let resolved = resolve_route_config(
            &resolver,
            "/v1/chat/completions",
            "POST",
            "gpt-4o",
            &global,
        );

        // Route has token_optimization with compress_above_tokens: 4000
        assert_eq!(resolved.effective_token_config.token_threshold, 4000);
        assert_eq!(resolved.effective_token_config.tokenizer_name, "cl100k_base");
        // stop_words come from global
        assert_eq!(resolved.effective_token_config.stop_words, global.stop_words);
    }

    #[test]
    fn test_resolve_route_uses_global_token_config_when_no_per_route() {
        let resolver = RouteResolver::new(sample_config());
        let global = global_compactor();

        // /v1/chat/raw has no token_optimization
        let resolved = resolve_route_config(
            &resolver,
            "/v1/chat/raw",
            "POST",
            "llama3",
            &global,
        );

        assert_eq!(resolved.effective_token_config, global);
    }

    #[test]
    fn test_resolve_route_extracts_multi_providers() {
        let resolver = RouteResolver::new(sample_config());
        let global = global_compactor();

        let resolved = resolve_route_config(
            &resolver,
            "/v1/chat/multi",
            "POST",
            "gpt-4o",
            &global,
        );

        assert_eq!(resolved.providers.len(), 2);
        assert_eq!(resolved.providers[0].name, "openai");
        assert_eq!(resolved.providers[0].weight, 70);
        assert_eq!(resolved.providers[1].name, "anthropic");
        assert_eq!(resolved.providers[1].weight, 30);
    }

    #[test]
    fn test_resolve_route_wraps_single_provider() {
        let resolver = RouteResolver::new(sample_config());
        let global = global_compactor();

        let resolved = resolve_route_config(
            &resolver,
            "/v1/chat/completions",
            "POST",
            "gpt-4o",
            &global,
        );

        // Single provider "openai" should be wrapped as a 1-element list
        assert_eq!(resolved.providers.len(), 1);
        assert_eq!(resolved.providers[0].name, "openai");
        assert_eq!(resolved.providers[0].weight, 1);
        assert_eq!(resolved.providers[0].model, "gpt-4o");
    }

    #[test]
    fn test_resolve_route_no_match_uses_defaults() {
        let resolver = RouteResolver::new(sample_config());
        let global = global_compactor();

        let resolved = resolve_route_config(
            &resolver,
            "/v1/chat/unknown",
            "POST",
            "my-model",
            &global,
        );

        // No match — falls back to payload model and global config
        assert_eq!(resolved.model, "my-model");
        assert_eq!(resolved.effective_token_config, global);
        assert!(resolved.providers.is_empty());
    }

    // --- Streaming detection unit tests ---

    #[test]
    fn test_is_streaming_with_accept_header() {
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::ACCEPT,
            "text/event-stream".parse().unwrap(),
        );
        let payload = serde_json::json!({"model": "gpt-4", "messages": []});
        assert!(is_streaming_request(&headers, &payload));
    }

    #[test]
    fn test_is_streaming_with_stream_true_in_payload() {
        let headers = HeaderMap::new();
        let payload = serde_json::json!({"model": "gpt-4", "messages": [], "stream": true});
        assert!(is_streaming_request(&headers, &payload));
    }

    #[test]
    fn test_is_not_streaming_without_accept_or_stream() {
        let headers = HeaderMap::new();
        let payload = serde_json::json!({"model": "gpt-4", "messages": []});
        assert!(!is_streaming_request(&headers, &payload));
    }

    #[test]
    fn test_is_not_streaming_with_json_accept() {
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::ACCEPT,
            "application/json".parse().unwrap(),
        );
        let payload = serde_json::json!({"model": "gpt-4", "messages": []});
        assert!(!is_streaming_request(&headers, &payload));
    }

    #[test]
    fn test_is_not_streaming_with_stream_false() {
        let headers = HeaderMap::new();
        let payload = serde_json::json!({"model": "gpt-4", "messages": [], "stream": false});
        assert!(!is_streaming_request(&headers, &payload));
    }

    #[test]
    fn test_is_streaming_accept_header_with_multiple_types() {
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::ACCEPT,
            "text/event-stream, application/json".parse().unwrap(),
        );
        let payload = serde_json::json!({"model": "gpt-4", "messages": []});
        assert!(is_streaming_request(&headers, &payload));
    }

    // --- Integration tests for streaming vs JSON response ---

    fn build_test_app() -> Router {
        use crate::balancer::WeightedRoundRobin;
        use crate::circuit_breaker::LocalCircuitBreaker;
        use crate::client::ReqwestLlmClient;
        use crate::config::GatewayConfig;
        use crate::middleware::rate_limiter::LocalTokenBucket;
        use crate::observability::Metrics;
        use crate::route_config::RouteConfigManager;
        use std::sync::atomic::AtomicBool;
        use std::sync::Arc;

        let gateway_config = GatewayConfig::from_yaml(
            r#"
server:
  host: "0.0.0.0"
  port: 8080
redis:
  cluster_urls:
    - "redis://localhost:6379"
routes_config_path: "./routes.yaml"
"#,
        )
        .unwrap();

        let route_config = RouteConfigManager::new(gateway_config.routes_config_path.clone());

        let app_state = AppState {
            route_config: Arc::new(route_config),
            gateway_config: Arc::new(gateway_config),
            rate_limiter: Arc::new(LocalTokenBucket::new()),
            load_balancer: Arc::new(WeightedRoundRobin::new()),
            circuit_breaker: Arc::new(LocalCircuitBreaker::new(Default::default())),
            http_client: Arc::new(ReqwestLlmClient::new()),
            metrics: Arc::new(Metrics::new()),
            redis_available: Arc::new(AtomicBool::new(true)),
        };

        build_router(app_state)
    }

    #[tokio::test]
    async fn test_handler_returns_json_without_accept_stream() {
        let app = build_test_app();

        let body = serde_json::json!({
            "model": "gpt-4",
            "messages": [{"role": "user", "content": "Hello"}]
        });

        let request = Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let content_type = response
            .headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap();
        assert!(
            content_type.contains("application/json"),
            "Expected application/json, got: {}",
            content_type
        );

        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Value = serde_json::from_slice(&body_bytes).unwrap();
        assert_eq!(json["object"], "chat.completion");
    }

    #[tokio::test]
    async fn test_handler_returns_sse_with_accept_event_stream() {
        let app = build_test_app();

        let body = serde_json::json!({
            "model": "gpt-4",
            "messages": [{"role": "user", "content": "Hello"}]
        });

        let request = Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header("content-type", "application/json")
            .header("accept", "text/event-stream")
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let content_type = response
            .headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap();
        assert!(
            content_type.contains("text/event-stream"),
            "Expected text/event-stream, got: {}",
            content_type
        );

        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body_str = String::from_utf8(body_bytes.to_vec()).unwrap();
        // SSE format: lines starting with "data:"
        assert!(
            body_str.contains("data:"),
            "Expected SSE data lines, got: {}",
            body_str
        );
        assert!(
            body_str.contains("[DONE]"),
            "Expected [DONE] terminator in SSE stream"
        );
    }

    #[tokio::test]
    async fn test_handler_returns_sse_with_stream_true_in_payload() {
        let app = build_test_app();

        let body = serde_json::json!({
            "model": "gpt-4",
            "messages": [{"role": "user", "content": "Hello"}],
            "stream": true
        });

        let request = Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let content_type = response
            .headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap();
        assert!(
            content_type.contains("text/event-stream"),
            "Expected text/event-stream, got: {}",
            content_type
        );
    }

    // --- Health check endpoint tests ---

    #[tokio::test]
    async fn test_healthz_returns_200_ok() {
        let app = build_test_app();

        let request = Request::builder()
            .method("GET")
            .uri("/healthz")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body_str = String::from_utf8(body_bytes.to_vec()).unwrap();
        assert_eq!(body_str, "OK");
    }

    #[tokio::test]
    async fn test_readyz_returns_200_when_redis_available() {
        let app = build_test_app();

        let request = Request::builder()
            .method("GET")
            .uri("/readyz")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Value = serde_json::from_slice(&body_bytes).unwrap();
        assert_eq!(json["status"], "ready");
    }

    #[tokio::test]
    async fn test_readyz_returns_503_when_redis_unavailable() {
        use crate::balancer::WeightedRoundRobin;
        use crate::circuit_breaker::LocalCircuitBreaker;
        use crate::client::ReqwestLlmClient;
        use crate::config::GatewayConfig;
        use crate::middleware::rate_limiter::LocalTokenBucket;
        use crate::observability::Metrics;
        use crate::route_config::RouteConfigManager;
        use std::sync::atomic::AtomicBool;
        use std::sync::Arc;

        let gateway_config = GatewayConfig::from_yaml(
            r#"
server:
  host: "0.0.0.0"
  port: 8080
redis:
  cluster_urls:
    - "redis://localhost:6379"
routes_config_path: "./routes.yaml"
"#,
        )
        .unwrap();

        let route_config = RouteConfigManager::new(gateway_config.routes_config_path.clone());

        let app_state = AppState {
            route_config: Arc::new(route_config),
            gateway_config: Arc::new(gateway_config),
            rate_limiter: Arc::new(LocalTokenBucket::new()),
            load_balancer: Arc::new(WeightedRoundRobin::new()),
            circuit_breaker: Arc::new(LocalCircuitBreaker::new(Default::default())),
            http_client: Arc::new(ReqwestLlmClient::new()),
            metrics: Arc::new(Metrics::new()),
            redis_available: Arc::new(AtomicBool::new(false)), // Redis unavailable
        };

        let app = build_router(app_state);

        let request = Request::builder()
            .method("GET")
            .uri("/readyz")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Value = serde_json::from_slice(&body_bytes).unwrap();
        assert_eq!(json["status"], "not_ready");
        assert_eq!(json["reason"], "Redis connection unavailable");
    }
}
