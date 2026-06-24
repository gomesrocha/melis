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
    let stream = async_stream::stream! {
        let mut stream = provider_stream;

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
}
