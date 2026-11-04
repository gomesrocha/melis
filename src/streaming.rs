//! SSE Streaming utilities module.
//!
//! Provides functionality to transform raw SSE chunks from LLM providers
//! into the OpenAI-compatible SSE format using the PayloadTranspiler trait.
//!
//! The `transform_sse_stream` function processes each chunk individually,
//! converting it on-the-fly without buffering the entire response. This ensures
//! low-latency token-by-token delivery to connected clients.

use axum::response::sse::Event;
use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use std::convert::Infallible;
use std::pin::Pin;

use crate::transpiler::PayloadTranspiler;

/// Error type representing upstream provider stream failures.
///
/// This is a simplified client error for the streaming layer.
/// The full `ClientError` will be defined in the HTTP client module.
#[derive(Debug, thiserror::Error)]
pub enum StreamError {
    /// Network or connection error from the upstream provider.
    #[error("Connection error: {0}")]
    Connection(String),

    /// Provider returned an unexpected response format.
    #[error("Invalid response: {0}")]
    InvalidResponse(String),
}

/// Transforms a raw SSE byte stream from a provider into OpenAI-format SSE events.
///
/// This function:
/// 1. Takes each raw SSE chunk (Bytes) from the provider stream
/// 2. Parses the chunk as JSON (`serde_json::Value`)
/// 3. Calls `transpiler.translate_chunk(&chunk_value)` to convert to OpenAI format
/// 4. Wraps the result in an `axum::response::sse::Event`
/// 5. On parse/transpiler error: logs a warning and skips the chunk (graceful degradation)
/// 6. On stream end: emits a `data: [DONE]` event to signal completion
///
/// # Arguments
///
/// * `provider_stream` - A pinned stream of `Result<Bytes, StreamError>` from the upstream provider.
/// * `transpiler` - A reference to the transpiler that converts native chunks to OpenAI format.
///
/// # Returns
///
/// A stream of `Result<Event, Infallible>` suitable for use with `axum::response::sse::Sse`.
pub fn transform_sse_stream(
    provider_stream: Pin<Box<dyn Stream<Item = Result<Bytes, StreamError>> + Send>>,
    transpiler: Box<dyn PayloadTranspiler>,
) -> Pin<Box<dyn Stream<Item = Result<Event, Infallible>> + Send>> {
    transform_sse_stream_guarded(provider_stream, transpiler, None, None)
}

/// Maximum size of the outbound sliding-window buffer (bytes).
///
/// Large enough to catch patterns that straddle chunk boundaries, bounded to keep
/// memory predictable. Only the suffix is retained once the limit is reached.
const GUARDRAIL_WINDOW_MAX: usize = 4096;

/// Like [`transform_sse_stream`], but applies outbound guardrails with a sliding
/// window over the accumulated assistant text before emitting each chunk.
///
/// - `guardrails`: compiled outbound rules for the route (`None` = passthrough).
/// - `metrics`: used to record guardrail actions.
///
/// Emission latency is at most one chunk: each chunk is evaluated against the
/// accumulated window *before* it is emitted. A blocking match emits an SSE error
/// event and terminates the stream without emitting the offending content.
pub fn transform_sse_stream_guarded(
    provider_stream: Pin<Box<dyn Stream<Item = Result<Bytes, StreamError>> + Send>>,
    transpiler: Box<dyn PayloadTranspiler>,
    guardrails: Option<std::sync::Arc<crate::guardrails::CompiledGuardrails>>,
    metrics: Option<std::sync::Arc<crate::observability::Metrics>>,
) -> Pin<Box<dyn Stream<Item = Result<Event, Infallible>> + Send>> {
    use crate::guardrails::GuardrailDecision;
    use crate::route_config::GuardrailDirection;

    let stream = async_stream::stream! {
        let mut stream = provider_stream;
        // Sliding window of accumulated assistant text (for cross-chunk patterns).
        let mut window = String::new();

        while let Some(chunk_result) = stream.next().await {
            match chunk_result {
                Ok(bytes) => {
                    // SSE chunks from providers come as "data: {...}\n\n"
                    // We need to strip the "data: " prefix and handle special markers
                    let raw = String::from_utf8_lossy(&bytes);

                    // Process each line in the chunk (may contain multiple SSE events)
                    for line in raw.lines() {
                        let line = line.trim();

                        // Skip empty lines and comments
                        if line.is_empty() || line.starts_with(':') {
                            continue;
                        }

                        // Strip "data: " prefix
                        let json_str = if let Some(stripped) = line.strip_prefix("data: ") {
                            stripped.trim()
                        } else if let Some(stripped) = line.strip_prefix("data:") {
                            stripped.trim()
                        } else {
                            // Try parsing the line directly as JSON (some providers don't use "data:" prefix)
                            line
                        };

                        // Handle [DONE] marker
                        if json_str == "[DONE]" {
                            yield Ok(Event::default().data("[DONE]"));
                            return;
                        }

                        // Skip empty data
                        if json_str.is_empty() {
                            continue;
                        }

                        // Parse the JSON payload
                        let parsed = match serde_json::from_str::<serde_json::Value>(json_str) {
                            Ok(value) => value,
                            Err(e) => {
                                tracing::warn!(
                                    error = %e,
                                    chunk = %json_str,
                                    "Failed to parse SSE chunk as JSON, skipping"
                                );
                                continue;
                            }
                        };

                        // Translate the chunk to OpenAI format via the transpiler
                        let translated = match transpiler.translate_chunk(&parsed) {
                            Ok(value) => value,
                            Err(e) => {
                                tracing::warn!(
                                    error = %e,
                                    "Transpiler failed to translate chunk, skipping"
                                );
                                continue;
                            }
                        };

                        // ─── Outbound guardrails (sliding window) ─────────────
                        // Evaluate the accumulated text BEFORE emitting this chunk.
                        let mut translated = translated;
                        if let Some(g) = &guardrails {
                            // Extract this chunk's delta content and append to the window.
                            let delta = translated
                                .get("choices")
                                .and_then(|c| c.as_array())
                                .and_then(|c| c.first())
                                .and_then(|c| c.get("delta"))
                                .and_then(|d| d.get("content"))
                                .and_then(|c| c.as_str())
                                .unwrap_or("")
                                .to_string();

                            if !delta.is_empty() {
                                window.push_str(&delta);
                                if window.len() > GUARDRAIL_WINDOW_MAX {
                                    // Keep the suffix on a char boundary.
                                    let cut = window.len() - GUARDRAIL_WINDOW_MAX;
                                    let mut boundary = cut;
                                    while boundary < window.len() && !window.is_char_boundary(boundary) {
                                        boundary += 1;
                                    }
                                    window = window[boundary..].to_string();
                                }

                                let noop_metrics;
                                let metrics_ref = match &metrics {
                                    Some(m) => m.as_ref(),
                                    None => {
                                        noop_metrics = crate::observability::Metrics::new();
                                        &noop_metrics
                                    }
                                };

                                match g.evaluate(GuardrailDirection::Outbound, &window, metrics_ref) {
                                    GuardrailDecision::Blocked { rule } => {
                                        tracing::warn!(
                                            rule = %rule,
                                            "outbound guardrail blocked streaming content; terminating stream"
                                        );
                                        // Emit an OpenAI-compatible SSE error, without the content.
                                        let err = serde_json::json!({
                                            "error": {
                                                "message": format!("Response blocked by guardrail rule '{}'", rule),
                                                "type": "guardrail_error",
                                                "code": "content_blocked"
                                            }
                                        });
                                        yield Ok(Event::default().data(err.to_string()));
                                        yield Ok(Event::default().data("[DONE]"));
                                        return;
                                    }
                                    GuardrailDecision::Redacted(_) => {
                                        // The window matched. Try to redact within
                                        // this delta. If the match straddles the
                                        // chunk boundary (the delta alone has no
                                        // match), redacting the delta is a no-op and
                                        // would leak the partial halves — suppress
                                        // this delta's content instead. Safe and
                                        // bounded (no full-response buffering); may
                                        // drop adjacent benign text in the same delta.
                                        if let Some(slot) = translated
                                            .get_mut("choices")
                                            .and_then(|c| c.as_array_mut())
                                            .and_then(|c| c.first_mut())
                                            .and_then(|c| c.get_mut("delta"))
                                            .and_then(|d| d.get_mut("content"))
                                        {
                                            let replacement = match g.evaluate(
                                                GuardrailDirection::Outbound,
                                                &delta,
                                                metrics_ref,
                                            ) {
                                                GuardrailDecision::Redacted(red) => red,
                                                // No in-delta match => cross-boundary
                                                // straddle: drop this delta's content.
                                                _ => String::new(),
                                            };
                                            *slot = serde_json::Value::String(replacement);
                                        }
                                    }
                                    GuardrailDecision::Allow => {}
                                }
                            }
                        }

                        // Serialize and wrap in SSE Event
                        match serde_json::to_string(&translated) {
                            Ok(data) => {
                                yield Ok(Event::default().data(data));
                            }
                            Err(e) => {
                                tracing::warn!(
                                    error = %e,
                                    "Failed to serialize translated chunk, skipping"
                                );
                                continue;
                            }
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "Provider stream error, terminating SSE stream"
                    );
                    // On stream error, emit [DONE] and terminate cleanly
                    yield Ok(Event::default().data("[DONE]"));
                    return;
                }
            }
        }

        // Stream ended normally — emit [DONE] terminator
        yield Ok(Event::default().data("[DONE]"));
    };

    Box::pin(stream)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transpiler::openai::OpenAiTranspiler;
    use axum::body::Body;
    use axum::response::sse::Sse;
    use axum::response::IntoResponse;
    use futures_util::stream;
    use serde_json::json;

    /// Helper: create a provider stream from a vec of results.
    fn mock_provider_stream(
        chunks: Vec<Result<Bytes, StreamError>>,
    ) -> Pin<Box<dyn Stream<Item = Result<Bytes, StreamError>> + Send>> {
        Box::pin(stream::iter(chunks))
    }

    /// Helper: render a transformed stream through Axum's Sse response
    /// and extract data lines from the SSE wire format.
    async fn collect_data_lines(
        stream: Pin<Box<dyn Stream<Item = Result<Event, Infallible>> + Send>>,
    ) -> Vec<String> {
        let sse = Sse::new(stream);
        let response = sse.into_response();
        let body_bytes = axum::body::to_bytes(Body::new(response.into_body()), usize::MAX)
            .await
            .unwrap();
        let body_str = String::from_utf8(body_bytes.to_vec()).unwrap();

        body_str
            .lines()
            .filter(|line| line.starts_with("data:"))
            .map(|line| line.strip_prefix("data:").unwrap().trim().to_string())
            .collect()
    }

    #[tokio::test]
    async fn test_transform_single_chunk_to_openai_format() {
        let chunk = json!({
            "id": "chatcmpl-abc123",
            "object": "chat.completion.chunk",
            "created": 1700000000,
            "model": "gpt-4o",
            "choices": [{
                "index": 0,
                "delta": {"role": "assistant", "content": "Hello"},
                "finish_reason": null
            }]
        });

        let bytes = Bytes::from(serde_json::to_vec(&chunk).unwrap());
        let provider_stream = mock_provider_stream(vec![Ok(bytes)]);
        let transpiler: Box<dyn PayloadTranspiler> = Box::new(OpenAiTranspiler);

        let transformed = transform_sse_stream(provider_stream, transpiler);
        let data_lines = collect_data_lines(transformed).await;

        // Should have the chunk data + [DONE] terminator
        assert_eq!(data_lines.len(), 2);

        // First line should contain the translated chunk
        let parsed: serde_json::Value = serde_json::from_str(&data_lines[0]).unwrap();
        assert_eq!(parsed["object"], "chat.completion.chunk");
        assert_eq!(parsed["choices"][0]["delta"]["content"], "Hello");

        // Last line should be [DONE]
        assert_eq!(data_lines[1], "[DONE]");
    }

    #[tokio::test]
    async fn test_transform_multiple_chunks() {
        let chunks: Vec<serde_json::Value> = vec![
            json!({
                "id": "chatcmpl-abc123",
                "object": "chat.completion.chunk",
                "created": 1700000000,
                "model": "gpt-4o",
                "choices": [{"index": 0, "delta": {"role": "assistant"}, "finish_reason": null}]
            }),
            json!({
                "id": "chatcmpl-abc123",
                "object": "chat.completion.chunk",
                "created": 1700000000,
                "model": "gpt-4o",
                "choices": [{"index": 0, "delta": {"content": "World"}, "finish_reason": null}]
            }),
            json!({
                "id": "chatcmpl-abc123",
                "object": "chat.completion.chunk",
                "created": 1700000000,
                "model": "gpt-4o",
                "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]
            }),
        ];

        let stream_items: Vec<Result<Bytes, StreamError>> = chunks
            .iter()
            .map(|c| Ok(Bytes::from(serde_json::to_vec(c).unwrap())))
            .collect();

        let provider_stream = mock_provider_stream(stream_items);
        let transpiler: Box<dyn PayloadTranspiler> = Box::new(OpenAiTranspiler);

        let transformed = transform_sse_stream(provider_stream, transpiler);
        let data_lines = collect_data_lines(transformed).await;

        // 3 chunk lines + 1 [DONE]
        assert_eq!(data_lines.len(), 4);

        // Verify first chunk has role
        let parsed0: serde_json::Value = serde_json::from_str(&data_lines[0]).unwrap();
        assert_eq!(parsed0["choices"][0]["delta"]["role"], "assistant");

        // Verify second chunk has content
        let parsed1: serde_json::Value = serde_json::from_str(&data_lines[1]).unwrap();
        assert_eq!(parsed1["choices"][0]["delta"]["content"], "World");

        // Verify third chunk has finish_reason
        let parsed2: serde_json::Value = serde_json::from_str(&data_lines[2]).unwrap();
        assert_eq!(parsed2["choices"][0]["finish_reason"], "stop");

        // Verify last line is [DONE]
        assert_eq!(data_lines[3], "[DONE]");
    }

    #[tokio::test]
    async fn test_invalid_json_chunks_are_skipped() {
        let valid_chunk = json!({
            "id": "chatcmpl-abc123",
            "object": "chat.completion.chunk",
            "created": 1700000000,
            "model": "gpt-4o",
            "choices": [{"index": 0, "delta": {"content": "Hi"}, "finish_reason": null}]
        });

        let stream_items: Vec<Result<Bytes, StreamError>> = vec![
            Ok(Bytes::from(b"not valid json".to_vec())), // Invalid - should be skipped
            Ok(Bytes::from(serde_json::to_vec(&valid_chunk).unwrap())), // Valid
            Ok(Bytes::from(b"{incomplete".to_vec())),    // Invalid - should be skipped
        ];

        let provider_stream = mock_provider_stream(stream_items);
        let transpiler: Box<dyn PayloadTranspiler> = Box::new(OpenAiTranspiler);

        let transformed = transform_sse_stream(provider_stream, transpiler);
        let data_lines = collect_data_lines(transformed).await;

        // Only valid chunk + [DONE] should appear (invalid ones skipped)
        assert_eq!(data_lines.len(), 2);

        let parsed: serde_json::Value = serde_json::from_str(&data_lines[0]).unwrap();
        assert_eq!(parsed["choices"][0]["delta"]["content"], "Hi");

        assert_eq!(data_lines[1], "[DONE]");
    }

    #[tokio::test]
    async fn test_stream_error_terminates_with_done() {
        let valid_chunk = json!({
            "id": "chatcmpl-abc123",
            "object": "chat.completion.chunk",
            "created": 1700000000,
            "model": "gpt-4o",
            "choices": [{"index": 0, "delta": {"content": "Before error"}, "finish_reason": null}]
        });

        let stream_items: Vec<Result<Bytes, StreamError>> = vec![
            Ok(Bytes::from(serde_json::to_vec(&valid_chunk).unwrap())),
            Err(StreamError::Connection("connection reset".to_string())),
            // This chunk should never be reached
            Ok(Bytes::from(
                serde_json::to_vec(&json!({"unreachable": true})).unwrap(),
            )),
        ];

        let provider_stream = mock_provider_stream(stream_items);
        let transpiler: Box<dyn PayloadTranspiler> = Box::new(OpenAiTranspiler);

        let transformed = transform_sse_stream(provider_stream, transpiler);
        let data_lines = collect_data_lines(transformed).await;

        // Should have: valid chunk + [DONE] from error handling
        assert_eq!(data_lines.len(), 2);

        let parsed: serde_json::Value = serde_json::from_str(&data_lines[0]).unwrap();
        assert_eq!(parsed["choices"][0]["delta"]["content"], "Before error");

        assert_eq!(data_lines[1], "[DONE]");
    }

    #[tokio::test]
    async fn test_empty_stream_emits_done() {
        let provider_stream = mock_provider_stream(vec![]);
        let transpiler: Box<dyn PayloadTranspiler> = Box::new(OpenAiTranspiler);

        let transformed = transform_sse_stream(provider_stream, transpiler);
        let data_lines = collect_data_lines(transformed).await;

        // Empty stream should still emit [DONE]
        assert_eq!(data_lines.len(), 1);
        assert_eq!(data_lines[0], "[DONE]");
    }

    #[tokio::test]
    async fn test_output_events_are_valid_openai_chunk_format() {
        let chunk = json!({
            "id": "chatcmpl-test",
            "object": "chat.completion.chunk",
            "created": 1700000000,
            "model": "gpt-4o",
            "choices": [{
                "index": 0,
                "delta": {"role": "assistant", "content": "test response"},
                "finish_reason": null
            }]
        });

        let bytes = Bytes::from(serde_json::to_vec(&chunk).unwrap());
        let provider_stream = mock_provider_stream(vec![Ok(bytes)]);
        let transpiler: Box<dyn PayloadTranspiler> = Box::new(OpenAiTranspiler);

        let transformed = transform_sse_stream(provider_stream, transpiler);
        let data_lines = collect_data_lines(transformed).await;

        // Verify the non-DONE event has proper OpenAI chunk structure
        let parsed: serde_json::Value = serde_json::from_str(&data_lines[0]).unwrap();

        // Validate required OpenAI chunk fields
        assert!(parsed.get("id").is_some(), "Missing 'id' field");
        assert_eq!(parsed["object"], "chat.completion.chunk");
        assert!(parsed.get("created").is_some(), "Missing 'created' field");
        assert!(parsed.get("model").is_some(), "Missing 'model' field");
        assert!(parsed.get("choices").is_some(), "Missing 'choices' field");

        let choices = parsed["choices"].as_array().unwrap();
        assert!(!choices.is_empty());
        assert!(choices[0].get("index").is_some());
        assert!(choices[0].get("delta").is_some());
    }

    // ─── Outbound guardrail (sliding window) streaming tests ──────────────────

    use crate::guardrails::CompiledGuardrails;
    use crate::route_config::{
        GuardrailAction, GuardrailDirection, GuardrailRule, GuardrailType, GuardrailsConfig,
    };
    use std::sync::Arc;

    fn content_chunk(content: &str) -> Bytes {
        let chunk = json!({
            "id": "chatcmpl-x",
            "object": "chat.completion.chunk",
            "created": 1700000000,
            "model": "gpt-4o",
            "choices": [{"index": 0, "delta": {"role": "assistant", "content": content}, "finish_reason": null}]
        });
        Bytes::from(serde_json::to_vec(&chunk).unwrap())
    }

    fn outbound_keyword_guardrail(
        action: GuardrailAction,
        keyword: &str,
    ) -> Arc<CompiledGuardrails> {
        let cfg = GuardrailsConfig {
            rules: vec![GuardrailRule {
                name: "stream-rule".to_string(),
                r#type: GuardrailType::Keyword,
                direction: GuardrailDirection::Outbound,
                action,
                pattern: None,
                keywords: Some(vec![keyword.to_string()]),
            }],
            fail_closed: None,
        };
        Arc::new(CompiledGuardrails::compile(&cfg, &Default::default()).unwrap())
    }

    #[tokio::test]
    async fn streaming_block_interrupts_without_leaking_content() {
        // Two chunks; the second contains the blocked keyword. The blocked content
        // must NOT be emitted; an SSE error event is emitted instead.
        let provider_stream = mock_provider_stream(vec![
            Ok(content_chunk("safe start ")),
            Ok(content_chunk("FORBIDDEN secret")),
        ]);
        let transpiler: Box<dyn PayloadTranspiler> = Box::new(OpenAiTranspiler);
        let g = outbound_keyword_guardrail(GuardrailAction::Block, "forbidden");

        let transformed = transform_sse_stream_guarded(provider_stream, transpiler, Some(g), None);
        let data_lines = collect_data_lines(transformed).await;

        let joined = data_lines.join("\n");
        assert!(
            !joined.to_lowercase().contains("forbidden"),
            "blocked content leaked: {}",
            joined
        );
        assert!(
            joined.contains("content_blocked") || joined.contains("guardrail"),
            "expected a guardrail error event, got: {}",
            joined
        );
        // First (safe) chunk may have been emitted; the point is no leak + termination.
        assert!(data_lines.last().map(|l| l == "[DONE]").unwrap_or(false));
    }

    #[tokio::test]
    async fn streaming_redact_rewrites_delta() {
        let provider_stream =
            mock_provider_stream(vec![Ok(content_chunk("here is a badword in text"))]);
        let transpiler: Box<dyn PayloadTranspiler> = Box::new(OpenAiTranspiler);
        let g = outbound_keyword_guardrail(GuardrailAction::Redact, "badword");

        let transformed = transform_sse_stream_guarded(provider_stream, transpiler, Some(g), None);
        let data_lines = collect_data_lines(transformed).await;

        let chunk = data_lines
            .iter()
            .find(|l| l.contains("delta"))
            .expect("expected a content chunk");
        assert!(
            chunk.contains("[REDACTED]"),
            "delta not redacted: {}",
            chunk
        );
        assert!(!chunk.contains("badword"), "keyword leaked: {}", chunk);
    }

    /// Reassembles the assistant content the CLIENT actually receives across all
    /// emitted SSE data lines (concatenated delta.content), ignoring [DONE]/errors.
    fn client_visible_content(data_lines: &[String]) -> String {
        data_lines
            .iter()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .filter_map(|v| {
                v.get("choices")
                    .and_then(|c| c.get(0))
                    .and_then(|c| c.get("delta"))
                    .and_then(|d| d.get("content"))
                    .and_then(|c| c.as_str())
                    .map(str::to_string)
            })
            .collect()
    }

    #[tokio::test]
    async fn streaming_block_split_keyword_not_in_client_output() {
        // Sensitive word "blockme" split across two chunks under a BLOCK rule. The
        // reassembled client output must never contain it; the stream ends in an
        // error event.
        let provider_stream = mock_provider_stream(vec![
            Ok(content_chunk("start bloc")),
            Ok(content_chunk("kme tail")),
        ]);
        let transpiler: Box<dyn PayloadTranspiler> = Box::new(OpenAiTranspiler);
        let g = outbound_keyword_guardrail(GuardrailAction::Block, "blockme");

        let transformed = transform_sse_stream_guarded(provider_stream, transpiler, Some(g), None);
        let data_lines = collect_data_lines(transformed).await;
        let seen = client_visible_content(&data_lines);
        assert!(
            !seen.replace(' ', "").contains("blockme"),
            "client received the blocked keyword: {seen:?}"
        );
        assert!(
            data_lines.join("\n").contains("content_blocked"),
            "expected a guardrail block error event"
        );
    }

    #[tokio::test]
    async fn streaming_already_emitted_prefix_cannot_be_recalled() {
        // Precise guarantee: suppressing the chunk that COMPLETES a pattern cannot
        // retract bytes already sent in an earlier chunk. Here the first chunk's
        // benign text is delivered before the pattern completes in a later chunk;
        // we document that the earlier chunk is (correctly) already visible, while
        // the keyword itself never appears in the client output.
        let provider_stream = mock_provider_stream(vec![
            Ok(content_chunk("harmless preamble ")),
            Ok(content_chunk("lead")),
            Ok(content_chunk("ved tail")), // "leadved"? no; keyword is "leadved"
        ]);
        let transpiler: Box<dyn PayloadTranspiler> = Box::new(OpenAiTranspiler);
        let g = outbound_keyword_guardrail(GuardrailAction::Block, "leadved");

        let transformed = transform_sse_stream_guarded(provider_stream, transpiler, Some(g), None);
        let data_lines = collect_data_lines(transformed).await;
        let seen = client_visible_content(&data_lines);
        // The earlier benign chunk was delivered (cannot be recalled).
        assert!(
            seen.contains("harmless preamble"),
            "benign prefix should have been delivered: {seen:?}"
        );
        // The keyword never appears in full.
        assert!(
            !seen.replace(' ', "").contains("leadved"),
            "keyword leaked: {seen:?}"
        );
    }

    #[tokio::test]
    async fn streaming_redact_does_not_leak_keyword_split_across_chunks() {
        // "secret" is split across two chunks. The window detects it (Redacted),
        // but neither single delta contains the whole keyword. The partial halves
        // must NOT be emitted verbatim; the straddling delta's content is
        // suppressed instead (safe, bounded; no full-response buffering).
        let provider_stream = mock_provider_stream(vec![
            Ok(content_chunk("the sec")),
            Ok(content_chunk("ret value")),
        ]);
        let transpiler: Box<dyn PayloadTranspiler> = Box::new(OpenAiTranspiler);
        let g = outbound_keyword_guardrail(GuardrailAction::Redact, "secret");

        let transformed = transform_sse_stream_guarded(provider_stream, transpiler, Some(g), None);
        let data_lines = collect_data_lines(transformed).await;
        let joined: String = data_lines.join("");
        // Reassembled emitted content must not contain the keyword.
        let emitted: String = data_lines
            .iter()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .filter_map(|v| {
                v.get("choices")
                    .and_then(|c| c.get(0))
                    .and_then(|c| c.get("delta"))
                    .and_then(|d| d.get("content"))
                    .and_then(|c| c.as_str())
                    .map(str::to_string)
            })
            .collect();
        assert!(
            !emitted.replace(' ', "").contains("secret"),
            "split keyword leaked across chunks: emitted={emitted:?} raw={joined:?}"
        );
    }

    #[tokio::test]
    async fn streaming_block_detects_pattern_across_chunk_boundary() {
        // The keyword "blockme" is split across two chunks; the sliding window
        // must detect it once the full pattern has accumulated.
        let provider_stream = mock_provider_stream(vec![
            Ok(content_chunk("start bloc")),
            Ok(content_chunk("kme end")),
        ]);
        let transpiler: Box<dyn PayloadTranspiler> = Box::new(OpenAiTranspiler);
        let g = outbound_keyword_guardrail(GuardrailAction::Block, "blockme");

        let transformed = transform_sse_stream_guarded(provider_stream, transpiler, Some(g), None);
        let data_lines = collect_data_lines(transformed).await;
        let joined = data_lines.join("\n");

        assert!(
            joined.contains("content_blocked") || joined.contains("guardrail"),
            "cross-boundary pattern not blocked: {}",
            joined
        );
    }

    #[tokio::test]
    async fn streaming_without_guardrails_is_unchanged() {
        let provider_stream = mock_provider_stream(vec![Ok(content_chunk("hello world"))]);
        let transpiler: Box<dyn PayloadTranspiler> = Box::new(OpenAiTranspiler);

        let transformed = transform_sse_stream_guarded(provider_stream, transpiler, None, None);
        let data_lines = collect_data_lines(transformed).await;
        let joined = data_lines.join("\n");
        assert!(joined.contains("hello world"));
        assert!(!joined.contains("guardrail"));
    }
}
