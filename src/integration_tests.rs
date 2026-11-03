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
            // No Redis connection in tests → disabled cache; embeddings also absent.
            semantic_cache: Arc::new(crate::semantic_cache::DisabledCache),
            outbound_guardrail: Arc::new(crate::semantic_cache::NoopOutboundGuardrail),
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
        let state = build_test_app_state();
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
}
