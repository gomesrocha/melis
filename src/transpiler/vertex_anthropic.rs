//! Claude-via-Vertex-AI transpiler ("Claude on Vertex AI" / Model
//! Garden partner models).
//!
//! feature/catia-vertex-docker-readiness (Phase 5/6): distinct from
//! `vertex.rs` (`VertexTranspiler`), which targets Google's OWN
//! Generative Language API wire format (`contents`/`candidates`,
//! Gemini-shaped) -- Claude models hosted on Vertex AI use Anthropic's
//! native Messages API wire format almost unchanged, just:
//!
//! - the request body carries `anthropic_version` (fixed value
//!   `"vertex-2023-10-16"`) instead of Anthropic's direct-API
//!   `anthropic-version` HTTP header;
//! - the request body OMITS `model` entirely -- Vertex encodes the
//!   model in the URL path (`.../publishers/anthropic/models/{model}:rawPredict`),
//!   repeating it in the body is rejected;
//! - authentication is a Google OAuth2 bearer token (ADC), never
//!   `x-api-key` (see `vertex_auth.rs`);
//! - the RESPONSE and streaming-chunk shapes are IDENTICAL to
//!   Anthropic's direct API (same `content` blocks, same
//!   `message_start`/`content_block_delta`/`message_delta`/`message_stop`
//!   SSE event types) -- confirmed against Anthropic's own
//!   "Claude on Vertex AI" documentation, not assumed. This transpiler
//!   therefore delegates `from_native`/`translate_chunk` to
//!   `AnthropicTranspiler` unchanged, and only adapts `to_native`.

use serde_json::Value;

use super::anthropic::AnthropicTranspiler;
use super::{PayloadTranspiler, TranspilerError};

/// Vertex's own fixed request-body version marker for the Anthropic
/// Messages API surface (distinct from Anthropic's direct-API
/// `anthropic-version` HTTP header value, e.g. `"2023-06-01"`).
const VERTEX_ANTHROPIC_VERSION: &str = "vertex-2023-10-16";

/// Transpiler for Claude models served through Google Vertex AI.
pub struct VertexAnthropicTranspiler;

impl PayloadTranspiler for VertexAnthropicTranspiler {
    fn to_native(&self, request: &Value) -> Result<Value, TranspilerError> {
        let mut native = AnthropicTranspiler.to_native(request)?;

        let obj = native
            .as_object_mut()
            .ok_or_else(|| TranspilerError::InvalidFieldValue {
                field: "request".to_string(),
                reason: "Expected JSON object".to_string(),
            })?;

        // Vertex rejects a request body containing `model` -- the model
        // is already the last path segment of the request URL.
        obj.remove("model");
        obj.insert(
            "anthropic_version".to_string(),
            Value::String(VERTEX_ANTHROPIC_VERSION.to_string()),
        );

        Ok(native)
    }

    fn from_native(&self, response: &Value) -> Result<Value, TranspilerError> {
        AnthropicTranspiler.from_native(response)
    }

    fn translate_chunk(&self, chunk: &Value) -> Result<Value, TranspilerError> {
        AnthropicTranspiler.translate_chunk(chunk)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn to_native_strips_model_and_adds_vertex_anthropic_version() {
        let request = json!({
            "model": "catia-bootstrap",
            "messages": [{"role": "user", "content": "hello"}],
            "max_tokens": 512,
        });

        let native = VertexAnthropicTranspiler.to_native(&request).unwrap();

        assert!(native.get("model").is_none(), "model must be stripped for Vertex");
        assert_eq!(
            native.get("anthropic_version").and_then(|v| v.as_str()),
            Some(VERTEX_ANTHROPIC_VERSION)
        );
        assert_eq!(native.get("max_tokens").and_then(|v| v.as_u64()), Some(512));
        assert_eq!(
            native
                .get("messages")
                .and_then(|m| m.as_array())
                .map(|a| a.len()),
            Some(1)
        );
    }

    #[test]
    fn to_native_extracts_system_message_same_as_anthropic() {
        let request = json!({
            "model": "catia-spec",
            "messages": [
                {"role": "system", "content": "You are helpful."},
                {"role": "user", "content": "hi"}
            ],
        });

        let native = VertexAnthropicTranspiler.to_native(&request).unwrap();
        assert_eq!(
            native.get("system").and_then(|v| v.as_str()),
            Some("You are helpful.")
        );
    }

    #[test]
    fn to_native_missing_messages_is_error() {
        let request = json!({ "model": "catia-implementation" });
        let result = VertexAnthropicTranspiler.to_native(&request);
        assert!(result.is_err());
    }

    #[test]
    fn from_native_matches_anthropic_transpiler_output() {
        let response = json!({
            "id": "msg_123",
            "model": "claude-sonnet-4-6",
            "content": [{"type": "text", "text": "hi there"}],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 10, "output_tokens": 3}
        });

        let vertex_result = VertexAnthropicTranspiler.from_native(&response).unwrap();
        let anthropic_result = AnthropicTranspiler.from_native(&response).unwrap();
        assert_eq!(vertex_result, anthropic_result);

        assert_eq!(
            vertex_result["choices"][0]["message"]["content"],
            json!("hi there")
        );
        assert_eq!(vertex_result["usage"]["prompt_tokens"], json!(10));
        assert_eq!(vertex_result["usage"]["completion_tokens"], json!(3));
    }

    #[test]
    fn translate_chunk_matches_anthropic_transpiler_output() {
        let chunk = json!({
            "type": "content_block_delta",
            "delta": {"text": "partial"}
        });

        let vertex_result = VertexAnthropicTranspiler.translate_chunk(&chunk).unwrap();
        let anthropic_result = AnthropicTranspiler.translate_chunk(&chunk).unwrap();
        assert_eq!(vertex_result, anthropic_result);
    }
}
