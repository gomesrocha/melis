//! Integration tests for the Melis AI Gateway.
//!
//! End-to-end tests that exercise the full request pipeline through
//! the Axum router using `tower::ServiceExt::oneshot`.
//!
//! These tests build a complete `AppState` with local/in-memory implementations
//! (LocalTokenBucket, LocalCircuitBreaker, WeightedRoundRobin, ReqwestLlmClient)
//! and validate JSON responses, SSE streaming, payload validation, health checks,
//! and the metrics endpoint.

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use crate::balancer::WeightedRoundRobin;
    use crate::circuit_breaker::LocalCircuitBreaker;
    use crate::client::ReqwestLlmClient;
    use crate::config::GatewayConfig;
    use crate::middleware::rate_limiter::LocalTokenBucket;
    use crate::observability::Metrics;
    use crate::route_config::RouteConfigManager;
    use crate::router::build_router;
    use crate::state::AppState;

    /// Creates a full AppState with local/in-memory implementations.
    /// Auth is disabled so requests can flow through without Bearer tokens.
    fn build_test_app_state() -> AppState {
        let yaml = r#"
server:
  host: "0.0.0.0"
  port: 8080
redis:
  cluster_urls:
    - "redis://localhost:6379"
auth:
  enabled: false
"#;
        let gateway_config = GatewayConfig::from_yaml(yaml).unwrap();

        let route_config = RouteConfigManager::new_for_test();

        AppState {
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
                    routes: vec![],
                    custom_providers: vec![],
                },
                Default::default(),
            )),
            embeddings: Arc::new(crate::embeddings::StubEmbeddingProvider::deterministic()),
        }
    }

    /// Helper to collect the full response body as a string.
    async fn body_to_string(body: Body) -> String {
        let bytes = body.collect().await.unwrap().to_bytes();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    // ─── Test 1: JSON Response ────────────────────────────────────────────────

    #[tokio::test]
    #[ignore = "requires a live provider (Ollama) at localhost:11434; run in pre-merge env"]
    async fn test_json_response_with_valid_payload() {
        let state = build_test_app_state();
        let app = build_router(state);

        let payload = serde_json::json!({
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": "Hello, world!"}]
        });

        let request = Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header("Content-Type", "application/json")
            .body(Body::from(serde_json::to_vec(&payload).unwrap()))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = body_to_string(response.into_body()).await;
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();

        // Verify response has expected OpenAI-compatible structure
        assert_eq!(json["object"], "chat.completion");
        assert!(json["choices"].is_array());
        assert!(!json["choices"].as_array().unwrap().is_empty());
        assert_eq!(json["choices"][0]["message"]["role"], "assistant");
        assert!(json["choices"][0]["message"]["content"].is_string());
        assert_eq!(json["choices"][0]["finish_reason"], "stop");
        assert!(json["usage"].is_object());
    }

    // ─── Test 2: SSE Streaming Response ──────────────────────────────────────

    #[tokio::test]
    #[ignore = "requires a live provider (Ollama) at localhost:11434; run in pre-merge env"]
    async fn test_sse_response_with_accept_header() {
        let state = build_test_app_state();
        let app = build_router(state);

        let payload = serde_json::json!({
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": "Hello!"}]
        });

        let request = Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header("Content-Type", "application/json")
            .header("Accept", "text/event-stream")
            .body(Body::from(serde_json::to_vec(&payload).unwrap()))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        // Verify content-type is text/event-stream
        let content_type = response
            .headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap();
        assert!(
            content_type.contains("text/event-stream"),
            "Expected text/event-stream content-type, got: {}",
            content_type
        );

        // Verify body contains SSE data lines
        let body = body_to_string(response.into_body()).await;
        assert!(
            body.contains("data:"),
            "SSE response should contain 'data:' lines, got: {}",
            body
        );
    }

    #[tokio::test]
    #[ignore = "requires a live provider (Ollama) at localhost:11434; run in pre-merge env"]
    async fn test_sse_response_with_stream_true_in_payload() {
        let state = build_test_app_state();
        let app = build_router(state);

        let payload = serde_json::json!({
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": "Hi!"}],
            "stream": true
        });

        let request = Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header("Content-Type", "application/json")
            .body(Body::from(serde_json::to_vec(&payload).unwrap()))
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
            "Expected text/event-stream when stream: true, got: {}",
            content_type
        );

        let body = body_to_string(response.into_body()).await;
        assert!(body.contains("data:"));
        // Verify [DONE] marker is present in SSE stream
        assert!(
            body.contains("[DONE]"),
            "SSE stream should end with [DONE], got: {}",
            body
        );
    }

    // ─── Test 3: Payload Validation ──────────────────────────────────────────

    #[tokio::test]
    async fn test_invalid_payload_missing_model_returns_400() {
        let state = build_test_app_state();
        let app = build_router(state);

        let payload = serde_json::json!({
            "messages": [{"role": "user", "content": "Hello"}]
        });

        let request = Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header("Content-Type", "application/json")
            .body(Body::from(serde_json::to_vec(&payload).unwrap()))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let body = body_to_string(response.into_body()).await;
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(json["error"]["type"], "invalid_request_error");
        assert!(json["error"]["message"].as_str().unwrap().contains("model"));
    }

    #[tokio::test]
    async fn test_invalid_payload_missing_messages_returns_400() {
        let state = build_test_app_state();
        let app = build_router(state);

        let payload = serde_json::json!({
            "model": "gpt-4o"
        });

        let request = Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header("Content-Type", "application/json")
            .body(Body::from(serde_json::to_vec(&payload).unwrap()))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let body = body_to_string(response.into_body()).await;
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert!(json["error"]["message"]
            .as_str()
            .unwrap()
            .contains("messages"));
    }

    #[tokio::test]
    async fn test_invalid_payload_empty_messages_returns_400() {
        let state = build_test_app_state();
        let app = build_router(state);

        let payload = serde_json::json!({
            "model": "gpt-4o",
            "messages": []
        });

        let request = Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header("Content-Type", "application/json")
            .body(Body::from(serde_json::to_vec(&payload).unwrap()))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_invalid_payload_message_missing_role_returns_400() {
        let state = build_test_app_state();
        let app = build_router(state);

        let payload = serde_json::json!({
            "model": "gpt-4o",
            "messages": [{"content": "Hello"}]
        });

        let request = Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header("Content-Type", "application/json")
            .body(Body::from(serde_json::to_vec(&payload).unwrap()))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let body = body_to_string(response.into_body()).await;
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert!(json["error"]["message"].as_str().unwrap().contains("role"));
    }

    // ─── Test 4: Health Checks ───────────────────────────────────────────────

    #[tokio::test]
    async fn test_healthz_returns_200() {
        let state = build_test_app_state();
        let app = build_router(state);

        let request = Request::builder()
            .method("GET")
            .uri("/healthz")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = body_to_string(response.into_body()).await;
        assert_eq!(body, "OK");
    }

    #[tokio::test]
    async fn test_readyz_returns_200_when_redis_available() {
        let state = build_test_app_state();
        // redis_available is set to true in build_test_app_state
        let app = build_router(state);

        let request = Request::builder()
            .method("GET")
            .uri("/readyz")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = body_to_string(response.into_body()).await;
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(json["status"], "ready");
    }

    #[tokio::test]
    async fn test_readyz_returns_503_when_redis_unavailable() {
        let mut state = build_test_app_state();
        require_redis_for_readiness(&mut state);
        state.redis_available = Arc::new(AtomicBool::new(false));
        let app = build_router(state);

        let request = Request::builder()
            .method("GET")
            .uri("/readyz")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);

        let body = body_to_string(response.into_body()).await;
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(json["status"], "not_ready");
        assert!(json["reason"].as_str().unwrap().contains("Redis"));
    }

    /// Marks Redis as a REQUIRED readiness dependency (`redis.required_for_readiness`).
    fn require_redis_for_readiness(state: &mut AppState) {
        let mut cfg = (*state.gateway_config).clone();
        cfg.redis.required_for_readiness = true;
        state.gateway_config = Arc::new(cfg);
    }

    #[tokio::test]
    async fn test_readyz_stays_ready_when_redis_optional_and_unavailable() {
        // Default: Redis is optional, so an outage must not pull replicas out of
        // rotation; readiness reports the degradation instead.
        let mut state = build_test_app_state();
        assert!(
            !state.gateway_config.redis.required_for_readiness,
            "default is optional"
        );
        state.redis_available = Arc::new(AtomicBool::new(false));
        let app = build_router(state);
        let request = Request::builder()
            .method("GET")
            .uri("/readyz")
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_to_string(response.into_body()).await;
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(json["status"], "ready");
        assert_eq!(json["degraded"], serde_json::json!(["redis"]));
    }

    #[tokio::test]
    async fn test_healthz_never_depends_on_redis() {
        for required in [false, true] {
            let mut state = build_test_app_state();
            if required {
                require_redis_for_readiness(&mut state);
            }
            state.redis_available = Arc::new(AtomicBool::new(false));
            let app = build_router(state);
            let request = Request::builder()
                .method("GET")
                .uri("/healthz")
                .body(Body::empty())
                .unwrap();
            let response = app.oneshot(request).await.unwrap();
            assert_eq!(response.status(), StatusCode::OK, "required={required}");
        }
    }

    // ─── Test 5: Metrics Endpoint ────────────────────────────────────────────

    #[tokio::test]
    async fn test_metrics_endpoint_returns_200_with_prometheus_format() {
        let state = build_test_app_state();
        // Trigger at least one metric to ensure counters appear in output
        state
            .metrics
            .requests_total
            .with_label_values(&["/v1/chat/completions", "test", "200"])
            .inc();
        state.metrics.gateway_overhead.observe(0.001);

        let app = build_router(state);

        let request = Request::builder()
            .method("GET")
            .uri("/metrics")
            .body(Body::empty())
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
            content_type.contains("text/plain"),
            "Expected text/plain content-type for Prometheus, got: {}",
            content_type
        );

        let body = body_to_string(response.into_body()).await;
        // Prometheus text format should contain HELP and TYPE annotations
        assert!(
            body.contains("# HELP"),
            "Metrics should contain # HELP lines"
        );
        assert!(
            body.contains("# TYPE"),
            "Metrics should contain # TYPE lines"
        );
        assert!(
            body.contains("melis_gateway_requests_total"),
            "Metrics should contain gateway requests counter"
        );
        assert!(
            body.contains("melis_gateway_overhead_seconds"),
            "Metrics should contain gateway overhead histogram"
        );
    }

    // ─── Test: Metrics increment after request ───────────────────────────────

    #[tokio::test]
    #[ignore = "requires a live provider (Ollama) at localhost:11434; run in pre-merge env"]
    async fn test_metrics_increment_after_chat_completion_request() {
        let state = build_test_app_state();
        let metrics = state.metrics.clone();
        let app = build_router(state);

        let payload = serde_json::json!({
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": "Hi"}]
        });

        let request = Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header("Content-Type", "application/json")
            .body(Body::from(serde_json::to_vec(&payload).unwrap()))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // Verify that the requests_total counter was incremented
        let counter_value = metrics
            .requests_total
            .with_label_values(&["/v1/chat/completions", "unknown", "200"])
            .get();
        assert_eq!(
            counter_value, 1,
            "requests_total should be incremented after a successful request"
        );
    }

    // ─── Test: token counting mode metric present from boot ──────────────────

    #[tokio::test]
    async fn test_token_counting_mode_metric_present_from_boot() {
        // Both labels should exist in /metrics output even before any request,
        // so a dashboard can tell the base of token numbers.
        let state = build_test_app_state();
        let app = build_router(state);

        let request = Request::builder()
            .method("GET")
            .uri("/metrics")
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = body_to_string(response.into_body()).await;
        assert!(
            body.contains("melis_token_counting_mode"),
            "metrics should expose melis_token_counting_mode"
        );
        assert!(
            body.contains("mode=\"estimated\""),
            "estimated mode series should be present from boot"
        );
        assert!(
            body.contains("mode=\"real\""),
            "real mode series should be present from boot"
        );
    }

    // ─── Test: default config uses estimated mode (backward-compat parity) ───

    #[tokio::test]
    async fn test_default_config_counts_in_estimated_mode() {
        // The default gateway config (no token_counting field) must count in
        // estimated mode — preserving legacy behavior bit-for-bit.
        //
        // The mode metric is incremented during the compaction stage, which runs
        // before provider selection, so this holds regardless of whether a live
        // provider is reachable (the request may 503 in CI/dev — orthogonal here).
        let state = build_test_app_state();
        let metrics = state.metrics.clone();
        let app = build_router(state);

        let payload = serde_json::json!({
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": "Hello there, how are you today?"}]
        });

        let request = Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header("Content-Type", "application/json")
            .body(Body::from(serde_json::to_vec(&payload).unwrap()))
            .unwrap();
        let _response = app.oneshot(request).await.unwrap();

        let estimated = metrics
            .token_counting_mode
            .with_label_values(&["estimated"])
            .get();
        let real = metrics
            .token_counting_mode
            .with_label_values(&["real"])
            .get();
        assert_eq!(
            estimated, 1,
            "default config should count in estimated mode exactly once"
        );
        assert_eq!(real, 0, "real mode must not be used with default config");
    }

    // ─── Semantic cache: handler fail-open when embeddings unavailable ───────

    /// Builds an AppState whose route config enables the semantic cache on
    /// `/v1/chat/completions`, using a temp routes.yaml. No Ollama provider is
    /// configured, so embedding computation fails and the cache must fail open.
    fn build_cache_enabled_app_state(
        dir: &tempfile::TempDir,
    ) -> (AppState, std::sync::Arc<Metrics>) {
        use crate::route_config::RouteConfigManager;

        let routes_path = dir.path().join("routes.yaml");
        std::fs::write(
            &routes_path,
            r#"
routes:
  - path: "/v1/chat/completions"
    method: "POST"
    provider: "ollama"
    model: "llama3.2"
    semantic_cache:
      enabled: true
      similarity_threshold: 0.92
      ttl_secs: 3600
      max_entries: 1000
      embedding_model: "nomic-embed-text"
"#,
        )
        .unwrap();

        let yaml = r#"
server:
  host: "0.0.0.0"
  port: 8080
redis:
  cluster_urls:
    - "redis://localhost:6379"
auth:
  enabled: false
"#;
        let gateway_config = GatewayConfig::from_yaml(yaml).unwrap();
        let route_config = RouteConfigManager::new(routes_path);
        let metrics = Arc::new(Metrics::new());

        let state = AppState {
            route_config: Arc::new(route_config),
            gateway_config: Arc::new(gateway_config),
            rate_limiter: Arc::new(LocalTokenBucket::new()),
            load_balancer: Arc::new(WeightedRoundRobin::new()),
            circuit_breaker: Arc::new(LocalCircuitBreaker::new(Default::default())),
            http_client: Arc::new(ReqwestLlmClient::new()),
            metrics: metrics.clone(),
            redis_available: Arc::new(AtomicBool::new(true)),
            // No Redis connection in tests → disabled cache. Embeddings explicitly
            // unavailable to exercise the fail-open path.
            semantic_cache: Arc::new(crate::semantic_cache::DisabledCache),
            outbound_guardrail: Arc::new(crate::semantic_cache::NoopOutboundGuardrail),
            guardrails: Arc::new(crate::guardrails::GuardrailEngine::new(
                &crate::route_config::RouteConfigFile {
                    routes: vec![],
                    custom_providers: vec![],
                },
                Default::default(),
            )),
            embeddings: Arc::new(crate::embeddings::StubEmbeddingProvider {
                fixed: None,
                unavailable: true,
            }),
        };
        (state, metrics)
    }

    #[tokio::test]
    async fn test_semantic_cache_fail_open_when_embeddings_unavailable() {
        let dir = tempfile::tempdir().unwrap();
        let (state, metrics) = build_cache_enabled_app_state(&dir);
        let app = build_router(state);

        let payload = serde_json::json!({
            "model": "llama3.2",
            "messages": [{"role": "user", "content": "summarize document X"}]
        });

        let request = Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header("Content-Type", "application/json")
            .body(Body::from(serde_json::to_vec(&payload).unwrap()))
            .unwrap();

        // The request may ultimately 503 (no live provider), but the cache must
        // degrade gracefully first: embeddings unavailable → melis_cache_degraded.
        let _response = app.oneshot(request).await.unwrap();

        let degraded = metrics
            .cache_degraded
            .with_label_values(&["embeddings"])
            .get();
        assert!(
            degraded >= 1,
            "expected melis_cache_degraded{{reason=embeddings}} to increment on fail-open"
        );
    }

    // ─── Test: Readiness state toggle ────────────────────────────────────────

    #[tokio::test]
    async fn test_readyz_reflects_redis_state_changes() {
        let mut state = build_test_app_state();
        require_redis_for_readiness(&mut state);
        let redis_flag = state.redis_available.clone();
        let app = build_router(state);

        // Initially available
        let request = Request::builder()
            .method("GET")
            .uri("/readyz")
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // Toggle to unavailable
        redis_flag.store(false, Ordering::Relaxed);

        let request = Request::builder()
            .method("GET")
            .uri("/readyz")
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    // ─── Embeddings Tests ────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_embeddings_missing_model_returns_400() {
        let state = build_test_app_state();
        let app = build_router(state);

        let payload = serde_json::json!({
            "input": "Hello world"
        });

        let request = Request::builder()
            .method("POST")
            .uri("/v1/embeddings")
            .header("Content-Type", "application/json")
            .body(Body::from(serde_json::to_vec(&payload).unwrap()))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let body = body_to_string(response.into_body()).await;
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert!(json["error"]["message"].as_str().unwrap().contains("model"));
    }

    #[tokio::test]
    async fn test_embeddings_missing_input_returns_400() {
        let state = build_test_app_state();
        let app = build_router(state);

        let payload = serde_json::json!({
            "model": "nomic-embed-text:latest"
        });

        let request = Request::builder()
            .method("POST")
            .uri("/v1/embeddings")
            .header("Content-Type", "application/json")
            .body(Body::from(serde_json::to_vec(&payload).unwrap()))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let body = body_to_string(response.into_body()).await;
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert!(json["error"]["message"].as_str().unwrap().contains("input"));
    }

    #[tokio::test]
    async fn test_embeddings_empty_input_returns_400() {
        let state = build_test_app_state();
        let app = build_router(state);

        let payload = serde_json::json!({
            "model": "nomic-embed-text:latest",
            "input": []
        });

        let request = Request::builder()
            .method("POST")
            .uri("/v1/embeddings")
            .header("Content-Type", "application/json")
            .body(Body::from(serde_json::to_vec(&payload).unwrap()))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_embeddings_route_registered_separately_from_chat() {
        let state = build_test_app_state();
        let app = build_router(state);

        // Verify that /v1/embeddings returns a proper embeddings error (not chat validation error)
        let payload = serde_json::json!({
            "model": "nomic-embed-text:latest",
            "input": "test"
        });

        let request = Request::builder()
            .method("POST")
            .uri("/v1/embeddings")
            .header("Content-Type", "application/json")
            .body(Body::from(serde_json::to_vec(&payload).unwrap()))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        // Should NOT return 400 with "messages" error (that would mean it hit chat handler)
        // It should either succeed (if Ollama available) or return 503 (provider unavailable)
        let status = response.status();
        assert!(
            status == StatusCode::OK
                || status == StatusCode::SERVICE_UNAVAILABLE
                || status == StatusCode::BAD_GATEWAY,
            "Expected 200, 503, or 502 from embeddings handler, got: {}",
            status
        );

        // Verify it's NOT a chat validation error
        if status == StatusCode::BAD_REQUEST {
            let body = body_to_string(response.into_body()).await;
            let json: serde_json::Value = serde_json::from_str(&body).unwrap();
            let msg = json["error"]["message"].as_str().unwrap_or("");
            assert!(
                !msg.contains("messages"),
                "Should not get chat validation error on /v1/embeddings"
            );
        }
    }

    #[tokio::test]
    async fn test_chat_completions_still_works() {
        let state = build_test_app_state();
        let app = build_router(state);

        let payload = serde_json::json!({
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": "Hello!"}]
        });

        let request = Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header("Content-Type", "application/json")
            .body(Body::from(serde_json::to_vec(&payload).unwrap()))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        // Should be 200 (mock/default behavior) or at least not 404
        assert_ne!(response.status(), StatusCode::NOT_FOUND);
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Phase 3: Guardrails integration tests
    // ═══════════════════════════════════════════════════════════════════════

    mod guardrails_it {
        use super::*;
        use crate::guardrails::GuardrailEngine;
        use crate::semantic_cache::{CacheKeyCtx, CacheLookup, SemanticCache};
        use async_trait::async_trait;
        use std::sync::Mutex;

        /// Records lookups/stores; can be primed to return a Hit and capture the
        /// normalized conversation the lookup was computed over (via a shared slot).
        #[derive(Default)]
        struct RecordingCache {
            lookups: Mutex<u32>,
            stores: Mutex<u32>,
            /// When set, `lookup` returns a Hit with this body.
            hit_body: Mutex<Option<serde_json::Value>>,
        }

        #[async_trait]
        impl SemanticCache for RecordingCache {
            async fn lookup(&self, _ctx: &CacheKeyCtx, _embedding: &[f64]) -> CacheLookup {
                *self.lookups.lock().unwrap() += 1;
                if let Some(body) = self.hit_body.lock().unwrap().clone() {
                    return CacheLookup::Hit(crate::semantic_cache::CachedResponse {
                        body,
                        was_streaming: false,
                        stored_at_unix: crate::semantic_cache::now_unix(),
                    });
                }
                CacheLookup::Miss
            }
            async fn store(
                &self,
                _ctx: &CacheKeyCtx,
                _entry_id: &str,
                _embedding: &[f64],
                _response: &serde_json::Value,
                _was_streaming: bool,
            ) {
                *self.stores.lock().unwrap() += 1;
            }
            async fn invalidate_namespace(&self, _ctx: &CacheKeyCtx) {}
        }

        /// Builds an AppState from an inline routes.yaml (written to `dir`), with the
        /// given semantic cache. The guardrail engine is compiled from that config.
        fn build_state(
            dir: &tempfile::TempDir,
            routes_yaml: &str,
            cache: Arc<dyn SemanticCache>,
            embeddings: Arc<dyn crate::embeddings::EmbeddingProvider>,
        ) -> (AppState, Arc<Metrics>) {
            let routes_path = dir.path().join("routes.yaml");
            std::fs::write(&routes_path, routes_yaml).unwrap();

            let gateway_config = GatewayConfig::from_yaml(
                r#"
server:
  host: "0.0.0.0"
  port: 8080
redis:
  cluster_urls:
    - "redis://localhost:6379"
auth:
  enabled: false
"#,
            )
            .unwrap();

            let route_config = RouteConfigManager::new(routes_path);
            let guardrails = {
                let resolver = route_config.current();
                Arc::new(GuardrailEngine::new(
                    resolver.config(),
                    gateway_config.guardrails.clone(),
                ))
            };
            let metrics = Arc::new(Metrics::new());

            let state = AppState {
                route_config: Arc::new(route_config),
                gateway_config: Arc::new(gateway_config),
                rate_limiter: Arc::new(LocalTokenBucket::new()),
                load_balancer: Arc::new(WeightedRoundRobin::new()),
                circuit_breaker: Arc::new(LocalCircuitBreaker::new(Default::default())),
                http_client: Arc::new(ReqwestLlmClient::new()),
                metrics: metrics.clone(),
                redis_available: Arc::new(AtomicBool::new(true)),
                semantic_cache: cache,
                outbound_guardrail: Arc::new(crate::semantic_cache::NoopOutboundGuardrail),
                guardrails,
                embeddings,
            };
            (state, metrics)
        }

        #[tokio::test]
        async fn inbound_block_returns_400_and_never_touches_cache_or_provider() {
            let dir = tempfile::tempdir().unwrap();
            let cache = Arc::new(RecordingCache::default());
            // Route also has semantic cache enabled to prove it is never touched.
            let routes = r#"
routes:
  - path: "/v1/chat/completions"
    method: "POST"
    provider: "mock-primary"
    model: "mock-model"
    semantic_cache:
      enabled: true
    guardrails:
      rules:
        - name: "blockword"
          type: "keyword"
          direction: "inbound"
          action: "block"
          keywords: ["forbidden"]
custom_providers:
  - name: "mock-primary"
    base_url: "http://127.0.0.1:59999/v1"
    api_format: "openai_compatible"
"#;
            let (state, metrics) = build_state(
                &dir,
                routes,
                cache.clone(),
                Arc::new(crate::embeddings::StubEmbeddingProvider::deterministic()),
            );
            let app = build_router(state);

            let payload = serde_json::json!({
                "model": "mock-model",
                "messages": [{"role": "user", "content": "this contains forbidden content"}]
            });
            let request = Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("Content-Type", "application/json")
                .body(Body::from(serde_json::to_vec(&payload).unwrap()))
                .unwrap();
            let response = app.oneshot(request).await.unwrap();

            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
            // Cache must not be consulted or fed on a blocked request.
            assert_eq!(*cache.lookups.lock().unwrap(), 0, "cache was consulted");
            assert_eq!(*cache.stores.lock().unwrap(), 0, "cache was fed");
            // The block action was recorded.
            let blocked = metrics
                .guardrail_actions_total
                .with_label_values(&["blockword", "inbound", "block"])
                .get();
            assert_eq!(blocked, 1);
        }

        // These two were unit-only in Phase 3 because the embedding step gated the
        // cache path. With the injectable `EmbeddingProvider` (Phase 4), a
        // deterministic stub lets them run as real end-to-end HTTP tests.

        #[tokio::test]
        async fn inbound_redact_feeds_cache_key_e2e() {
            // A redact rule rewrites the inbound content; the cache lookup must then
            // run (redact does not block), proving the pipeline reached the cache
            // with the redacted conversation (deterministic stub embedding available).
            let dir = tempfile::tempdir().unwrap();
            let cache = Arc::new(RecordingCache::default()); // Miss → proceeds
            let routes = r#"
routes:
  - path: "/v1/chat/completions"
    method: "POST"
    provider: "mock-primary"
    model: "mock-model"
    semantic_cache:
      enabled: true
    guardrails:
      rules:
        - name: "redactword"
          type: "keyword"
          direction: "inbound"
          action: "redact"
          keywords: ["secret"]
custom_providers:
  - name: "mock-primary"
    base_url: "http://127.0.0.1:59999/v1"
    api_format: "openai_compatible"
"#;
            let (state, metrics) = build_state(
                &dir,
                routes,
                cache.clone(),
                Arc::new(crate::embeddings::StubEmbeddingProvider::deterministic()),
            );
            let app = build_router(state);

            let payload = serde_json::json!({
                "model": "mock-model",
                "messages": [{"role": "user", "content": "my secret token"}]
            });
            let request = Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("Content-Type", "application/json")
                .body(Body::from(serde_json::to_vec(&payload).unwrap()))
                .unwrap();
            let _ = app.oneshot(request).await.unwrap();

            // The redact rule fired and the cache lookup happened (not degraded/blocked).
            let redacted = metrics
                .guardrail_actions_total
                .with_label_values(&["redactword", "inbound", "redact"])
                .get();
            assert_eq!(redacted, 1, "redact rule should have fired");
            assert_eq!(
                *cache.lookups.lock().unwrap(),
                1,
                "cache lookup should run over the redacted conversation"
            );
        }

        #[tokio::test]
        async fn cache_hit_passes_through_outbound_guardrail_e2e() {
            // Prime the cache to return a hit whose content violates an outbound
            // block rule. The hit must be filtered on delivery → HTTP 403.
            let dir = tempfile::tempdir().unwrap();
            let cache = Arc::new(RecordingCache::default());
            *cache.hit_body.lock().unwrap() = Some(serde_json::json!({
                "id": "chatcmpl-cached",
                "object": "chat.completion",
                "model": "mock-model",
                "choices": [{
                    "index": 0,
                    "message": {"role": "assistant", "content": "here is a leaked secret"},
                    "finish_reason": "stop"
                }]
            }));
            let routes = r#"
routes:
  - path: "/v1/chat/completions"
    method: "POST"
    provider: "mock-primary"
    model: "mock-model"
    semantic_cache:
      enabled: true
    guardrails:
      rules:
        - name: "out-block"
          type: "keyword"
          direction: "outbound"
          action: "block"
          keywords: ["leaked secret"]
custom_providers:
  - name: "mock-primary"
    base_url: "http://127.0.0.1:59999/v1"
    api_format: "openai_compatible"
"#;
            let (state, metrics) = build_state(
                &dir,
                routes,
                cache.clone(),
                Arc::new(crate::embeddings::StubEmbeddingProvider::deterministic()),
            );
            let app = build_router(state);

            let payload = serde_json::json!({
                "model": "mock-model",
                "messages": [{"role": "user", "content": "give me the data"}]
            });
            let request = Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("Content-Type", "application/json")
                .body(Body::from(serde_json::to_vec(&payload).unwrap()))
                .unwrap();
            let response = app.oneshot(request).await.unwrap();

            // Cache hit was consulted and the outbound guardrail blocked delivery (403).
            assert_eq!(
                *cache.lookups.lock().unwrap(),
                1,
                "cache should be consulted"
            );
            assert_eq!(response.status(), StatusCode::FORBIDDEN);
            let blocked = metrics
                .guardrail_actions_total
                .with_label_values(&["out-block", "outbound", "block"])
                .get();
            assert_eq!(blocked, 1, "outbound guardrail should block the cached hit");
        }
    }
}
