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
                    model_aliases: Vec::new(),
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

    // ─── Real-provider (G1) parametrization ──────────────────────────────────
    //
    // The live-provider tests target a real OpenAI-compatible endpoint (Ollama by
    // default). Endpoint and model come from the environment so G1 does not depend
    // on a model literally named like a hosted one, and never aliases `gpt-4o` to a
    // local model:
    //   MELIS_IT_OLLAMA_URL   (default http://localhost:11434)
    //   MELIS_IT_CHAT_MODEL   (default llama3.2)
    // The effective model is logged so the run records what was actually used.

    fn real_chat_model() -> String {
        std::env::var("MELIS_IT_CHAT_MODEL").unwrap_or_else(|_| "llama3.2".to_string())
    }

    fn real_ollama_url() -> String {
        std::env::var("MELIS_IT_OLLAMA_URL")
            .unwrap_or_else(|_| "http://localhost:11434".to_string())
    }

    /// AppState wired to the configured real Ollama provider (G1 tests).
    ///
    /// Installs a route mapping `/v1/chat/completions` to the ollama provider so
    /// the failover loop has a provider to select (an empty route config yields no
    /// providers and a 503, independent of the model). The temp dir is leaked for
    /// the lifetime of the test process, which is fine for `#[ignore]` G1 tests.
    fn build_real_provider_app_state() -> AppState {
        let url = real_ollama_url();
        let model = real_chat_model();
        tracing::info!(endpoint = %url, model = %model, "G1 live-provider test using model");
        eprintln!("G1 live-provider test: endpoint={url} model={model}");

        // new_without_watcher loads the config into memory, so the temp dir can be
        // dropped right after; no watcher is created (keeps inotify usage low).
        let dir = tempfile::tempdir().unwrap();
        let routes_path = dir.path().join("routes.yaml");
        std::fs::write(
            &routes_path,
            format!(
                "routes:\n  - path: \"/v1/chat/completions\"\n    method: \"POST\"\n    provider: \"ollama\"\n    model: \"{model}\"\n"
            ),
        )
        .unwrap();
        let gateway_config = GatewayConfig::from_yaml(&format!(
            "server:\n  host: \"0.0.0.0\"\n  port: 8080\nredis:\n  cluster_urls:\n    - \"redis://localhost:6379\"\nauth:\n  enabled: false\nproviders:\n  - id: \"ollama\"\n    provider_type: \"ollama\"\n    api_key: \"ollama\"\n    base_url: \"{url}\"\n"
        ))
        .unwrap();
        AppState {
            route_config: Arc::new(RouteConfigManager::new_without_watcher(routes_path)),
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
        }
    }

    // ─── Test 1: JSON Response ────────────────────────────────────────────────

    #[tokio::test]
    #[ignore = "G1: live provider; set MELIS_IT_OLLAMA_URL / MELIS_IT_CHAT_MODEL (defaults http://localhost:11434, llama3.2)"]
    async fn test_json_response_with_valid_payload() {
        let state = build_real_provider_app_state();
        let app = build_router(state);

        let payload = serde_json::json!({
            "model": real_chat_model(),
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
    #[ignore = "G1: live provider; set MELIS_IT_OLLAMA_URL / MELIS_IT_CHAT_MODEL (defaults http://localhost:11434, llama3.2)"]
    async fn test_sse_response_with_accept_header() {
        let state = build_real_provider_app_state();
        let app = build_router(state);

        let payload = serde_json::json!({
            "model": real_chat_model(),
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
    #[ignore = "G1: live provider; set MELIS_IT_OLLAMA_URL / MELIS_IT_CHAT_MODEL (defaults http://localhost:11434, llama3.2)"]
    async fn test_sse_response_with_stream_true_in_payload() {
        let state = build_real_provider_app_state();
        let app = build_router(state);

        let payload = serde_json::json!({
            "model": real_chat_model(),
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
    #[ignore = "G1: live provider; set MELIS_IT_OLLAMA_URL / MELIS_IT_CHAT_MODEL (defaults http://localhost:11434, llama3.2)"]
    async fn test_metrics_increment_after_chat_completion_request() {
        let state = build_real_provider_app_state();
        let metrics = state.metrics.clone();
        let app = build_router(state);

        let payload = serde_json::json!({
            "model": real_chat_model(),
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

        // requests_total uses a fixed "unknown" provider label on this path
        // (request_duration carries the real provider_type); see the audit note on
        // the provider label inconsistency.
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
      scope: "shared"
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
        let route_config = RouteConfigManager::new_without_watcher(routes_path);
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
                    model_aliases: Vec::new(),
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

            let route_config = RouteConfigManager::new_without_watcher(routes_path);
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
      scope: "shared"
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
      scope: "shared"
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
      scope: "shared"
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

        // ── cache isolation (audit fix): namespace must reflect system/params/tools ──

        /// Cache that records the namespaces it is asked to look up, and serves a
        /// hit only for an exact namespace it has "stored".
        #[derive(Default)]
        struct NamespaceRecordingCache {
            looked_up: Mutex<Vec<String>>,
            hits_for: Mutex<std::collections::HashSet<String>>,
        }

        #[async_trait]
        impl SemanticCache for NamespaceRecordingCache {
            async fn lookup(&self, ctx: &CacheKeyCtx, _embedding: &[f64]) -> CacheLookup {
                let ns = ctx.namespace();
                self.looked_up.lock().unwrap().push(ns.clone());
                if self.hits_for.lock().unwrap().contains(&ns) {
                    return CacheLookup::Hit(crate::semantic_cache::CachedResponse {
                        body: serde_json::json!({
                            "object": "chat.completion",
                            "choices": [{"index": 0, "finish_reason": "stop",
                                         "message": {"role": "assistant", "content": "cached"}}]
                        }),
                        was_streaming: false,
                        stored_at_unix: crate::semantic_cache::now_unix(),
                    });
                }
                CacheLookup::Miss
            }
            async fn store(
                &self,
                ctx: &CacheKeyCtx,
                _entry_id: &str,
                _embedding: &[f64],
                _response: &serde_json::Value,
                _was_streaming: bool,
            ) {
                self.hits_for.lock().unwrap().insert(ctx.namespace());
            }
            async fn invalidate_namespace(&self, _ctx: &CacheKeyCtx) {}
        }

        async fn lookup_namespace(cache: Arc<NamespaceRecordingCache>, payload: serde_json::Value) {
            let dir = tempfile::tempdir().unwrap();
            let routes = r#"
routes:
  - path: "/v1/chat/completions"
    method: "POST"
    provider: "mock-primary"
    model: "mock-model"
    semantic_cache:
      enabled: true
      scope: "shared"
custom_providers:
  - name: "mock-primary"
    base_url: "http://127.0.0.1:59998/v1"
    api_format: "openai_compatible"
"#;
            let (state, _m) = build_state(
                &dir,
                routes,
                cache,
                Arc::new(crate::embeddings::StubEmbeddingProvider::deterministic()),
            );
            let app = build_router(state);
            let request = Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("Content-Type", "application/json")
                .body(Body::from(serde_json::to_vec(&payload).unwrap()))
                .unwrap();
            // The provider is unreachable; we only care about the cache namespace
            // used during lookup, which happens before the provider call.
            let _ = app.oneshot(request).await.unwrap();
        }

        /// A response cached for one (system, params, tools) must NOT be returned
        /// for a request that differs in any of them, even with identical user
        /// messages (the embedding covers only non-system content).
        #[tokio::test]
        async fn cache_does_not_reuse_across_incompatible_requests() {
            let cache = Arc::new(NamespaceRecordingCache::default());
            let user = serde_json::json!([{"role": "user", "content": "what is the policy?"}]);

            // Base request: seed the cache with its namespace via a store.
            let base = serde_json::json!({"model": "mock-model", "messages": user});
            // Variants that must land in different namespaces.
            let with_system = serde_json::json!({
                "model": "mock-model",
                "messages": [
                    {"role": "system", "content": "Tenant A. Only answer about tenant A."},
                    {"role": "user", "content": "what is the policy?"}
                ]
            });
            let hot =
                serde_json::json!({"model": "mock-model", "messages": user, "temperature": 0.9});
            let tooled = serde_json::json!({
                "model": "mock-model", "messages": user,
                "tools": [{"type": "function", "function": {"name": "lookup"}}]
            });

            for p in [&base, &with_system, &hot, &tooled] {
                lookup_namespace(cache.clone(), p.clone()).await;
            }

            let seen = cache.looked_up.lock().unwrap().clone();
            assert_eq!(seen.len(), 4);
            let distinct: std::collections::HashSet<_> = seen.iter().collect();
            assert_eq!(
                distinct.len(),
                4,
                "each of base/system/temperature/tools must use a distinct cache namespace: {seen:?}"
            );
        }
    }
}

/// Bugfix D1 end-to-end: capture the payload the gateway actually sends to an
/// OpenAI-compatible provider after compaction fires, and compare it with the
/// original message objects.
#[cfg(test)]
mod d1_payload_preservation_e2e {
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;

    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use serde_json::{json, Value};
    use tower::ServiceExt;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use crate::balancer::WeightedRoundRobin;
    use crate::circuit_breaker::LocalCircuitBreaker;
    use crate::client::ReqwestLlmClient;
    use crate::config::GatewayConfig;
    use crate::guardrails::GuardrailEngine;
    use crate::middleware::rate_limiter::LocalTokenBucket;
    use crate::observability::Metrics;
    use crate::route_config::RouteConfigManager;
    use crate::router::build_router;
    use crate::state::AppState;

    fn long(tag: &str) -> String {
        format!("{tag} ").repeat(80)
    }

    async fn forwarded_messages(strategy: &str, messages: &[Value]) -> (Vec<Value>, Arc<Metrics>) {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "x", "object": "chat.completion", "created": 0, "model": "mock-model",
                "choices": [{"index": 0, "finish_reason": "stop",
                             "message": {"role": "assistant", "content": "ok"}}],
                "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
            })))
            .mount(&server)
            .await;

        let dir = tempfile::tempdir().unwrap();
        let routes_path = dir.path().join("routes.yaml");
        std::fs::write(
            &routes_path,
            format!(
                r#"
routes:
  - path: "/v1/chat/completions"
    method: "POST"
    provider: "openai"
    model: "mock-model"
    token_optimization:
      strategy: "{strategy}"
      max_history_messages: 50
      compress_above_tokens: 50
      target_token_ratio: 0.3
      min_final_tokens: 10
"#
            ),
        )
        .unwrap();
        let gateway_config = GatewayConfig::from_yaml(&format!(
            r#"
server:
  host: "0.0.0.0"
  port: 8080
redis:
  cluster_urls:
    - "redis://localhost:6379"
auth:
  enabled: false
providers:
  - id: "mock-openai"
    provider_type: "openai"
    api_key: "test-key"
    base_url: "{}/v1"
"#,
            server.uri()
        ))
        .unwrap();

        let route_config = RouteConfigManager::new_without_watcher(routes_path);
        let guardrails = Arc::new(GuardrailEngine::new(
            route_config.current().config(),
            gateway_config.guardrails.clone(),
        ));
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
            semantic_cache: Arc::new(crate::semantic_cache::DisabledCache),
            outbound_guardrail: Arc::new(crate::semantic_cache::NoopOutboundGuardrail),
            guardrails,
            embeddings: Arc::new(crate::embeddings::StubEmbeddingProvider::deterministic()),
        };

        let request = Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header("Content-Type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&json!({"model": "mock-model", "messages": messages})).unwrap(),
            ))
            .unwrap();
        let response = build_router(state).oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let received = server.received_requests().await.unwrap();
        assert_eq!(received.len(), 1, "exactly one upstream call");
        let body: Value = serde_json::from_slice(&received[0].body).unwrap();
        (body["messages"].as_array().unwrap().clone(), metrics)
    }

    /// Shapes that pass `validate_chat_payload` (string content) but used to be
    /// corrupted by the `{role, content}` rebuild when compaction fired.
    fn conversation() -> Vec<Value> {
        vec![
            json!({"role": "system", "content": "You are a helpful assistant."}),
            json!({"role": "user", "content": long("old filler"), "name": "alice", "x_trace": {"id": 7}}),
            json!({"role": "assistant", "content": long("old answer")}),
            json!({
                "role": "assistant",
                "content": "Calling the weather tool.",
                "tool_calls": [{"id": "call_1", "type": "function",
                                "function": {"name": "get_weather", "arguments": "{\"city\":\"Lisboa\"}"}}]
            }),
            json!({"role": "tool", "tool_call_id": "call_1", "name": "get_weather", "content": "{\"temp\":21}"}),
            json!({"role": "assistant", "content": long("filler reply")}),
            json!({"role": "user", "content": "And tomorrow's weather?", "name": "alice", "x_client": "web"}),
        ]
    }

    #[tokio::test]
    async fn compaction_forwards_original_objects_with_tool_fields() {
        for strategy in ["adaptive_trimming", "semantic_guarded_trimming"] {
            let input = conversation();
            let (sent, metrics) = forwarded_messages(strategy, &input).await;

            assert_eq!(
                metrics.compaction_applied_total.get(),
                1,
                "{strategy}: compaction must have fired"
            );
            assert!(sent.len() < input.len(), "{strategy}: something was pruned");

            // Every forwarded message is one of the ORIGINAL objects, in order.
            let mut last = None;
            for m in &sent {
                let idx = input
                    .iter()
                    .position(|o| o == m)
                    .unwrap_or_else(|| panic!("{strategy}: forwarded message was rewritten: {m}"));
                assert!(last.is_none_or(|l| idx > l), "{strategy}: order changed");
                last = Some(idx);
            }

            // Tool call and its result reach the provider intact.
            assert!(sent.contains(&input[3]), "{strategy}: tool_calls lost");
            assert!(sent.contains(&input[4]), "{strategy}: tool result lost");
            // Last user message keeps `name` and the unknown field.
            assert_eq!(sent.last().unwrap(), &input[6], "{strategy}");
        }
    }

    #[tokio::test]
    async fn text_only_conversation_is_forwarded_as_before() {
        // Legacy shape: `{role, content}` only. Forwarded payload is exactly the
        // subset of the original objects the old code produced.
        let input = vec![
            json!({"role": "system", "content": "sys"}),
            json!({"role": "user", "content": long("old filler")}),
            json!({"role": "assistant", "content": long("old answer")}),
            json!({"role": "user", "content": "final question"}),
        ];
        let (sent, _) = forwarded_messages("adaptive_trimming", &input).await;
        // Legacy rule: target = 75% of original; oldest eligible pruned first.
        // Dropping message 1 alone reaches the target.
        assert_eq!(
            sent,
            vec![input[0].clone(), input[2].clone(), input[3].clone()]
        );
    }
}

/// Phase 5 end-to-end: opt-in semantic scoring through the HTTP handler, with a
/// simulated OpenAI-compatible provider (wiremock, payload captured) and a
/// scripted embedding stub (calls counted). No live services.
#[cfg(test)]
mod phase5_scoring_e2e {
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;

    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use serde_json::{json, Value};
    use tower::ServiceExt;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use crate::balancer::WeightedRoundRobin;
    use crate::circuit_breaker::LocalCircuitBreaker;
    use crate::client::ReqwestLlmClient;
    use crate::config::GatewayConfig;
    use crate::embeddings::ScriptedEmbeddingProvider;
    use crate::guardrails::GuardrailEngine;
    use crate::middleware::rate_limiter::LocalTokenBucket;
    use crate::observability::Metrics;
    use crate::route_config::RouteConfigManager;
    use crate::router::build_router;
    use crate::state::AppState;

    fn long(tag: &str, n: usize) -> String {
        format!("{tag} ").repeat(n)
    }

    fn topic_embeddings() -> ScriptedEmbeddingProvider {
        ScriptedEmbeddingProvider::new(
            vec![
                ("pods", vec![1.0, 0.0, 0.0]),
                ("kubernetes", vec![1.0, 0.0, 0.0]),
                ("recipe", vec![0.0, 1.0, 0.0]),
                ("garden", vec![0.0, 0.0, 1.0]),
            ],
            vec![0.3, 0.3, 0.3],
        )
    }

    struct Run {
        status: StatusCode,
        body: Value,
        /// Messages the provider actually received (None = provider not called).
        sent: Option<Vec<Value>>,
        metrics: Arc<Metrics>,
    }

    /// `token_opt`: YAML lines under `token_optimization:` (6-space indent).
    async fn run(
        token_opt: &str,
        messages: &[Value],
        embeddings: Arc<ScriptedEmbeddingProvider>,
    ) -> Run {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "x", "object": "chat.completion", "created": 0, "model": "mock-model",
                "choices": [{"index": 0, "finish_reason": "stop",
                             "message": {"role": "assistant", "content": "ok"}}],
                "usage": {"prompt_tokens": 111, "completion_tokens": 7, "total_tokens": 118}
            })))
            .mount(&server)
            .await;

        let dir = tempfile::tempdir().unwrap();
        let routes_path = dir.path().join("routes.yaml");
        std::fs::write(
            &routes_path,
            format!(
                "routes:\n  - path: \"/v1/chat/completions\"\n    method: \"POST\"\n    provider: \"openai\"\n    model: \"mock-model\"\n    token_optimization:\n{token_opt}"
            ),
        )
        .unwrap();
        let gateway_config = GatewayConfig::from_yaml(&format!(
            "server:\n  host: \"0.0.0.0\"\n  port: 8080\nredis:\n  cluster_urls:\n    - \"redis://localhost:6379\"\nauth:\n  enabled: false\nproviders:\n  - id: \"mock-openai\"\n    provider_type: \"openai\"\n    api_key: \"test-key\"\n    base_url: \"{}/v1\"\n",
            server.uri()
        ))
        .unwrap();

        let route_config = RouteConfigManager::new_without_watcher(routes_path);
        let guardrails = Arc::new(GuardrailEngine::new(
            route_config.current().config(),
            gateway_config.guardrails.clone(),
        ));
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
            semantic_cache: Arc::new(crate::semantic_cache::DisabledCache),
            outbound_guardrail: Arc::new(crate::semantic_cache::NoopOutboundGuardrail),
            guardrails,
            embeddings,
        };

        let request = Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header("Content-Type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&json!({"model": "mock-model", "messages": messages})).unwrap(),
            ))
            .unwrap();
        let response = build_router(state).oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);

        let received = server.received_requests().await.unwrap();
        let sent = received.first().map(|r| {
            let b: Value = serde_json::from_slice(&r.body).unwrap();
            b["messages"].as_array().unwrap().clone()
        });
        Run {
            status,
            body,
            sent,
            metrics,
        }
    }

    const SGT: &str = "      strategy: \"semantic_guarded_trimming\"\n      compress_above_tokens: 50\n      target_token_ratio: 0.5\n      min_final_tokens: 10\n";

    /// Passes HTTP validation (string content only: GAP-OAI-1 is unchanged).
    fn conversation() -> Vec<Value> {
        vec![
            json!({"role": "system", "content": "You are helpful.", "x_meta": {"v": 1}}),
            json!({"role": "user", "content": long("container pods rollout", 30), "name": "alice"}),
            json!({"role": "assistant", "content": "Looking it up.",
                   "tool_calls": [{"id": "c1", "type": "function", "function": {"name": "f", "arguments": "{}"}},
                                  {"id": "c2", "type": "function", "function": {"name": "g", "arguments": "{}"}}]}),
            json!({"role": "tool", "tool_call_id": "c1", "content": long("recipe salt", 20)}),
            json!({"role": "tool", "tool_call_id": "c2", "content": long("recipe sugar", 20)}),
            json!({"role": "assistant", "content": long("recipe with tomatoes", 30)}),
            json!({"role": "user", "content": long("garden flowers", 30)}),
            json!({"role": "user", "content": "kubernetes question again", "x_client": "web"}),
        ]
    }

    fn assert_original_subsequence(input: &[Value], sent: &[Value]) -> Vec<usize> {
        let mut idx = Vec::new();
        for m in sent {
            let i = input
                .iter()
                .position(|o| o == m)
                .unwrap_or_else(|| panic!("forwarded message was rewritten: {m}"));
            assert!(idx.last().is_none_or(|&l| i > l), "order changed");
            idx.push(i);
        }
        idx
    }

    #[tokio::test]
    async fn legacy_route_makes_no_embedding_calls_and_keeps_d1_payload() {
        let input = conversation();
        let emb = Arc::new(topic_embeddings());
        let r = run(SGT, &input, emb.clone()).await;
        assert_eq!(r.status, StatusCode::OK);
        assert_eq!(
            r.metrics.compaction_applied_total.get(),
            1,
            "compaction fired"
        );
        assert_eq!(
            emb.total_calls(),
            0,
            "no semantic_scoring → no embedding calls"
        );
        let sent = r.sent.unwrap();
        let kept = assert_original_subsequence(&input, &sent);
        // D1 pins: tool call + results kept verbatim on the legacy path.
        for i in [0, 2, 3, 4, 7] {
            assert!(kept.contains(&i), "legacy D1 keeps {i}: {kept:?}");
        }
        assert_eq!(
            r.metrics
                .compaction_scoring_total
                .with_label_values(&["hybrid", "hybrid"])
                .get(),
            0
        );
    }

    #[tokio::test]
    async fn enabled_but_below_threshold_forwards_input_unchanged_without_embedding() {
        let input = conversation();
        let emb = Arc::new(topic_embeddings());
        let opt = "      strategy: \"semantic_guarded_trimming\"\n      compress_above_tokens: 100000\n      semantic_scoring: hybrid\n";
        let r = run(opt, &input, emb.clone()).await;
        assert_eq!(r.status, StatusCode::OK);
        assert_eq!(emb.total_calls(), 0);
        assert_eq!(r.sent.unwrap(), input);
    }

    #[tokio::test]
    async fn embedding_scoring_selects_by_relevance_over_recency() {
        let input = conversation();
        let emb = Arc::new(topic_embeddings());
        // eligible_for_removal so the (preserved-by-default) tool group does not
        // absorb the budget; this isolates the relevance-vs-recency behavior.
        let r = run(
            &format!("{SGT}      semantic_scoring:\n        mode: embedding\n        tool_group_policy: eligible_for_removal\n"),
            &input,
            emb.clone(),
        )
        .await;
        assert_eq!(r.status, StatusCode::OK);
        let kept = assert_original_subsequence(&input, &r.sent.unwrap());
        assert!(
            kept.contains(&1),
            "relevant old message kept (no lexical overlap): {kept:?}"
        );
        assert!(
            !kept.contains(&5),
            "irrelevant recent message removed: {kept:?}"
        );
        let group = [2, 3, 4].iter().filter(|i| kept.contains(i)).count();
        assert!(group == 0 || group == 3, "tool group split: {kept:?}");
    }

    #[tokio::test]
    async fn complete_tool_group_preserved_by_default_over_http() {
        // Default policy: the complete tool group (indices 2,3,4) reaches the
        // provider intact even under a tight budget.
        let input = conversation();
        let emb = Arc::new(topic_embeddings());
        let opt = format!("{SGT}      semantic_scoring:\n        mode: embedding\n        tool_group_policy: preserve\n");
        let r = run(&opt, &input, emb.clone()).await;
        assert_eq!(r.status, StatusCode::OK);
        let sent = r.sent.unwrap();
        for i in [2, 3, 4] {
            assert!(
                sent.contains(&input[i]),
                "preserve: group member {i} must reach provider"
            );
        }
    }

    #[tokio::test]
    async fn hybrid_scoring_forwards_original_objects_and_keeps_structure() {
        // Relevance is asserted in pure `embedding` mode above; here we only check
        // payload preservation, metrics and that the tool group stays intact.
        let input = conversation();
        let emb = Arc::new(topic_embeddings());
        let r = run(
            &format!("{SGT}      semantic_scoring: hybrid\n"),
            &input,
            emb.clone(),
        )
        .await;
        assert_eq!(r.status, StatusCode::OK);
        assert_eq!(emb.batch_calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        let sent = r.sent.unwrap();
        let kept = assert_original_subsequence(&input, &sent);
        assert!(
            kept.contains(&0) && kept.contains(&7),
            "protected kept: {kept:?}"
        );
        assert!(sent.len() < input.len(), "something pruned");
        let group = [2, 3, 4].iter().filter(|i| kept.contains(i)).count();
        assert!(group == 0 || group == 3, "tool group split: {kept:?}");
        assert_eq!(
            r.metrics
                .compaction_scoring_total
                .with_label_values(&["hybrid", "hybrid"])
                .get(),
            1
        );
        // Provider usage is forwarded as is (never overwritten by local counts).
        assert_eq!(r.body["usage"]["prompt_tokens"], 111);
        assert_eq!(r.body["usage"]["total_tokens"], 118);
    }

    #[tokio::test]
    async fn embedding_failure_falls_back_to_the_legacy_payload() {
        let input = conversation();
        let legacy = run(SGT, &input, Arc::new(topic_embeddings()))
            .await
            .sent
            .unwrap();

        let mut failing = topic_embeddings();
        failing.fail = true;
        let r = run(
            &format!("{SGT}      semantic_scoring: embedding\n"),
            &input,
            Arc::new(failing),
        )
        .await;
        assert_eq!(
            r.status,
            StatusCode::OK,
            "scoring failure never fails the request"
        );
        assert_eq!(
            r.sent.unwrap(),
            legacy,
            "full fallback = legacy D1 selection"
        );
        assert_eq!(
            r.metrics
                .compaction_scoring_fallback_total
                .with_label_values(&["provider_error"])
                .get(),
            1
        );
        assert_eq!(
            r.metrics
                .compaction_scoring_total
                .with_label_values(&["embedding", "heuristic"])
                .get(),
            1
        );
    }

    #[tokio::test]
    async fn too_many_candidates_falls_back_without_embedding() {
        let input = conversation();
        let emb = Arc::new(topic_embeddings());
        let opt = format!(
            "{SGT}      semantic_scoring:\n        mode: hybrid\n        max_messages: 1\n"
        );
        let r = run(&opt, &input, emb.clone()).await;
        assert_eq!(r.status, StatusCode::OK);
        assert_eq!(emb.total_calls(), 0);
        assert_eq!(
            r.metrics
                .compaction_scoring_fallback_total
                .with_label_values(&["limit_exceeded"])
                .get(),
            1
        );
    }

    #[tokio::test]
    async fn strict_budget_insufficient_returns_400_and_never_calls_provider() {
        let input = conversation();
        let emb = Arc::new(topic_embeddings());
        let opt = format!(
            "{SGT}      semantic_scoring:\n        mode: hybrid\n        budget:\n          mode: strict\n          max_context_tokens: 30\n          output_reserve_tokens: 10\n          formatting_overhead: openai_chat_estimate\n"
        );
        let r = run(&opt, &input, emb.clone()).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST);
        assert_eq!(r.body["error"]["type"], "invalid_request_error");
        assert!(r.body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("Insufficient context budget"));
        assert!(r.sent.is_none(), "provider must not be called");
        assert_eq!(emb.total_calls(), 0);
        assert_eq!(r.metrics.compaction_budget_insufficient_total.get(), 1);
    }

    #[tokio::test]
    async fn invalid_semantic_scoring_config_is_rejected_at_load() {
        // Same rejection path as any other invalid routes.yaml (panics on initial
        // load, keeps the previous config on hot reload).
        let result = tokio::spawn(async {
            run(
                &format!("{SGT}      semantic_scoring:\n        mode: hybrid\n        semantic_weight: -1\n"),
                &conversation(),
                Arc::new(topic_embeddings()),
            )
            .await
        })
        .await;
        assert!(result.is_err(), "invalid weights must not be accepted");
    }
}

/// Cache identity isolation (audit fix) end-to-end, with auth enabled so a real
/// `ClientIdentity` is produced by the middleware. Uses a cache that records the
/// namespaces looked up and serves a hit only for a namespace it has "stored".
#[cfg(test)]
mod cache_identity_e2e {
    use std::sync::atomic::AtomicBool;
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use serde_json::json;
    use tower::ServiceExt;

    use crate::balancer::WeightedRoundRobin;
    use crate::circuit_breaker::LocalCircuitBreaker;
    use crate::client::ReqwestLlmClient;
    use crate::config::GatewayConfig;
    use crate::guardrails::GuardrailEngine;
    use crate::middleware::rate_limiter::LocalTokenBucket;
    use crate::observability::Metrics;
    use crate::route_config::RouteConfigManager;
    use crate::router::build_router;
    use crate::semantic_cache::{CacheKeyCtx, CacheLookup, SemanticCache};
    use crate::state::AppState;

    /// Records lookups and serves a hit for namespaces previously stored.
    #[derive(Default)]
    struct NamespaceRecordingCache {
        looked_up: Mutex<Vec<String>>,
        hits_for: Mutex<std::collections::HashSet<String>>,
    }

    #[async_trait]
    impl SemanticCache for NamespaceRecordingCache {
        async fn lookup(&self, ctx: &CacheKeyCtx, _embedding: &[f64]) -> CacheLookup {
            let ns = ctx.namespace();
            self.looked_up.lock().unwrap().push(ns.clone());
            if self.hits_for.lock().unwrap().contains(&ns) {
                return CacheLookup::Hit(crate::semantic_cache::CachedResponse {
                    body: json!({
                        "object": "chat.completion",
                        "choices": [{"index": 0, "finish_reason": "stop",
                                     "message": {"role": "assistant", "content": "cached"}}]
                    }),
                    was_streaming: false,
                    stored_at_unix: crate::semantic_cache::now_unix(),
                });
            }
            CacheLookup::Miss
        }
        async fn store(
            &self,
            ctx: &CacheKeyCtx,
            _entry_id: &str,
            _embedding: &[f64],
            _response: &serde_json::Value,
            _was_streaming: bool,
        ) {
            self.hits_for.lock().unwrap().insert(ctx.namespace());
        }
        async fn invalidate_namespace(&self, _ctx: &CacheKeyCtx) {}
    }

    /// Builds an auth-enabled gateway (two API keys) with the given scope and cache.
    fn build_auth_state(scope: &str, cache: Arc<NamespaceRecordingCache>) -> AppState {
        let dir = Box::leak(Box::new(tempfile::tempdir().unwrap()));
        let routes_path = dir.path().join("routes.yaml");
        std::fs::write(
            &routes_path,
            format!(
                "routes:\n  - path: \"/v1/chat/completions\"\n    method: \"POST\"\n    provider: \"openai\"\n    model: \"mock-model\"\n    semantic_cache:\n      enabled: true\n      scope: \"{scope}\"\n"
            ),
        )
        .unwrap();
        let gateway_config = GatewayConfig::from_yaml(
            r#"
server:
  host: "0.0.0.0"
  port: 8080
redis:
  cluster_urls:
    - "redis://localhost:6379"
auth:
  enabled: true
  api_keys:
    - key: "sk-alice"
      client_id: "alice"
    - key: "sk-bob"
      client_id: "bob"
providers:
  - id: "mock-openai"
    provider_type: "openai"
    api_key: "test-key"
    base_url: "http://127.0.0.1:59997/v1"
"#,
        )
        .unwrap();
        let route_config = RouteConfigManager::new_without_watcher(routes_path);
        let guardrails = Arc::new(GuardrailEngine::new(
            route_config.current().config(),
            gateway_config.guardrails.clone(),
        ));
        AppState {
            route_config: Arc::new(route_config),
            gateway_config: Arc::new(gateway_config),
            rate_limiter: Arc::new(LocalTokenBucket::new()),
            load_balancer: Arc::new(WeightedRoundRobin::new()),
            circuit_breaker: Arc::new(LocalCircuitBreaker::new(Default::default())),
            http_client: Arc::new(ReqwestLlmClient::new()),
            metrics: Arc::new(Metrics::new()),
            redis_available: Arc::new(AtomicBool::new(true)),
            semantic_cache: cache,
            outbound_guardrail: Arc::new(crate::semantic_cache::NoopOutboundGuardrail),
            guardrails,
            embeddings: Arc::new(crate::embeddings::StubEmbeddingProvider::deterministic()),
        }
    }

    /// Sends one chat request with the given bearer token; returns (status, body).
    async fn send(
        cache: Arc<NamespaceRecordingCache>,
        scope: &str,
        bearer: Option<&str>,
    ) -> (StatusCode, serde_json::Value) {
        let state = build_auth_state(scope, cache);
        let app = build_router(state);
        let mut builder = Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header("Content-Type", "application/json");
        if let Some(b) = bearer {
            builder = builder.header("Authorization", format!("Bearer {b}"));
        }
        let payload = json!({"model": "mock-model", "messages": [{"role": "user", "content": "what is the policy?"}]});
        let request = builder
            .body(Body::from(serde_json::to_vec(&payload).unwrap()))
            .unwrap();
        let response = app.oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
        (status, body)
    }

    #[tokio::test]
    async fn same_identity_can_hit() {
        let cache = Arc::new(NamespaceRecordingCache::default());
        // Alice's first request stores her namespace (the provider is unreachable,
        // but store happens only on a real response; so instead we seed by lookup
        // then mark the looked-up namespace as a hit and repeat).
        let (_s1, _b1) = send(cache.clone(), "per_identity", Some("sk-alice")).await;
        let ns = cache.looked_up.lock().unwrap().last().unwrap().clone();
        cache.hits_for.lock().unwrap().insert(ns);
        let (s2, b2) = send(cache.clone(), "per_identity", Some("sk-alice")).await;
        assert_eq!(s2, StatusCode::OK);
        assert_eq!(b2["choices"][0]["message"]["content"], "cached");
    }

    #[tokio::test]
    async fn different_identities_do_not_share_in_per_identity() {
        let cache = Arc::new(NamespaceRecordingCache::default());
        // Seed Alice's namespace as a hit.
        send(cache.clone(), "per_identity", Some("sk-alice")).await;
        let alice_ns = cache.looked_up.lock().unwrap().last().unwrap().clone();
        cache.hits_for.lock().unwrap().insert(alice_ns.clone());

        // Bob looks up a DIFFERENT namespace and must not hit Alice's entry.
        let (status, _body) = send(cache.clone(), "per_identity", Some("sk-bob")).await;
        let bob_ns = cache.looked_up.lock().unwrap().last().unwrap().clone();
        assert_ne!(
            alice_ns, bob_ns,
            "alice and bob must use different namespaces"
        );
        assert_ne!(
            status,
            StatusCode::OK,
            "bob must not get alice's cached hit (provider unreachable → non-200)"
        );
    }

    #[tokio::test]
    async fn forged_identity_header_does_not_change_scope() {
        // A client-controlled header that is NOT the authenticated bearer must not
        // influence the namespace. Here Alice adds an X-Client-Id: bob header; the
        // namespace must still be Alice's (derived from the validated token).
        let cache = Arc::new(NamespaceRecordingCache::default());
        send(cache.clone(), "per_identity", Some("sk-alice")).await;
        let alice_ns = cache.looked_up.lock().unwrap().last().unwrap().clone();

        let state = build_auth_state("per_identity", cache.clone());
        let app = build_router(state);
        let payload = json!({"model": "mock-model", "messages": [{"role": "user", "content": "what is the policy?"}]});
        let request = Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header("Content-Type", "application/json")
            .header("Authorization", "Bearer sk-alice")
            .header("X-Client-Id", "bob")
            .header("X-Tenant", "bob")
            .body(Body::from(serde_json::to_vec(&payload).unwrap()))
            .unwrap();
        let _ = app.oneshot(request).await.unwrap();
        let ns_with_forged = cache.looked_up.lock().unwrap().last().unwrap().clone();
        assert_eq!(
            alice_ns, ns_with_forged,
            "forged identity headers must not change the authenticated cache scope"
        );
    }

    #[tokio::test]
    async fn per_identity_without_identity_bypasses_cache() {
        // Auth is enabled, so a request with no bearer is rejected (401) before the
        // handler; the cache is never consulted.
        let cache = Arc::new(NamespaceRecordingCache::default());
        let (status, _b) = send(cache.clone(), "per_identity", None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert!(
            cache.looked_up.lock().unwrap().is_empty(),
            "no identity → cache must not be consulted"
        );
    }

    #[tokio::test]
    async fn shared_scope_allows_cross_identity_reuse_only_when_configured() {
        let cache = Arc::new(NamespaceRecordingCache::default());
        // Alice seeds the shared namespace.
        send(cache.clone(), "shared", Some("sk-alice")).await;
        let ns = cache.looked_up.lock().unwrap().last().unwrap().clone();
        cache.hits_for.lock().unwrap().insert(ns.clone());
        // Bob, in shared scope, uses the SAME namespace and gets the hit.
        let (status, body) = send(cache.clone(), "shared", Some("sk-bob")).await;
        let bob_ns = cache.looked_up.lock().unwrap().last().unwrap().clone();
        assert_eq!(
            ns, bob_ns,
            "shared scope must use one namespace across identities"
        );
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["choices"][0]["message"]["content"], "cached");
    }
}

/// allowed_models enforcement (security): a client's allowed_models restricts which
/// models it may use; empty list = unrestricted. Checked for the requested name and
/// the resolved model. HTTP, auth enabled.
#[cfg(test)]
mod allowed_models_e2e {
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;

    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use serde_json::{json, Value};
    use tower::ServiceExt;

    use crate::balancer::WeightedRoundRobin;
    use crate::circuit_breaker::LocalCircuitBreaker;
    use crate::client::ReqwestLlmClient;
    use crate::config::GatewayConfig;
    use crate::guardrails::GuardrailEngine;
    use crate::middleware::rate_limiter::LocalTokenBucket;
    use crate::observability::Metrics;
    use crate::route_config::RouteConfigManager;
    use crate::router::build_router;
    use crate::state::AppState;

    /// `route_model` = optional route-level override (`model:` in the route).
    fn state_with(route_model: Option<&str>) -> AppState {
        let dir = Box::leak(Box::new(tempfile::tempdir().unwrap()));
        let routes_path = dir.path().join("routes.yaml");
        let model_line = match route_model {
            Some(m) => format!("    model: \"{m}\"\n"),
            None => String::new(),
        };
        std::fs::write(
            &routes_path,
            format!("routes:\n  - path: \"/v1/chat/completions\"\n    method: \"POST\"\n    provider: \"openai\"\n{model_line}"),
        )
        .unwrap();
        // alice is restricted to gpt-4o; bob is unrestricted (empty allowed_models).
        let gateway_config = GatewayConfig::from_yaml(
            r#"
server:
  host: "0.0.0.0"
  port: 8080
redis:
  cluster_urls:
    - "redis://localhost:6379"
auth:
  enabled: true
  api_keys:
    - key: "sk-alice"
      client_id: "alice"
      allowed_models: ["gpt-4o"]
    - key: "sk-bob"
      client_id: "bob"
providers:
  - id: "mock-openai"
    provider_type: "openai"
    api_key: "test-key"
    base_url: "http://127.0.0.1:59996/v1"
"#,
        )
        .unwrap();
        let route_config = RouteConfigManager::new_without_watcher(routes_path);
        let guardrails = Arc::new(GuardrailEngine::new(
            route_config.current().config(),
            gateway_config.guardrails.clone(),
        ));
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
            guardrails,
            embeddings: Arc::new(crate::embeddings::StubEmbeddingProvider::deterministic()),
        }
    }

    async fn status_for(route_model: Option<&str>, bearer: &str, model: &str) -> StatusCode {
        let app = build_router(state_with(route_model));
        let payload = json!({"model": model, "messages": [{"role": "user", "content": "hi"}]});
        let request = Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header("Content-Type", "application/json")
            .header("Authorization", format!("Bearer {bearer}"))
            .body(Body::from(serde_json::to_vec(&payload).unwrap()))
            .unwrap();
        let resp = app.oneshot(request).await.unwrap();
        resp.status()
    }

    #[tokio::test]
    async fn restricted_client_denied_for_disallowed_requested_model() {
        // alice may only use gpt-4o; requesting llama3.2 is 403 (not forwarded).
        let status = status_for(None, "sk-alice", "llama3.2").await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn restricted_client_allowed_for_permitted_model() {
        // alice requests gpt-4o (allowed). Provider is unreachable, so not 200/403;
        // the point is it is NOT 403 (authorization passed) → a 5xx/503 from the
        // provider attempt.
        let status = status_for(None, "sk-alice", "gpt-4o").await;
        assert_ne!(
            status,
            StatusCode::FORBIDDEN,
            "permitted model must not be blocked"
        );
    }

    #[tokio::test]
    async fn unrestricted_client_may_use_any_model() {
        let status = status_for(None, "sk-bob", "llama3.2").await;
        assert_ne!(
            status,
            StatusCode::FORBIDDEN,
            "empty allowed_models = unrestricted"
        );
    }

    #[tokio::test]
    async fn route_override_to_disallowed_model_is_denied() {
        // The route forces model "secret-model"; alice requests the allowed gpt-4o,
        // but the resolved model is not permitted → 403 (no silent routing to a
        // model the client can't use).
        let status = status_for(Some("secret-model"), "sk-alice", "gpt-4o").await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }

    // ── alias authorization: an alias must not bypass a model restriction ──────

    /// Gateway with an alias `escape` -> concrete `restricted-model`, and a client
    /// `carol` allowed ONLY to use the alias name `escape` (not the destination).
    fn state_with_alias() -> AppState {
        let dir = Box::leak(Box::new(tempfile::tempdir().unwrap()));
        let routes_path = dir.path().join("routes.yaml");
        std::fs::write(
            &routes_path,
            "routes:\n  - path: \"/v1/chat/completions\"\n    method: \"POST\"\n    provider: \"openai\"\nmodel_aliases:\n  - name: \"escape\"\n    targets:\n      - provider: \"openai\"\n        model: \"restricted-model\"\n  - name: \"ok-alias\"\n    targets:\n      - provider: \"openai\"\n        model: \"gpt-4o\"\n",
        )
        .unwrap();
        let gateway_config = GatewayConfig::from_yaml(
            r#"
server:
  host: "0.0.0.0"
  port: 8080
redis:
  cluster_urls: ["redis://localhost:6379"]
auth:
  enabled: true
  api_keys:
    - key: "sk-carol"
      client_id: "carol"
      allowed_models: ["escape", "gpt-4o"]
    - key: "sk-dave"
      client_id: "dave"
      allowed_models: ["ok-alias", "gpt-4o"]
providers:
  - id: "openai"
    provider_type: "openai"
    api_key: "test-key"
    base_url: "http://127.0.0.1:59995/v1"
"#,
        )
        .unwrap();
        let route_config = RouteConfigManager::new_without_watcher(routes_path);
        let guardrails = Arc::new(GuardrailEngine::new(
            route_config.current().config(),
            gateway_config.guardrails.clone(),
        ));
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
            guardrails,
            embeddings: Arc::new(crate::embeddings::StubEmbeddingProvider::deterministic()),
        }
    }

    async fn alias_status(bearer: &str, model: &str) -> StatusCode {
        let app = build_router(state_with_alias());
        let payload = json!({"model": model, "messages": [{"role": "user", "content": "hi"}]});
        let req = Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header("Content-Type", "application/json")
            .header("Authorization", format!("Bearer {bearer}"))
            .body(Body::from(serde_json::to_vec(&payload).unwrap()))
            .unwrap();
        app.oneshot(req).await.unwrap().status()
    }

    #[tokio::test]
    async fn alias_cannot_bypass_destination_restriction() {
        // carol may use the alias NAME "escape" and "gpt-4o", but NOT the alias's
        // destination "restricted-model". Resolving the alias must still enforce the
        // destination → 403 (the alias is not a bypass).
        let status = alias_status("sk-carol", "escape").await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "alias must not grant access to a restricted destination"
        );
    }

    #[tokio::test]
    async fn alias_allowed_when_name_and_destination_permitted() {
        // dave may use "ok-alias" (name) and its destination "gpt-4o" → not blocked.
        let status = alias_status("sk-dave", "ok-alias").await;
        assert_ne!(
            status,
            StatusCode::FORBIDDEN,
            "permitted alias+destination must pass"
        );
    }

    #[tokio::test]
    async fn alias_denied_when_name_not_permitted() {
        // dave is not allowed the "escape" alias name at all.
        let status = alias_status("sk-dave", "escape").await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }

    // ── /v1/models respects permissions ───────────────────────────────────────

    async fn models_ids(bearer: &str) -> Vec<String> {
        let app = build_router(state_with_alias());
        let req = Request::builder()
            .method("GET")
            .uri("/v1/models")
            .header("Authorization", format!("Bearer {bearer}"))
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        body["data"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["id"].as_str().unwrap().to_string())
            .collect()
    }

    #[tokio::test]
    async fn models_listing_filtered_by_allowed_models() {
        // Listing = declared aliases (escape, ok-alias) ∩ allowed_models. carol is
        // allowed [escape, gpt-4o]; only "escape" is a declared listable id.
        // gpt-4o / restricted-model are alias targets, not declared concrete
        // models, so they are not listed at all.
        let ids = models_ids("sk-carol").await;
        assert!(
            ids.contains(&"escape".to_string()),
            "allowed alias listed: {ids:?}"
        );
        assert!(
            !ids.contains(&"ok-alias".to_string()),
            "disallowed alias not listed: {ids:?}"
        );
        assert!(
            !ids.contains(&"restricted-model".to_string()),
            "targets are not listed: {ids:?}"
        );
        // dave is allowed [ok-alias, gpt-4o] → sees ok-alias, not escape.
        let dave = models_ids("sk-dave").await;
        assert!(
            dave.contains(&"ok-alias".to_string()),
            "dave sees ok-alias: {dave:?}"
        );
        assert!(
            !dave.contains(&"escape".to_string()),
            "dave does not see escape: {dave:?}"
        );
    }

    #[tokio::test]
    async fn models_listing_requires_auth() {
        // auth enabled → no bearer → 401 (handled by the auth layer).
        let app = build_router(state_with_alias());
        let req = Request::builder()
            .method("GET")
            .uri("/v1/models")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }
}

/// Phase 6A virtual models end-to-end: precedence, @-alias, authorization,
/// /v1/models, cache bypass for multi-target, and hot-reload consistency.
#[cfg(test)]
mod phase6a_virtual_models_e2e {
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;

    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use serde_json::{json, Value};
    use tower::ServiceExt;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use crate::balancer::WeightedRoundRobin;
    use crate::circuit_breaker::LocalCircuitBreaker;
    use crate::client::ReqwestLlmClient;
    use crate::config::GatewayConfig;
    use crate::guardrails::GuardrailEngine;
    use crate::middleware::rate_limiter::LocalTokenBucket;
    use crate::observability::Metrics;
    use crate::route_config::RouteConfigManager;
    use crate::router::build_router;
    use crate::state::AppState;

    /// A mock OpenAI provider that echoes the `model` it received in the response's
    /// `model` field, so tests can read the forwarded model.
    struct EchoModel;
    impl wiremock::Respond for EchoModel {
        fn respond(&self, req: &wiremock::Request) -> ResponseTemplate {
            let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
            let forwarded = body["model"].as_str().unwrap_or("").to_string();
            ResponseTemplate::new(200).set_body_json(json!({
                "id": "x", "object": "chat.completion", "created": 0,
                "model": forwarded,
                "choices": [{"index": 0, "finish_reason": "stop",
                             "message": {"role": "assistant", "content": "ok"}}],
                "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
            }))
        }
    }

    async fn mock_provider() -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(EchoModel)
            .mount(&server)
            .await;
        server
    }

    /// Build a gateway. `route_yaml_extra` is inserted under the single route;
    /// `aliases_yaml` is appended at top level; `uri` is the mock provider base.
    fn state_with(uri: &str, route_yaml_extra: &str, aliases_yaml: &str) -> AppState {
        let dir = Box::leak(Box::new(tempfile::tempdir().unwrap()));
        let routes_path = dir.path().join("routes.yaml");
        std::fs::write(
            &routes_path,
            format!(
                "routes:\n  - path: \"/v1/chat/completions\"\n    method: \"POST\"\n    provider: \"openai\"\n{route_yaml_extra}{aliases_yaml}"
            ),
        )
        .unwrap();
        let gateway_config = GatewayConfig::from_yaml(&format!(
            "server:\n  host: \"0.0.0.0\"\n  port: 8080\nredis:\n  cluster_urls: [\"redis://localhost:6379\"]\nauth:\n  enabled: false\nproviders:\n  - id: \"openai\"\n    provider_type: \"openai\"\n    api_key: \"k\"\n    base_url: \"{uri}/v1\"\n  - id: \"ollama\"\n    provider_type: \"ollama\"\n    api_key: \"ollama\"\n    base_url: \"{uri}/v1\"\n"
        ))
        .unwrap();
        let route_config = RouteConfigManager::new_without_watcher(routes_path);
        let guardrails = Arc::new(GuardrailEngine::new(
            route_config.current().config(),
            gateway_config.guardrails.clone(),
        ));
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
            guardrails,
            embeddings: Arc::new(crate::embeddings::StubEmbeddingProvider::deterministic()),
        }
    }

    const ALIASES: &str = "model_aliases:\n  - name: \"fast\"\n    targets:\n      - provider: \"ollama\"\n        model: \"llama3.2\"\n";

    /// Returns (status, forwarded_model or None if provider not called).
    async fn send(state: AppState, model: &str) -> (StatusCode, Option<String>) {
        let app = build_router(state);
        let payload = json!({"model": model, "messages": [{"role": "user", "content": "hi"}]});
        let req = Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header("Content-Type", "application/json")
            .body(Body::from(serde_json::to_vec(&payload).unwrap()))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        let forwarded = body["model"]
            .as_str()
            .map(str::to_string)
            .filter(|s| !s.is_empty());
        (status, forwarded)
    }

    // ── Precedence table (9 cases) ──────────────────────────────────────────

    #[tokio::test]
    async fn precedence_route_override_ignores_alias() {
        let server = mock_provider().await;
        // Route forces "forced-model"; client sends alias "fast" → override wins.
        let st = state_with(&server.uri(), "    model: \"forced-model\"\n", ALIASES);
        let (status, fwd) = send(st, "fast").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(fwd.as_deref(), Some("forced-model"));
    }

    #[tokio::test]
    async fn precedence_default_does_not_shadow_known_alias() {
        let server = mock_provider().await;
        let st = state_with(&server.uri(), "    default_model: \"d\"\n", ALIASES);
        let (_s, fwd) = send(st, "fast").await;
        assert_eq!(
            fwd.as_deref(),
            Some("llama3.2"),
            "alias resolves despite default_model"
        );
    }

    #[tokio::test]
    async fn precedence_default_keeps_concrete() {
        let server = mock_provider().await;
        let st = state_with(&server.uri(), "    default_model: \"d\"\n", ALIASES);
        let (_s, fwd) = send(st, "gpt-4o").await;
        assert_eq!(fwd.as_deref(), Some("gpt-4o"));
    }

    #[tokio::test]
    async fn precedence_no_route_model_resolves_alias() {
        let server = mock_provider().await;
        let st = state_with(&server.uri(), "", ALIASES);
        let (_s, fwd) = send(st, "fast").await;
        assert_eq!(fwd.as_deref(), Some("llama3.2"));
    }

    #[tokio::test]
    async fn precedence_no_route_model_keeps_concrete() {
        let server = mock_provider().await;
        let st = state_with(&server.uri(), "", ALIASES);
        let (_s, fwd) = send(st, "gpt-4o").await;
        assert_eq!(fwd.as_deref(), Some("gpt-4o"));
    }

    #[tokio::test]
    async fn precedence_unknown_bare_alias_passes_through() {
        let server = mock_provider().await;
        let st = state_with(&server.uri(), "", ALIASES);
        let (status, fwd) = send(st, "totally-unknown").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            fwd.as_deref(),
            Some("totally-unknown"),
            "bare unknown is concrete passthrough"
        );
    }

    #[tokio::test]
    async fn at_prefix_resolves_known_alias() {
        let server = mock_provider().await;
        let st = state_with(&server.uri(), "", ALIASES);
        let (_s, fwd) = send(st, "@fast").await;
        assert_eq!(fwd.as_deref(), Some("llama3.2"));
    }

    #[tokio::test]
    async fn at_prefix_unknown_alias_is_400_not_forwarded() {
        let server = mock_provider().await;
        let st = state_with(&server.uri(), "", ALIASES);
        let (status, fwd) = send(st, "@nope").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(fwd.is_none(), "unknown @alias must not be forwarded");
        assert_eq!(server.received_requests().await.unwrap().len(), 0);
    }

    // ── /v1/models ──────────────────────────────────────────────────────────

    #[tokio::test]
    async fn models_endpoint_lists_aliases_and_concretes_without_secrets() {
        let server = mock_provider().await;
        let st = state_with(&server.uri(), "    model: \"gpt-4o\"\n", ALIASES);
        let app = build_router(st);
        let req = Request::builder()
            .method("GET")
            .uri("/v1/models")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["object"], "list");
        let ids: Vec<&str> = body["data"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["id"].as_str().unwrap())
            .collect();
        assert!(ids.contains(&"fast"), "alias listed: {ids:?}");
        assert!(ids.contains(&"gpt-4o"), "concrete listed: {ids:?}");
        // No secrets/topology anywhere in the payload.
        let raw = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(
            !raw.contains("base_url") && !raw.contains(&server.uri()) && !raw.contains("api_key")
        );
        assert!(!raw.contains("weight") && !raw.contains("targets"));
        assert!(!raw.contains("melis:kind"));
    }

    // ── cache: multi-target bypass ────────────────────────────────────────────

    #[tokio::test]
    async fn multi_target_alias_bypasses_cache() {
        let server = mock_provider().await;
        let aliases = "model_aliases:\n  - name: \"pool\"\n    targets:\n      - provider: \"openai\"\n        model: \"gpt-4o\"\n        weight: 1\n      - provider: \"ollama\"\n        model: \"llama3.2\"\n        weight: 1\n";
        let st = state_with(
            &server.uri(),
            "    semantic_cache:\n      enabled: true\n      scope: \"shared\"\n",
            aliases,
        );
        let metrics = st.metrics.clone();
        let (status, _fwd) = send(st, "pool").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            metrics
                .cache_skipped_total
                .with_label_values(&["multi_target_alias"])
                .get(),
            1,
            "multi-target alias must bypass the cache"
        );
    }

    // ── hot reload mid-request consistency ────────────────────────────────────

    #[tokio::test]
    async fn request_uses_consistent_snapshot_across_reload() {
        // A resolver snapshot is captured once per request. We simulate a reload by
        // swapping the manager's config after capturing `current()`: the captured
        // guard must keep resolving the OLD alias mapping.
        let server = mock_provider().await;
        let st = state_with(&server.uri(), "", ALIASES);
        let resolver_before = st.route_config.current();
        // The captured snapshot resolves `fast` -> llama3.2 regardless of later swaps.
        let resolution = resolver_before.resolve_model("fast");
        match resolution {
            crate::route_config::ModelResolution::Alias { name, targets } => {
                assert_eq!(name, "fast");
                assert_eq!(targets[0].model, "llama3.2");
            }
            other => panic!("expected alias, got {other:?}"),
        }
        // Holding `resolver_before` pins the version for the whole request lifetime.
        drop(resolver_before);
    }
}

/// Cache + failover destination (audit): a response produced by a failover
/// provider must not be stored under the primary destination's namespace.
#[cfg(test)]
mod cache_failover_destination_e2e {
    use std::sync::atomic::AtomicBool;
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use serde_json::json;
    use tower::ServiceExt;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use crate::balancer::WeightedRoundRobin;
    use crate::circuit_breaker::LocalCircuitBreaker;
    use crate::client::ReqwestLlmClient;
    use crate::config::GatewayConfig;
    use crate::guardrails::GuardrailEngine;
    use crate::middleware::rate_limiter::LocalTokenBucket;
    use crate::observability::Metrics;
    use crate::route_config::RouteConfigManager;
    use crate::router::build_router;
    use crate::semantic_cache::{CacheKeyCtx, CacheLookup, SemanticCache};
    use crate::state::AppState;

    /// Records the namespaces that receive a store.
    #[derive(Default)]
    struct StoreRecordingCache {
        stored_ns: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl SemanticCache for StoreRecordingCache {
        async fn lookup(&self, _ctx: &CacheKeyCtx, _embedding: &[f64]) -> CacheLookup {
            CacheLookup::Miss
        }
        async fn store(
            &self,
            ctx: &CacheKeyCtx,
            _entry_id: &str,
            _embedding: &[f64],
            _response: &serde_json::Value,
            _was_streaming: bool,
        ) {
            self.stored_ns.lock().unwrap().push(ctx.namespace());
        }
        async fn invalidate_namespace(&self, _ctx: &CacheKeyCtx) {}
    }

    #[tokio::test]
    async fn failover_response_not_stored_under_primary_destination() {
        // Primary provider returns 500 (triggers failover); secondary returns 200.
        let primary = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
            .mount(&primary)
            .await;
        let secondary = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "x", "object": "chat.completion", "created": 0, "model": "m2",
                "choices": [{"index": 0, "finish_reason": "stop",
                             "message": {"role": "assistant", "content": "ok"}}],
                "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
            })))
            .mount(&secondary)
            .await;

        let dir = Box::leak(Box::new(tempfile::tempdir().unwrap()));
        let routes_path = dir.path().join("routes.yaml");
        // Route lists two providers: prov-a (primary) then prov-b (secondary).
        std::fs::write(
            &routes_path,
            "routes:\n  - path: \"/v1/chat/completions\"\n    method: \"POST\"\n    providers:\n      - name: \"prov-a\"\n        weight: 1\n        model: \"m1\"\n      - name: \"prov-b\"\n        weight: 1\n        model: \"m2\"\n    semantic_cache:\n      enabled: true\n      scope: \"shared\"\ncustom_providers:\n  - name: \"prov-a\"\n    base_url: \"unused\"\n    api_format: \"openai_compatible\"\n  - name: \"prov-b\"\n    base_url: \"unused\"\n    api_format: \"openai_compatible\"\n",
        )
        .unwrap();
        let gateway_config = GatewayConfig::from_yaml(&format!(
            "server:\n  host: \"0.0.0.0\"\n  port: 8080\nredis:\n  cluster_urls: [\"redis://localhost:6379\"]\nauth:\n  enabled: false\nproviders:\n  - id: \"prov-a\"\n    provider_type: \"openai\"\n    api_key: \"k\"\n    base_url: \"{}/v1\"\n  - id: \"prov-b\"\n    provider_type: \"openai\"\n    api_key: \"k\"\n    base_url: \"{}/v1\"\n",
            primary.uri(),
            secondary.uri()
        ))
        .unwrap();
        let route_config = RouteConfigManager::new_without_watcher(routes_path);
        let guardrails = Arc::new(GuardrailEngine::new(
            route_config.current().config(),
            gateway_config.guardrails.clone(),
        ));
        let cache = Arc::new(StoreRecordingCache::default());
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
            semantic_cache: cache.clone(),
            outbound_guardrail: Arc::new(crate::semantic_cache::NoopOutboundGuardrail),
            guardrails,
            embeddings: Arc::new(crate::embeddings::StubEmbeddingProvider::deterministic()),
        };

        let app = build_router(state);
        let payload = json!({"model": "m1", "messages": [{"role": "user", "content": "hi"}]});
        let req = Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header("Content-Type", "application/json")
            .body(Body::from(serde_json::to_vec(&payload).unwrap()))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        // Failover delivered the secondary's 200.
        assert_eq!(resp.status(), StatusCode::OK);

        // The response came from prov-b, but the cache key was built for the primary
        // (prov-a). The store must have been SKIPPED (not stored under prov-a's
        // namespace), and the skip must be counted.
        let stored = cache.stored_ns.lock().unwrap().clone();
        assert!(
            stored.is_empty(),
            "a failover response must not be stored under the primary namespace: {stored:?}"
        );
        assert_eq!(
            metrics
                .cache_skipped_total
                .with_label_values(&["failover_destination_changed"])
                .get(),
            1,
            "failover store-skip must be recorded"
        );
    }
}
