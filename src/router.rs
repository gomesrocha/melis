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

use crate::config::CompactorConfig;
use crate::embeddings::embeddings_handler;
use crate::error::GatewayError;
use crate::middleware::auth::{
    auth_middleware, AuthValidator, ClientIdentity, ConfigAuthValidator,
};
use crate::middleware::rate_limiter::rate_limit_middleware;
use crate::observability::metrics_handler;
use crate::route_config::{RouteDefinition, WeightedProvider};
use crate::state::AppState;
use crate::tokenizer::TokenCounter;

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
    /// The model the client originally requested (before alias/override), for
    /// authorization and observability. Never used as a label directly.
    pub requested_model: String,
    /// Canonical alias name when the request resolved through an alias (Phase 6A).
    pub via_alias: Option<String>,
    /// True when the resolved alias has more than one target (cache is bypassed).
    pub alias_multi_target: bool,
    /// Set when resolution failed in a client-visible way (e.g. `@unknown-alias`).
    /// The handler turns this into a 400 before contacting any provider.
    pub resolution_error: Option<String>,
    /// Effective compactor configuration (per-route or global fallback).
    pub effective_token_config: CompactorConfig,
    /// Provider list with weights for the load balancer.
    /// If an alias resolved, this holds the alias targets. Otherwise, if the route
    /// defines multi-provider (`providers`), this holds them; a single `provider`
    /// becomes a one-entry list. Empty if no provider info is available.
    pub providers: Vec<WeightedProvider>,
    /// Effective semantic cache configuration for the route (Phase 2).
    /// `None` when the route does not opt in (cache disabled).
    pub semantic_cache: Option<crate::route_config::SemanticCacheConfig>,
    /// Phase 5 opt-in scoring. `None` = legacy compaction (no embedding calls).
    pub semantic_scoring: Option<crate::semantic_scoring::ScoringConfig>,
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
        .route("/readyz", get(readyz_handler))
        .route("/v1/models", get(models_handler))
        .route("/v1/embeddings", post(embeddings_handler));

    // Dynamically register routes from RouteConfigManager
    // This ensures all paths defined in routes.yaml are reachable
    {
        let resolver = state.route_config.current();
        let mut registered_paths = std::collections::HashSet::new();
        // /v1/embeddings is already registered above with its own handler
        registered_paths.insert("/v1/embeddings".to_string());
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
    // `redis_available` is refreshed by a periodic PING (see main.rs).
    let redis_ok = state.redis_available.load(Ordering::Relaxed);
    let required = state.gateway_config.redis.required_for_readiness;

    match (redis_ok, required) {
        (true, _) => (StatusCode::OK, Json(serde_json::json!({"status": "ready"}))).into_response(),
        // Redis is a required dependency: take this replica out of rotation.
        (false, true) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "status": "not_ready",
                "reason": "Redis connection unavailable"
            })),
        )
            .into_response(),
        // Redis is optional: Redis-backed features degrade, the replica keeps serving.
        (false, false) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "status": "ready",
                "degraded": ["redis"]
            })),
        )
            .into_response(),
    }
}

/// `GET /v1/models` — OpenAI-compatible model listing (Phase 6A).
///
/// Lists declared model aliases and declared concrete models (no remote
/// discovery). Respects auth: when `auth.enabled`, the request must be
/// authenticated (the auth layer rejects it otherwise), and if the client's
/// `allowed_models` is non-empty the list is filtered to permitted ids. Never
/// reveals credentials, provider endpoints, weights, or the alias mapping.
async fn models_handler(
    State(state): State<AppState>,
    identity: Option<axum::Extension<ClientIdentity>>,
) -> Response {
    let resolver = state.route_config.current();
    let mut ids: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for a in resolver.alias_names() {
        ids.insert(a);
    }
    for m in resolver.concrete_model_names() {
        if !m.is_empty() {
            ids.insert(m);
        }
    }

    // Filter by allowed_models when the client is restricted.
    if let Some(axum::Extension(id)) = &identity {
        if !id.allowed_models.is_empty() {
            let allowed: std::collections::HashSet<&str> =
                id.allowed_models.iter().map(String::as_str).collect();
            ids.retain(|m| allowed.contains(m.as_str()));
        }
    }

    let data: Vec<Value> = ids
        .into_iter()
        .map(|id| {
            serde_json::json!({
                "id": id,
                "object": "model",
                "created": 0,
                "owned_by": "melis",
            })
        })
        .collect();

    Json(serde_json::json!({ "object": "list", "data": data })).into_response()
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
    // Present only when auth is enabled and succeeded (set by auth_middleware).
    // Used solely to scope the semantic cache by caller identity; never logged raw.
    identity: Option<axum::Extension<ClientIdentity>>,
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
    let mut resolved = resolve_route_config(
        &resolver,
        request_path,
        request_method,
        &model_from_payload,
        &state.gateway_config.compactor,
    );

    // Alias resolution error (e.g. `@unknown`): 400 before any provider call.
    if let Some(reason) = resolved.resolution_error.clone() {
        state
            .metrics
            .requests_total
            .with_label_values(&[request_path, "unknown", "400"])
            .inc();
        record_alias_resolution(&state.metrics, &resolved, "unknown_alias");
        return Err(GatewayError::BadRequest(reason));
    }

    // Apply model override by replacing in the JSON value
    let effective_model = resolved.model.clone();
    if let Some(obj) = payload.as_object_mut() {
        obj.insert("model".to_string(), Value::String(effective_model.clone()));
    }

    // ─── Model authorization (allowed_models) ─────────────────────────────────
    // A client's `allowed_models` restricts which models it may use. An empty list
    // means unrestricted (preserves the previous behavior, where this field was
    // carried but not enforced). The model the client REQUESTED, the effective
    // model, AND every resolved alias destination must be permitted — so neither a
    // route override nor an alias can route a client to a model it isn't allowed to
    // use (an alias must not become a way to bypass a model restriction).
    if let Some(axum::Extension(id)) = &identity {
        let mut to_check: Vec<&String> = vec![&model_from_payload, &effective_model];
        for p in &resolved.providers {
            to_check.push(&p.model);
        }
        if let Err(denied) = authorize_models(&id.allowed_models, to_check) {
            tracing::warn!(
                client = %id.client_id,
                route = %request_path,
                "model not permitted for client"
            );
            state
                .metrics
                .requests_total
                .with_label_values(&[request_path, "unknown", "403"])
                .inc();
            return Err(GatewayError::Forbidden(denied));
        }
    }

    // Alias-resolution observability (requested vs resolved vs effective). Labels
    // are bounded (declared alias name or `other`); the requested string is logged,
    // not used as a label.
    let alias_outcome = match (&resolved.via_alias, resolved.alias_multi_target) {
        (Some(_), true) => "resolved_multi",
        (Some(_), false) => "resolved_single",
        (None, _) => "no_alias",
    };
    record_alias_resolution(&state.metrics, &resolved, alias_outcome);

    tracing::debug!(
        requested_model = %resolved.requested_model,
        effective_model = %effective_model,
        via_alias = resolved.via_alias.as_deref().unwrap_or("none"),
        alias_multi_target = resolved.alias_multi_target,
        providers_count = resolved.providers.len(),
        token_threshold = resolved.effective_token_config.token_threshold,
        streaming = streaming,
        "Route resolved for /v1/chat/completions"
    );

    // Resolve the route's compiled guardrails ONCE per request and reuse the same
    // snapshot for the inbound check and every outbound check. The guardrail engine
    // is a separate ArcSwap from the route resolver, so reading it repeatedly could
    // otherwise observe different versions mid-request across a hot reload (audit:
    // per-request configuration consistency).
    let request_guardrails = state.guardrails.for_route(request_path, request_method);
    if let Some(g) = &request_guardrails {
        // Referenced through the GuardrailPolicy contract (Phase 6B): logs the
        // active policy and keeps the trait a live part of the request path.
        let policy: &dyn crate::guardrails::GuardrailPolicy = g.as_ref();
        tracing::debug!(
            guardrail_policy = policy.name(),
            fail_closed = policy.fail_closed(),
            "route guardrail policy resolved"
        );
    }

    // ─── Inbound Guardrails (Phase 3) ─────────────────────────────────────────
    // Runs AFTER auth + rate limit (Tower layers) and BEFORE cache lookup/store
    // and compaction. A block rejects with 400 (never touching cache or provider);
    // a redact rewrites the payload so the redacted content is what flows into the
    // cache key, compaction, and the provider.
    if let Some(guardrails) = request_guardrails.clone() {
        use crate::guardrails::GuardrailDecision;
        use crate::route_config::GuardrailDirection;

        if let Some(messages) = payload.get("messages").and_then(|m| m.as_array()).cloned() {
            let mut rewritten: Vec<Value> = Vec::with_capacity(messages.len());
            let mut any_redaction = false;

            for msg in messages {
                let content = msg.get("content").and_then(|c| c.as_str());
                match content {
                    Some(text) => {
                        match guardrails.evaluate(GuardrailDirection::Inbound, text, &state.metrics)
                        {
                            GuardrailDecision::Blocked { rule } => {
                                tracing::warn!(
                                    rule = %rule,
                                    route = %request_path,
                                    "inbound guardrail blocked request; not forwarding to provider or cache"
                                );
                                state
                                    .metrics
                                    .requests_total
                                    .with_label_values(&[request_path, "unknown", "400"])
                                    .inc();
                                return Err(GatewayError::BadRequest(format!(
                                    "Request blocked by guardrail rule '{}'",
                                    rule
                                )));
                            }
                            GuardrailDecision::Redacted(new_text) => {
                                any_redaction = true;
                                let mut m = msg.clone();
                                if let Some(obj) = m.as_object_mut() {
                                    obj.insert("content".to_string(), Value::String(new_text));
                                }
                                rewritten.push(m);
                            }
                            GuardrailDecision::Allow => rewritten.push(msg),
                        }
                    }
                    None => rewritten.push(msg),
                }
            }

            if any_redaction {
                payload["messages"] = Value::Array(rewritten);
            }
        }
    }

    // ─── Semantic Cache Lookup (Phase 2) ──────────────────────────────────────
    // Only when the route opts in. The normalized conversation embedding and a
    // stable entry id are computed up front and reused for store-on-miss.
    let mut cache_ctx: Option<crate::semantic_cache::CacheKeyCtx> = None;
    let mut cache_entry_id: Option<String> = None;
    let mut cache_embedding: Option<Vec<f64>> = None;
    // The destination (provider id, concrete model) the cache ctx/key was built
    // for. Lookup targets this deterministic primary destination; store must only
    // happen when the response actually came from it, so a failover response is
    // never stored under an incompatible destination's namespace.
    let mut cache_primary_dest: Option<(String, String)> = None;

    // Resolve the cache reuse scope (security isolation). `per_identity` (default)
    // keys the cache by the authenticated client identity; a request with no
    // trusted identity does NOT consult or populate the cache (no shared bucket for
    // unknown callers). `shared` reuses across callers and ignores identity.
    let cache_identity: Option<crate::semantic_cache::CacheIdentity> =
        match &resolved.semantic_cache {
            None => None,
            Some(sc) => match sc.scope {
                crate::route_config::CacheScope::Shared => {
                    Some(crate::semantic_cache::CacheIdentity::Shared)
                }
                crate::route_config::CacheScope::PerIdentity => match &identity {
                    Some(axum::Extension(id)) if !id.client_id.is_empty() => Some(
                        crate::semantic_cache::CacheIdentity::Identity(id.client_id.clone()),
                    ),
                    // per_identity but no trusted identity → bypass the cache entirely.
                    _ => None,
                },
            },
        };

    // Multi-target aliases are non-deterministic for the cache (the destination is
    // chosen after lookup and can fall back): bypass lookup AND store this version.
    let cache_bypass_multi_target =
        resolved.semantic_cache.is_some() && resolved.alias_multi_target;
    if cache_bypass_multi_target {
        state
            .metrics
            .cache_skipped_total
            .with_label_values(&["multi_target_alias"])
            .inc();
    }

    if let (Some(sc), Some(cache_identity), false) = (
        resolved.semantic_cache.clone(),
        cache_identity.clone(),
        cache_bypass_multi_target,
    ) {
        let messages = payload
            .get("messages")
            .and_then(|m| m.as_array())
            .cloned()
            .unwrap_or_default();
        let normalized = crate::semantic_cache::normalize_conversation(&messages);
        // Effective destination for the key: the single resolved provider+model
        // (single-target alias or normal route). This distinguishes provider/
        // deployment and concrete model, so a remapped alias or different deployment
        // never reuses an incompatible entry.
        let destination = {
            let (prov, model) = resolved
                .providers
                .first()
                .map(|p| (p.name.clone(), p.model.clone()))
                .unwrap_or_else(|| (String::new(), effective_model.clone()));
            let base_url = state
                .gateway_config
                .providers
                .iter()
                .find(|p| p.id == prov || p.provider_type == prov)
                .map(|p| p.base_url.clone())
                .unwrap_or_default();
            let model = if model.is_empty() {
                effective_model.clone()
            } else {
                model
            };
            crate::semantic_cache::CacheDestination::new(&prov, &base_url, &model)
        };
        // Fold caller identity, effective destination, system instructions,
        // generation parameters, and tool definitions into the key: the embedding
        // only covers non-system content, so without this a different identity /
        // destination / system prompt / temperature / tool set would share a
        // namespace and could return an incompatible cached response.
        let key_salt = crate::semantic_cache::compatibility_salt(
            &messages,
            &payload,
            &cache_identity,
            &destination,
        );
        let ctx = crate::semantic_cache::CacheKeyCtx {
            route_path: request_path.to_string(),
            route_method: request_method.to_string(),
            model: effective_model.clone(),
            key_salt,
            threshold: sc.similarity_threshold,
            ttl_secs: sc.ttl_secs,
            max_entries: sc.max_entries,
        };

        let lookup_start = Instant::now();
        // Compute the embedding via the injectable embeddings backend (fail-open).
        let embed_start = Instant::now();
        let embedding_opt = state
            .embeddings
            .embed(&sc.embedding_model, &normalized)
            .await;
        state
            .metrics
            .cache_embed_seconds
            .observe(embed_start.elapsed().as_secs_f64());
        match embedding_opt {
            Some(embedding) => {
                let outcome = state.semantic_cache.lookup(&ctx, &embedding).await;
                state
                    .metrics
                    .cache_lookup_seconds
                    .observe(lookup_start.elapsed().as_secs_f64());
                match outcome {
                    crate::semantic_cache::CacheLookup::Hit(cached) => {
                        state.metrics.cache_hits_total.inc();
                        let age =
                            crate::semantic_cache::now_unix().saturating_sub(cached.stored_at_unix);
                        state.metrics.cache_entry_age_seconds.observe(age as f64);
                        tracing::info!(
                            route = %request_path,
                            model = %effective_model,
                            "semantic cache hit"
                        );
                        // Phase 2 reserved hook (kept) + Phase 3 real outbound
                        // guardrails: a cache hit must pass through output filtering
                        // exactly like a provider response.
                        let mut body = state.outbound_guardrail.apply(cached.body);
                        if let Some(guardrails) = request_guardrails.clone() {
                            if let Err(rule) = crate::guardrails::apply_outbound_to_response(
                                &mut body,
                                guardrails.as_ref(),
                                &state.metrics,
                            ) {
                                tracing::warn!(
                                    rule = %rule,
                                    route = %request_path,
                                    "outbound guardrail blocked cached response"
                                );
                                state
                                    .metrics
                                    .requests_total
                                    .with_label_values(&[request_path, "unknown", "403"])
                                    .inc();
                                return Err(GatewayError::ContentBlocked(format!(
                                    "Response blocked by guardrail rule '{}'",
                                    rule
                                )));
                            }
                        }
                        state
                            .metrics
                            .requests_total
                            .with_label_values(&[request_path, "unknown", "200"])
                            .inc();
                        if streaming || cached.was_streaming {
                            return Ok(cache_hit_sse_response(&body));
                        }
                        return Ok(Json(body).into_response());
                    }
                    crate::semantic_cache::CacheLookup::Miss => {
                        state.metrics.cache_misses_total.inc();
                        cache_embedding = Some(embedding);
                    }
                    crate::semantic_cache::CacheLookup::Degraded(reason) => {
                        state
                            .metrics
                            .cache_degraded
                            .with_label_values(&[reason.label()])
                            .inc();
                        tracing::warn!(
                            reason = reason.label(),
                            "semantic cache degraded; fail-open"
                        );
                    }
                }
            }
            None => {
                state
                    .metrics
                    .cache_degraded
                    .with_label_values(&[crate::semantic_cache::DegradeReason::Embeddings.label()])
                    .inc();
                tracing::warn!("semantic cache: embeddings unavailable; fail-open");
            }
        }

        cache_entry_id = Some(crate::semantic_cache::entry_id_for(&normalized));
        cache_ctx = Some(ctx);
        cache_primary_dest = resolved
            .providers
            .first()
            .map(|p| (p.name.clone(), p.model.clone()));
    }

    // ─── Context Compaction ───────────────────────────────────────────────────
    // Resolve the token counter for this request from the effective route config.
    // Used both for compaction counting and for synthesizing `usage` when a provider
    // omits it. Real BPE encodings are cached singletons (cheap to resolve).
    let token_counter = TokenCounter::resolve(
        resolved.effective_token_config.token_counting,
        &resolved.effective_token_config.tokenizer_name,
    );
    state
        .metrics
        .token_counting_mode
        .with_label_values(&[token_counter.mode_label()])
        .inc();

    let compaction_start = Instant::now();
    if let Some(messages_arr) = payload.get("messages").and_then(|m| m.as_array()) {
        // Select the context policy for this request (legacy vs scored), exactly as
        // before. The policy only decides selection/ranking and returns a
        // JsonCompaction referencing the original messages by index; the host below
        // keeps applying/validating the result (preservation, budget metrics, 400).
        let policy = crate::context_policy::select_policy(&resolved.semantic_scoring);
        tracing::debug!(context_policy = policy.name(), "applying context policy");
        let compaction = match policy
            .compact(
                messages_arr,
                &resolved.effective_token_config,
                state.embeddings.as_ref(),
            )
            .await
        {
            Ok(outcome) => {
                if let Some(scoring) = &outcome.scoring {
                    record_scoring_outcome(&state.metrics, scoring);
                }
                outcome.compaction
            }
            Err(crate::context_policy::ContextError::BudgetInsufficient(insufficient)) => {
                state.metrics.compaction_budget_insufficient_total.inc();
                tracing::warn!(
                    required_tokens = insufficient.required_tokens,
                    overhead_tokens = insufficient.overhead_tokens,
                    output_reserve_tokens = insufficient.output_reserve_tokens,
                    max_context_tokens = insufficient.max_context_tokens,
                    "strict context budget insufficient; request rejected"
                );
                return Err(GatewayError::BadRequest(insufficient.message()));
            }
        };
        let result = compaction.result;

        if result.was_compressed {
            payload["messages"] = Value::Array(compaction.messages);
            state.metrics.compaction_applied_total.inc();
            state
                .metrics
                .context_original_tokens
                .inc_by(result.original_tokens as u64);
            state
                .metrics
                .context_final_tokens
                .inc_by(result.final_tokens as u64);
            state
                .metrics
                .context_saved_tokens_total
                .inc_by((result.original_tokens - result.final_tokens) as u64);
            state
                .metrics
                .compression_ratio
                .observe(result.compression_ratio);
        } else {
            // Still record token counts and ratio even when not compressing
            state
                .metrics
                .context_original_tokens
                .inc_by(result.original_tokens as u64);
            state
                .metrics
                .context_final_tokens
                .inc_by(result.final_tokens as u64);
            state.metrics.compression_ratio.observe(1.0);
            state
                .metrics
                .compaction_skipped_total
                .with_label_values(&["below_threshold"])
                .inc();
        }
    }
    let compaction_elapsed = compaction_start.elapsed().as_secs_f64();
    state
        .metrics
        .compaction_duration
        .observe(compaction_elapsed);

    // A model declared by the Bedrock provider is served only by that provider.
    // This is exact routing, not a failover path.
    // When an alias resolved, its targets are authoritative; don't let the
    // exact-model owner override collapse them.
    if resolved.via_alias.is_none() {
        if let Some(owner) = crate::bedrock::provider_for_exact_model(
            &state.gateway_config.providers,
            &effective_model,
        ) {
            resolved.providers = vec![WeightedProvider {
                name: owner.id.clone(),
                weight: 1,
                model: effective_model.clone(),
            }];
        }
    }

    // ─── Provider Selection & Failover Loop ──────────────────────────────────
    let mut excluded_providers: Vec<String> = Vec::new();
    let max_attempts = resolved.providers.len().min(3);
    let mut last_error: Option<GatewayError> = None;
    let mut provider_type_used = String::from("unknown");

    let response = 'failover: {
        for _attempt in 0..max_attempts {
            // Select provider, excluding previously failed ones
            let available: Vec<WeightedProvider> = resolved
                .providers
                .iter()
                .filter(|p| !excluded_providers.contains(&p.name))
                .cloned()
                .collect();

            if available.is_empty() {
                break 'failover Err(last_error.unwrap_or(GatewayError::ServiceUnavailable(
                    "All providers exhausted".to_string(),
                )));
            }

            let selected = match state.load_balancer.select_provider(&available) {
                Ok(p) => p,
                Err(e) => break 'failover Err(e),
            };
            let selected_name = selected.name.clone();
            // The concrete model to forward for THIS attempt: the selected target's
            // model (for an alias, each target has its own model; for a normal route
            // this equals the effective model). Empty = keep the payload's model.
            let selected_model = selected.model.clone();

            // Check circuit breaker. A half-open probe lease must outlive the provider
            // call (timeout + margin) so it neither expires mid-call nor stays stuck
            // if the client disconnects: it then simply expires and another probe runs.
            let probe_lease = state
                .gateway_config
                .providers
                .iter()
                .find(|p| p.id == selected_name || p.provider_type == selected_name)
                .map(|p| p.timeout())
                .unwrap_or(Duration::from_secs(120))
                + Duration::from_secs(1);
            let admission = state
                .circuit_breaker
                .admit(&selected_name, probe_lease)
                .await;
            if admission.is_denied() {
                // Distinguish a provider-failure circuit from a Redis outage.
                let reason = if admission == crate::circuit_breaker::Admission::DenyUnverified {
                    "breaker_state_unavailable"
                } else {
                    "circuit_open"
                };
                state
                    .metrics
                    .failover_total
                    .with_label_values(&[&selected_name, reason])
                    .inc();
                state
                    .metrics
                    .fallback_mode_total
                    .with_label_values(&[&selected_name, "next", reason])
                    .inc();
                excluded_providers.push(selected_name);
                continue;
            }

            // Resolve provider config
            let pconfig = state
                .gateway_config
                .providers
                .iter()
                .find(|p| p.id == selected_name || p.provider_type == selected_name);

            let base_url = pconfig
                .map(|p| p.base_url.clone())
                .unwrap_or_else(|| "http://localhost:11434".to_string());
            let api_key = pconfig.map(|p| p.api_key.clone()).unwrap_or_default();
            let ptype = pconfig
                .map(|p| p.provider_type.clone())
                .unwrap_or_else(|| "ollama".to_string());
            let timeout = pconfig
                .map(|p| p.timeout())
                .unwrap_or(Duration::from_secs(120));
            provider_type_used = ptype.clone();

            // Build forward URL
            let url = if ptype == crate::bedrock::PROVIDER_TYPE {
                crate::bedrock::chat_completions_url(&base_url)
            } else if ptype == "anthropic" {
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
            let mut bearer_secret = String::new();
            if ptype == crate::bedrock::PROVIDER_TYPE {
                let provider = match pconfig {
                    Some(provider) => provider,
                    None => {
                        break 'failover Err(GatewayError::Unauthorized(
                            "Bedrock provider configuration is missing".to_string(),
                        ));
                    }
                };
                match crate::bedrock::ProviderCredential::from_provider(provider).resolve() {
                    Ok(token) => {
                        if let Err(error) =
                            crate::bedrock::insert_bearer_header(&mut provider_headers, &token)
                        {
                            break 'failover Err(error);
                        }
                        bearer_secret = token;
                    }
                    Err(error) => break 'failover Err(error),
                }
            } else if !api_key.is_empty() && api_key != "ollama" {
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

            // Per-attempt payload: forward the selected target's concrete model.
            let attempt_model = if selected_model.is_empty() {
                effective_model.clone()
            } else {
                selected_model.clone()
            };
            let mut attempt_payload = payload.clone();
            if let Some(obj) = attempt_payload.as_object_mut() {
                obj.insert("model".to_string(), Value::String(attempt_model.clone()));
            }

            // Translate payload
            let translation_start = Instant::now();
            let body = if ptype == "anthropic" {
                let transpiler = crate::transpiler::get_transpiler("anthropic");
                match transpiler.to_native(&attempt_payload) {
                    Ok(native) => bytes::Bytes::from(serde_json::to_vec(&native).unwrap()),
                    Err(e) => {
                        tracing::error!(error = %e, "Failed to translate payload to Anthropic format");
                        break 'failover Err(GatewayError::Internal(
                            "Payload translation failed".to_string(),
                        ));
                    }
                }
            } else if ptype == crate::bedrock::PROVIDER_TYPE {
                let prepared = crate::bedrock::prepare_chat_body(&attempt_payload, &attempt_model);
                bytes::Bytes::from(serde_json::to_vec(&prepared).unwrap())
            } else {
                bytes::Bytes::from(serde_json::to_vec(&attempt_payload).unwrap())
            };
            let translation_elapsed = translation_start.elapsed().as_secs_f64();
            state
                .metrics
                .payload_translation
                .observe(translation_elapsed);

            // ─── Streaming: attempt once (can't retry mid-stream) ─────────────
            if streaming {
                let provider_start = Instant::now();
                let stream = state
                    .http_client
                    .send_stream(&url, provider_headers, body, timeout);

                backend_elapsed_time = provider_start.elapsed().as_secs_f64();
                state
                    .metrics
                    .backend_latency
                    .with_label_values(&[&ptype])
                    .observe(backend_elapsed_time);

                state
                    .circuit_breaker
                    .record_outcome(&selected_name, CallResult::Success, &admission)
                    .await;

                let transpiler = crate::transpiler::get_transpiler(&ptype);
                let guardrails = request_guardrails.clone();
                let transformed = crate::streaming::transform_sse_stream_guarded(
                    Box::pin(stream.map(|r| {
                        r.map_err(|e| crate::streaming::StreamError::Connection(e.to_string()))
                    })),
                    transpiler,
                    guardrails,
                    Some(state.metrics.clone()),
                );

                break 'failover Ok(Sse::new(transformed).into_response());
            }

            // ─── Non-streaming: try and handle errors ─────────────────────────
            let provider_start = Instant::now();
            match state
                .http_client
                .send(&url, provider_headers, body, timeout)
                .await
            {
                Ok(response_bytes) => {
                    backend_elapsed_time = provider_start.elapsed().as_secs_f64();
                    state
                        .metrics
                        .backend_latency
                        .with_label_values(&[&ptype])
                        .observe(backend_elapsed_time);

                    state
                        .circuit_breaker
                        .record_outcome(&selected_name, CallResult::Success, &admission)
                        .await;

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
                            } else if ptype == crate::bedrock::PROVIDER_TYPE {
                                crate::bedrock::normalize_chat_response(&json_response)
                            } else {
                                json_response
                            };

                            let mut final_response = final_response;

                            // Fill `usage` with real token counts when the provider omitted
                            // it (or returned zeros) and real counting is enabled for this route.
                            fill_usage_if_missing(&mut final_response, &payload, &token_counter);

                            // ─── Semantic cache store on miss (Phase 2) ──────────
                            // Only store when the response came from the SAME
                            // destination the cache key was built for. A failover to
                            // a different provider/model must not be stored under the
                            // primary destination's namespace (it would be an
                            // incompatible entry). The lookup was deterministic
                            // against the primary; a diverged store is skipped.
                            let dest_matches = cache_primary_dest
                                .as_ref()
                                .map(|(name, model)| {
                                    *name == selected_name && *model == attempt_model
                                })
                                .unwrap_or(false);
                            if let (Some(ctx), Some(entry_id), Some(embedding), true) =
                                (&cache_ctx, &cache_entry_id, &cache_embedding, dest_matches)
                            {
                                state
                                    .semantic_cache
                                    .store(ctx, entry_id, embedding, &final_response, false)
                                    .await;
                            } else if cache_ctx.is_some() && !dest_matches {
                                state
                                    .metrics
                                    .cache_skipped_total
                                    .with_label_values(&["failover_destination_changed"])
                                    .inc();
                                tracing::debug!(
                                    "cache store skipped: response came from a failover destination, \
                                     not the primary the key was built for"
                                );
                            }

                            // Phase 2 reserved hook (kept) + Phase 3 real outbound
                            // guardrails. Note: the cache stores the unfiltered provider
                            // response above; filtering is applied on every delivery
                            // (fresh and cache hit), so a cache hit still passes through here.
                            let mut final_response = state.outbound_guardrail.apply(final_response);
                            if let Some(guardrails) = request_guardrails.clone() {
                                if let Err(rule) = crate::guardrails::apply_outbound_to_response(
                                    &mut final_response,
                                    guardrails.as_ref(),
                                    &state.metrics,
                                ) {
                                    tracing::warn!(
                                        rule = %rule,
                                        route = %request_path,
                                        "outbound guardrail blocked provider response"
                                    );
                                    break 'failover Err(GatewayError::ContentBlocked(format!(
                                        "Response blocked by guardrail rule '{}'",
                                        rule
                                    )));
                                }
                            }

                            // Record token metrics
                            if let Some(usage) = final_response.get("usage") {
                                let prompt_tokens = usage
                                    .get("prompt_tokens")
                                    .and_then(|v| v.as_u64())
                                    .unwrap_or(0);
                                let completion_tokens = usage
                                    .get("completion_tokens")
                                    .and_then(|v| v.as_u64())
                                    .unwrap_or(0);
                                let model_name = final_response
                                    .get("model")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or(&effective_model);
                                state
                                    .metrics
                                    .llm_tokens_total
                                    .with_label_values(&["input", model_name, "unknown"])
                                    .inc_by(prompt_tokens);
                                state
                                    .metrics
                                    .llm_tokens_total
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
                Err(crate::client::ClientError::HttpError {
                    status,
                    body: err_body,
                }) => {
                    backend_elapsed_time = provider_start.elapsed().as_secs_f64();
                    state
                        .metrics
                        .backend_latency
                        .with_label_values(&[&ptype])
                        .observe(backend_elapsed_time);
                    state
                        .metrics
                        .provider_errors_total
                        .with_label_values(&[&ptype, &status.to_string()])
                        .inc();

                    if !crate::bedrock::retries_another_provider(&ptype) {
                        if status == 429 {
                            state
                                .circuit_breaker
                                .record_outcome(&selected_name, CallResult::RateLimited, &admission)
                                .await;
                        } else if status >= 500 {
                            state
                                .circuit_breaker
                                .record_outcome(&selected_name, CallResult::Failure, &admission)
                                .await;
                        }
                        break 'failover Err(crate::bedrock::http_status_error(
                            status,
                            &err_body,
                            &bearer_secret,
                            &effective_model,
                        ));
                    }
                    match status {
                        // Non-retryable errors — return immediately
                        400 => {
                            break 'failover Err(GatewayError::BadRequest(format!(
                                "Provider error: {}",
                                err_body
                            )));
                        }
                        401 | 403 => {
                            break 'failover Err(GatewayError::Unauthorized(format!(
                                "Provider auth error: {}",
                                err_body
                            )));
                        }
                        404 => {
                            break 'failover Err(GatewayError::BadRequest(format!(
                                "Model '{}' not found at provider",
                                effective_model
                            )));
                        }
                        // Retryable — record failure and try next provider
                        429 => {
                            state
                                .circuit_breaker
                                .record_outcome(&selected_name, CallResult::RateLimited, &admission)
                                .await;
                            state
                                .metrics
                                .failover_total
                                .with_label_values(&[&selected_name, "429"])
                                .inc();
                            excluded_providers.push(selected_name);
                            last_error = Some(GatewayError::RateLimited {
                                retry_after_secs: 30,
                            });
                        }
                        s if s >= 500 => {
                            state
                                .circuit_breaker
                                .record_outcome(&selected_name, CallResult::Failure, &admission)
                                .await;
                            state
                                .metrics
                                .failover_total
                                .with_label_values(&[&selected_name, "5xx"])
                                .inc();
                            excluded_providers.push(selected_name);
                            last_error = Some(GatewayError::ServiceUnavailable(format!(
                                "Provider error {}: {}",
                                status, err_body
                            )));
                        }
                        _ => {
                            excluded_providers.push(selected_name);
                            last_error = Some(GatewayError::ProviderError(format!(
                                "HTTP {}: {}",
                                status, err_body
                            )));
                        }
                    }
                }
                Err(crate::client::ClientError::Timeout) => {
                    if !crate::bedrock::retries_another_provider(&ptype) {
                        state
                            .circuit_breaker
                            .record_outcome(&selected_name, CallResult::Timeout, &admission)
                            .await;
                        state
                            .metrics
                            .provider_errors_total
                            .with_label_values(&[&ptype, "timeout"])
                            .inc();
                        break 'failover Err(crate::bedrock::timeout_error());
                    }
                    backend_elapsed_time = provider_start.elapsed().as_secs_f64();
                    state
                        .metrics
                        .backend_latency
                        .with_label_values(&[&ptype])
                        .observe(backend_elapsed_time);
                    state
                        .circuit_breaker
                        .record_outcome(&selected_name, CallResult::Timeout, &admission)
                        .await;
                    state
                        .metrics
                        .provider_errors_total
                        .with_label_values(&[&ptype, "timeout"])
                        .inc();
                    state
                        .metrics
                        .failover_total
                        .with_label_values(&[&selected_name, "timeout"])
                        .inc();
                    excluded_providers.push(selected_name);
                    last_error = Some(GatewayError::ServiceUnavailable(
                        "Provider timeout".to_string(),
                    ));
                }
                Err(e) => {
                    if !crate::bedrock::retries_another_provider(&ptype) {
                        let safe = crate::bedrock::redact(&e.to_string(), &bearer_secret);
                        state
                            .circuit_breaker
                            .record_outcome(&selected_name, CallResult::Failure, &admission)
                            .await;
                        break 'failover Err(GatewayError::ServiceUnavailable(format!(
                            "Provider unavailable: {safe}"
                        )));
                    }
                    backend_elapsed_time = provider_start.elapsed().as_secs_f64();
                    state
                        .metrics
                        .backend_latency
                        .with_label_values(&[&ptype])
                        .observe(backend_elapsed_time);
                    state
                        .circuit_breaker
                        .record_outcome(&selected_name, CallResult::Failure, &admission)
                        .await;
                    state
                        .metrics
                        .provider_errors_total
                        .with_label_values(&[&ptype, "network_error"])
                        .inc();
                    state
                        .metrics
                        .failover_total
                        .with_label_values(&[&selected_name, "network"])
                        .inc();
                    excluded_providers.push(selected_name);
                    last_error = Some(GatewayError::ServiceUnavailable(format!(
                        "Provider unavailable: {}",
                        e
                    )));
                }
            }
        }

        // All attempts exhausted
        Err(last_error.unwrap_or(GatewayError::ServiceUnavailable(
            "All providers exhausted".to_string(),
        )))
    };

    // Record fallback success metric if we had failures but ultimately succeeded
    if !excluded_providers.is_empty() {
        if response.is_ok() {
            state
                .metrics
                .fallback_mode_total
                .with_label_values(&[
                    excluded_providers
                        .first()
                        .map(|s| s.as_str())
                        .unwrap_or("unknown"),
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

    state
        .metrics
        .requests_total
        .with_label_values(&[request_path, "unknown", &status])
        .inc();
    state
        .metrics
        .request_duration
        .with_label_values(&[request_path, &provider_type_used])
        .observe(total_elapsed);
    state.metrics.internal_overhead.observe(internal_overhead);
    state.metrics.gateway_overhead.observe(total_elapsed);

    Ok(response)
}

/// Builds an SSE response that replays a cached (non-streaming) OpenAI response
/// as a single `chat.completion.chunk` followed by `[DONE]`.
///
/// This lets streaming clients benefit from the cache: the assistant content from
/// the cached `choices[0].message.content` is emitted as one delta chunk.
fn cache_hit_sse_response(body: &Value) -> Response {
    use axum::response::sse::{Event, Sse};
    use futures_util::stream;

    let id = body
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("chatcmpl-cache")
        .to_string();
    let model = body
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let content = body
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|c| c.first())
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .unwrap_or("")
        .to_string();
    let created = body.get("created").and_then(|v| v.as_u64()).unwrap_or(0);

    let chunk = serde_json::json!({
        "id": id,
        "object": "chat.completion.chunk",
        "created": created,
        "model": model,
        "choices": [{
            "index": 0,
            "delta": {"role": "assistant", "content": content},
            "finish_reason": "stop"
        }]
    });

    let events: Vec<Result<Event, std::convert::Infallible>> = vec![
        Ok(Event::default().data(chunk.to_string())),
        Ok(Event::default().data("[DONE]")),
    ];
    Sse::new(stream::iter(events)).into_response()
}

/// Fills the `usage` field of an OpenAI response with real token counts when the
/// provider omitted it (or returned zeros) and real token counting is enabled.
///
/// Rules:
/// - Only acts when `counter` is in real mode (`TokenCounter::Real`). In estimated
///   mode this is a no-op, preserving legacy behavior.
/// - Preserves provider-supplied values: if `prompt_tokens` and `completion_tokens`
///   are both already non-zero, nothing is changed.
/// - `prompt` tokens are counted from the (post-compaction) request messages;
///   `completion` tokens from the assistant content in the first choice.
fn fill_usage_if_missing(response: &mut Value, request_payload: &Value, counter: &TokenCounter) {
    // No synthetic usage in estimated mode.
    if counter.mode_label() != "real" {
        return;
    }

    let existing_prompt = response
        .get("usage")
        .and_then(|u| u.get("prompt_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let existing_completion = response
        .get("usage")
        .and_then(|u| u.get("completion_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    // Provider already supplied real usage — do not overwrite.
    if existing_prompt > 0 && existing_completion > 0 {
        return;
    }

    // Count prompt tokens from the request messages' content.
    let prompt_tokens = request_payload
        .get("messages")
        .and_then(|m| m.as_array())
        .map(|msgs| {
            msgs.iter()
                .filter_map(|m| m.get("content").and_then(|c| c.as_str()))
                .map(|text| counter.count(text) as u64)
                .sum::<u64>()
        })
        .unwrap_or(0);

    // Count completion tokens from the first choice's assistant content.
    let completion_tokens = response
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|choices| choices.first())
        .and_then(|choice| choice.get("message"))
        .and_then(|msg| msg.get("content"))
        .and_then(|c| c.as_str())
        .map(|text| counter.count(text) as u64)
        .unwrap_or(0);

    let total_tokens = prompt_tokens + completion_tokens;

    response["usage"] = serde_json::json!({
        "prompt_tokens": prompt_tokens,
        "completion_tokens": completion_tokens,
        "total_tokens": total_tokens,
    });
}

/// Phase 5 metrics + one content-free log line per scored compaction.
fn record_scoring_outcome(
    metrics: &crate::observability::Metrics,
    o: &crate::semantic_scoring::ScoringOutcome,
) {
    if !o.needed {
        return; // below threshold: nothing was scored
    }
    metrics
        .compaction_scoring_total
        .with_label_values(&[o.requested.label(), o.effective.label()])
        .inc();
    metrics
        .compaction_scoring_seconds
        .observe(o.elapsed.as_secs_f64());
    if let Some(r) = o.fallback {
        metrics
            .compaction_scoring_fallback_total
            .with_label_values(&[r.label()])
            .inc();
    }
    if let Some(r) = o.skipped {
        metrics
            .compaction_scoring_skipped_total
            .with_label_values(&[r.label()])
            .inc();
    }
    tracing::info!(
        requested_mode = o.requested.label(),
        effective_mode = o.effective.label(),
        fallback_reason = o.fallback.map(|r| r.label()).unwrap_or("none"),
        skipped_reason = o.skipped.map(|r| r.label()).unwrap_or("none"),
        candidates = o.candidates,
        embedded = o.embedded,
        pruned = o.compaction.result.messages_pruned,
        elapsed_ms = o.elapsed.as_millis() as u64,
        "compaction scoring"
    );
}

/// Phase 6A alias-resolution metric + content-free log. `alias` label is a declared
/// alias name (bounded) or `other`; never an arbitrary client string.
fn record_alias_resolution(
    metrics: &crate::observability::Metrics,
    resolved: &ResolvedRoute,
    outcome: &str,
) {
    let alias_label = resolved.via_alias.as_deref().unwrap_or("other");
    metrics
        .model_alias_resolution_total
        .with_label_values(&[alias_label, outcome])
        .inc();
}

/// Checks that every model in `models` is permitted by `allowed`.
///
/// An empty `allowed` list means unrestricted (the field was previously carried
/// but never enforced; empty preserves that behavior). Returns `Err(message)` with
/// the first model that is not allowed. The message names only the model, never the
/// allowed list, to avoid leaking a client's full entitlement set.
fn authorize_models<'a>(
    allowed: &[String],
    models: impl IntoIterator<Item = &'a String>,
) -> Result<(), String> {
    if allowed.is_empty() {
        return Ok(());
    }
    for m in models {
        if !allowed.iter().any(|a| a == m) {
            return Err(format!("Model '{m}' is not permitted for this client."));
        }
    }
    Ok(())
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
    use crate::route_config::ModelResolution;

    match resolver.resolve_route(path, method) {
        Some(route) => {
            let effective_token_config = resolver.effective_token_config(route, global_compactor);
            let semantic_cache = route.semantic_cache.clone().filter(|c| c.enabled);
            let semantic_scoring = resolver.effective_scoring_config(route);

            // ── Precedence (Phase 6A): route override > payload alias > concrete >
            //    default. `model` is a mandatory override; `default_model` only
            //    fills in when the client sent no usable model (and never shadows a
            //    client alias).
            let mut via_alias = None;
            let mut alias_multi_target = false;
            let mut resolution_error = None;

            // Default builder for a concrete model: route providers or single provider.
            let concrete_providers = |model: &str| -> Vec<WeightedProvider> {
                extract_providers_with_model(route, model)
            };

            let (effective_model, providers) = if let Some(forced) = &route.model {
                // (1) Mandatory route override: alias from the client is ignored.
                (forced.clone(), concrete_providers(forced))
            } else {
                match resolver.resolve_model(payload_model) {
                    // (2) payload is a known alias.
                    ModelResolution::Alias { name, targets } => {
                        via_alias = Some(name);
                        alias_multi_target = targets.len() > 1;
                        let providers: Vec<WeightedProvider> = targets
                            .iter()
                            .map(|t| WeightedProvider {
                                name: t.provider.clone(),
                                weight: t.weight,
                                model: t.model.clone(),
                            })
                            .collect();
                        // The effective `model` field forwarded is set per-target at
                        // send time; here we keep the first target's model as the
                        // representative effective model for metrics/cache checks.
                        let eff = providers
                            .first()
                            .map(|p| p.model.clone())
                            .unwrap_or_else(|| payload_model.to_string());
                        (eff, providers)
                    }
                    // (3) concrete model from the payload.
                    ModelResolution::Concrete(m) => {
                        // Unknown bare name is a concrete passthrough; but if the
                        // payload was empty/unusable, apply default_model (4).
                        if m.is_empty() {
                            match &route.default_model {
                                Some(d) => (d.clone(), concrete_providers(d)),
                                None => (m.clone(), concrete_providers(&m)),
                            }
                        } else {
                            (m.clone(), concrete_providers(&m))
                        }
                    }
                    // (5) `@unknown` → hard error, never forwarded.
                    ModelResolution::UnknownRequiredAlias(a) => {
                        resolution_error = Some(format!(
                            "Unknown model alias '@{a}' (no such alias declared)."
                        ));
                        (payload_model.to_string(), Vec::new())
                    }
                }
            };

            ResolvedRoute {
                model: effective_model,
                requested_model: payload_model.to_string(),
                via_alias,
                alias_multi_target,
                resolution_error,
                effective_token_config,
                providers,
                semantic_cache,
                semantic_scoring,
            }
        }
        None => {
            // No matching route — resolve aliases against the global alias table so
            // an alias still works on an unmatched path; otherwise defaults.
            tracing::warn!(
                path = %path,
                method = %method,
                "No matching route found in route config, using defaults"
            );
            let mut via_alias = None;
            let mut alias_multi_target = false;
            let mut resolution_error = None;
            let (model, providers) = match resolver.resolve_model(payload_model) {
                crate::route_config::ModelResolution::Alias { name, targets } => {
                    via_alias = Some(name);
                    alias_multi_target = targets.len() > 1;
                    let providers = targets
                        .iter()
                        .map(|t| WeightedProvider {
                            name: t.provider.clone(),
                            weight: t.weight,
                            model: t.model.clone(),
                        })
                        .collect::<Vec<_>>();
                    let eff = targets
                        .first()
                        .map(|t| t.model.clone())
                        .unwrap_or_else(|| payload_model.to_string());
                    (eff, providers)
                }
                crate::route_config::ModelResolution::UnknownRequiredAlias(a) => {
                    resolution_error = Some(format!(
                        "Unknown model alias '@{a}' (no such alias declared)."
                    ));
                    (payload_model.to_string(), Vec::new())
                }
                crate::route_config::ModelResolution::Concrete(m) => (m, Vec::new()),
            };
            ResolvedRoute {
                model,
                requested_model: payload_model.to_string(),
                via_alias,
                alias_multi_target,
                resolution_error,
                effective_token_config: global_compactor.clone(),
                providers,
                semantic_cache: None,
                semantic_scoring: None,
            }
        }
    }
}

/// Builds the provider list for a route, using `model` for the single-`provider`
/// case (so a concrete/alias-resolved model is carried to the provider).
/// `providers` (multi-provider) is returned as-is; a single `provider` becomes a
/// one-entry list with `model`; otherwise an empty list.
fn extract_providers_with_model(route: &RouteDefinition, model: &str) -> Vec<WeightedProvider> {
    if let Some(ref providers) = route.providers {
        providers.clone()
    } else if let Some(ref provider_name) = route.provider {
        vec![WeightedProvider {
            name: provider_name.clone(),
            weight: 1,
            model: model.to_string(),
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
                proptest::collection::vec((role_strategy, content_strategy), 1..5),
            )
                .prop_map(|(model, messages)| {
                    let msgs: Vec<serde_json::Value> = messages
                        .into_iter()
                        .map(
                            |(role, content)| serde_json::json!({"role": role, "content": content}),
                        )
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

    #[test]
    fn authorize_models_empty_list_is_unrestricted() {
        let allowed: Vec<String> = vec![];
        assert!(authorize_models(&allowed, [&"anything".to_string()]).is_ok());
    }

    #[test]
    fn authorize_models_enforces_requested_and_resolved() {
        let allowed = vec!["gpt-4o".to_string()];
        // Permitted.
        assert!(authorize_models(&allowed, [&"gpt-4o".to_string()]).is_ok());
        // Requested not allowed.
        assert!(authorize_models(&allowed, [&"llama3.2".to_string()]).is_err());
        // Requested allowed but resolved (e.g. route override) not allowed → deny.
        let err = authorize_models(
            &allowed,
            [&"gpt-4o".to_string(), &"secret-model".to_string()],
        )
        .unwrap_err();
        assert!(err.contains("secret-model"));
        // The message must not leak the full allow-list.
        assert!(!err.contains("gpt-4o"));
    }

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
        let resolved = resolve_route_config(&resolver, "/v1/chat/raw", "POST", "llama3", &global);

        // No route model override — should keep payload model
        assert_eq!(resolved.model, "llama3");
    }

    #[test]
    fn test_resolve_route_uses_per_route_token_config() {
        let resolver = RouteResolver::new(sample_config());
        let global = global_compactor();

        let resolved =
            resolve_route_config(&resolver, "/v1/chat/completions", "POST", "gpt-4o", &global);

        // Route has token_optimization with compress_above_tokens: 4000
        assert_eq!(resolved.effective_token_config.token_threshold, 4000);
        assert_eq!(
            resolved.effective_token_config.tokenizer_name,
            "cl100k_base"
        );
        // stop_words come from global
        assert_eq!(
            resolved.effective_token_config.stop_words,
            global.stop_words
        );
    }

    #[test]
    fn test_resolve_route_uses_global_token_config_when_no_per_route() {
        let resolver = RouteResolver::new(sample_config());
        let global = global_compactor();

        // /v1/chat/raw has no token_optimization
        let resolved = resolve_route_config(&resolver, "/v1/chat/raw", "POST", "llama3", &global);

        assert_eq!(resolved.effective_token_config, global);
    }

    #[test]
    fn test_resolve_route_extracts_multi_providers() {
        let resolver = RouteResolver::new(sample_config());
        let global = global_compactor();

        let resolved = resolve_route_config(&resolver, "/v1/chat/multi", "POST", "gpt-4o", &global);

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

        let resolved =
            resolve_route_config(&resolver, "/v1/chat/completions", "POST", "gpt-4o", &global);

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

        let resolved =
            resolve_route_config(&resolver, "/v1/chat/unknown", "POST", "my-model", &global);

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

        let route_config =
            RouteConfigManager::new_without_watcher(gateway_config.routes_config_path.clone());

        let app_state = AppState {
            route_config: Arc::new(route_config),
            gateway_config: Arc::new(gateway_config),
            rate_limiter: Arc::new(LocalTokenBucket::new()),
            load_balancer: Arc::new(WeightedRoundRobin::new()),
            circuit_breaker: Arc::new(LocalCircuitBreaker::new(Default::default())),
            http_client: Arc::new(ReqwestLlmClient::new()),
            metrics: Arc::new(Metrics::new()),
            redis_available: Arc::new(AtomicBool::new(true)),
            semantic_cache: Arc::new(crate::semantic_cache::DisabledCache),
            outbound_guardrail: Arc::new(crate::semantic_cache::NoopOutboundGuardrail),
            guardrails: Arc::new(crate::guardrails::GuardrailEngine::new(
                &crate::route_config::RouteConfigFile {
                    model_aliases: Vec::new(),
                    routes: vec![],
                    custom_providers: vec![],
                },
                Default::default(),
            )),
            embeddings: Arc::new(crate::embeddings::StubEmbeddingProvider::deterministic()),
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
  required_for_readiness: true
routes_config_path: "./routes.yaml"
"#,
        )
        .unwrap();

        let route_config =
            RouteConfigManager::new_without_watcher(gateway_config.routes_config_path.clone());

        let app_state = AppState {
            route_config: Arc::new(route_config),
            gateway_config: Arc::new(gateway_config),
            rate_limiter: Arc::new(LocalTokenBucket::new()),
            load_balancer: Arc::new(WeightedRoundRobin::new()),
            circuit_breaker: Arc::new(LocalCircuitBreaker::new(Default::default())),
            http_client: Arc::new(ReqwestLlmClient::new()),
            metrics: Arc::new(Metrics::new()),
            redis_available: Arc::new(AtomicBool::new(false)), // Redis unavailable
            semantic_cache: Arc::new(crate::semantic_cache::DisabledCache),
            outbound_guardrail: Arc::new(crate::semantic_cache::NoopOutboundGuardrail),
            guardrails: Arc::new(crate::guardrails::GuardrailEngine::new(
                &crate::route_config::RouteConfigFile {
                    model_aliases: Vec::new(),
                    routes: vec![],
                    custom_providers: vec![],
                },
                Default::default(),
            )),
            embeddings: Arc::new(crate::embeddings::StubEmbeddingProvider::deterministic()),
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

    // --- fill_usage_if_missing unit tests (Phase 1) ---

    use crate::config::TokenCountingMode;
    use crate::tokenizer::TokenCounter;

    fn req_payload() -> Value {
        serde_json::json!({
            "model": "gpt-4",
            "messages": [
                {"role": "system", "content": "You are a helpful assistant."},
                {"role": "user", "content": "Summarize the quarterly report in two sentences."}
            ]
        })
    }

    fn response_with_content(content: &str) -> Value {
        serde_json::json!({
            "id": "chatcmpl-1",
            "object": "chat.completion",
            "model": "gpt-4",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": content},
                "finish_reason": "stop"
            }]
        })
    }

    #[test]
    fn fill_usage_noop_in_estimated_mode() {
        let counter = TokenCounter::resolve(TokenCountingMode::Estimated, "cl100k_base");
        let mut response = response_with_content("Revenue grew. Costs fell.");
        fill_usage_if_missing(&mut response, &req_payload(), &counter);
        // Estimated mode must not synthesize usage.
        assert!(response.get("usage").is_none());
    }

    #[test]
    fn fill_usage_fills_when_missing_in_real_mode() {
        let counter = TokenCounter::resolve(TokenCountingMode::Real, "cl100k_base");
        let mut response = response_with_content("Revenue grew. Costs fell.");
        fill_usage_if_missing(&mut response, &req_payload(), &counter);

        let usage = response.get("usage").expect("usage should be filled");
        let prompt = usage["prompt_tokens"].as_u64().unwrap();
        let completion = usage["completion_tokens"].as_u64().unwrap();
        let total = usage["total_tokens"].as_u64().unwrap();
        assert!(prompt > 0, "prompt_tokens should be counted");
        assert!(completion > 0, "completion_tokens should be counted");
        assert_eq!(total, prompt + completion);
    }

    #[test]
    fn fill_usage_preserves_provider_values() {
        let counter = TokenCounter::resolve(TokenCountingMode::Real, "cl100k_base");
        let mut response = response_with_content("Revenue grew. Costs fell.");
        response["usage"] = serde_json::json!({
            "prompt_tokens": 11,
            "completion_tokens": 22,
            "total_tokens": 33
        });
        fill_usage_if_missing(&mut response, &req_payload(), &counter);

        // Provider-supplied non-zero values must be preserved untouched.
        assert_eq!(response["usage"]["prompt_tokens"], 11);
        assert_eq!(response["usage"]["completion_tokens"], 22);
        assert_eq!(response["usage"]["total_tokens"], 33);
    }

    #[test]
    fn fill_usage_overwrites_zeroed_provider_values_in_real_mode() {
        let counter = TokenCounter::resolve(TokenCountingMode::Real, "cl100k_base");
        let mut response = response_with_content("Revenue grew. Costs fell.");
        response["usage"] = serde_json::json!({
            "prompt_tokens": 0,
            "completion_tokens": 0,
            "total_tokens": 0
        });
        fill_usage_if_missing(&mut response, &req_payload(), &counter);

        assert!(response["usage"]["prompt_tokens"].as_u64().unwrap() > 0);
        assert!(response["usage"]["completion_tokens"].as_u64().unwrap() > 0);
    }
}
