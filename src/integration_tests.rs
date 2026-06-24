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
        }
    }

    /// Helper to collect the full response body as a string.
    async fn body_to_string(body: Body) -> String {
        let bytes = body.collect().await.unwrap().to_bytes();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    // ─── Test 1: JSON Response ────────────────────────────────────────────────

    #[tokio::test]
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
        assert!(json["error"]["message"].as_str().unwrap().contains("messages"));
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
        assert_eq!(counter_value, 1, "requests_total should be incremented after a successful request");
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
}
