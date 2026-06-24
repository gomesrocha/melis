//! Anthropic Messages API transpiler.
//!
//! Translates OpenAI ↔ Anthropic Messages API format.
//!
//! Key differences between OpenAI and Anthropic formats:
//! - System messages are extracted to a top-level `system` field (not in messages array)
//! - Messages use roles "user" and "assistant" only (no "system" in messages)
//! - `max_tokens` is required (defaults to 4096 if absent from OpenAI request)
//! - `stop` (string or array) maps to `stop_sequences` (array)
//! - Response `choices` → `content` array, `finish_reason` → `stop_reason`
//! - Streaming chunks use `content_block_delta` type with `text_delta`

use serde_json::{json, Value};
use tracing::warn;

use super::{PayloadTranspiler, TranspilerError};

/// Default max_tokens value when not specified in the OpenAI request.
/// Anthropic requires this field, so we provide a sensible default.
const DEFAULT_MAX_TOKENS: u64 = 4096;

/// Transpiler for the Anthropic Messages API.
///
/// Handles translation between OpenAI's chat completions format and
/// Anthropic's Messages API format.
pub struct AnthropicTranspiler;

impl AnthropicTranspiler {
    /// Extracts system messages from the messages array and returns them
    /// as a single concatenated string.
    fn extract_system_message(messages: &[Value]) -> Option<String> {
        let system_parts: Vec<&str> = messages
            .iter()
            .filter_map(|msg| {
                if msg.get("role").and_then(|r| r.as_str()) == Some("system") {
                    msg.get("content").and_then(|c| c.as_str())
                } else {
                    None
                }
            })
            .collect();

        if system_parts.is_empty() {
            None
        } else {
            Some(system_parts.join("\n"))
        }
    }

    /// Filters out system messages and maps roles for the Anthropic messages array.
    fn convert_messages_to_native(messages: &[Value]) -> Vec<Value> {
        messages
            .iter()
            .filter(|msg| {
                msg.get("role").and_then(|r| r.as_str()) != Some("system")
            })
            .map(|msg| {
                let role = msg
                    .get("role")
                    .and_then(|r| r.as_str())
                    .unwrap_or("user");

                // Anthropic only supports "user" and "assistant" roles
                let mapped_role = match role {
                    "assistant" => "assistant",
                    _ => "user", // "user", "function", "tool" → "user"
                };

                json!({
                    "role": mapped_role,
                    "content": msg.get("content").cloned().unwrap_or(Value::Null)
                })
            })
            .collect()
    }

    /// Converts OpenAI `stop` field (string or array) into Anthropic `stop_sequences` (array).
    fn convert_stop_sequences(stop: &Value) -> Option<Vec<Value>> {
        match stop {
            Value::String(s) => Some(vec![Value::String(s.clone())]),
            Value::Array(arr) => Some(arr.clone()),
            _ => None,
        }
    }

    /// Maps Anthropic `stop_reason` to OpenAI `finish_reason`.
    fn map_stop_reason(stop_reason: Option<&str>) -> &'static str {
        match stop_reason {
            Some("end_turn") => "stop",
            Some("max_tokens") => "length",
            Some("stop_sequence") => "stop",
            _ => "stop",
        }
    }
}

impl PayloadTranspiler for AnthropicTranspiler {
    fn to_native(&self, request: &Value) -> Result<Value, TranspilerError> {
        let obj = request
            .as_object()
            .ok_or_else(|| TranspilerError::InvalidFieldValue {
                field: "request".to_string(),
                reason: "Expected JSON object".to_string(),
            })?;

        let messages = obj
            .get("messages")
            .and_then(|m| m.as_array())
            .ok_or_else(|| TranspilerError::MissingField {
                field: "messages".to_string(),
            })?;

        let model = obj.get("model").ok_or_else(|| TranspilerError::MissingField {
            field: "model".to_string(),
        })?;

        // Build the Anthropic native request
        let mut native = serde_json::Map::new();

        // Model maps directly
        native.insert("model".to_string(), model.clone());

        // Extract system message to top-level field
        if let Some(system) = Self::extract_system_message(messages) {
            native.insert("system".to_string(), Value::String(system));
        }

        // Convert messages (filter system, map roles)
        let native_messages = Self::convert_messages_to_native(messages);
        native.insert("messages".to_string(), Value::Array(native_messages));

        // max_tokens is REQUIRED for Anthropic - default to 4096 if not present
        let max_tokens = obj
            .get("max_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(DEFAULT_MAX_TOKENS);
        native.insert("max_tokens".to_string(), json!(max_tokens));

        // Temperature maps directly (optional)
        if let Some(temp) = obj.get("temperature") {
            native.insert("temperature".to_string(), temp.clone());
        }

        // stop → stop_sequences
        if let Some(stop) = obj.get("stop") {
            if let Some(sequences) = Self::convert_stop_sequences(stop) {
                native.insert("stop_sequences".to_string(), Value::Array(sequences));
            }
        }

        // stream maps directly (optional)
        if let Some(stream) = obj.get("stream") {
            native.insert("stream".to_string(), stream.clone());
        }

        // Log warnings for unsupported fields that are being omitted
        let supported_fields = [
            "model",
            "messages",
            "max_tokens",
            "temperature",
            "stop",
            "stream",
        ];
        for key in obj.keys() {
            if !supported_fields.contains(&key.as_str()) {
                warn!(
                    field = %key,
                    provider = "anthropic",
                    "Omitting unsupported field during OpenAI→Anthropic conversion"
                );
            }
        }

        Ok(Value::Object(native))
    }

    fn from_native(&self, response: &Value) -> Result<Value, TranspilerError> {
        let obj = response
            .as_object()
            .ok_or_else(|| TranspilerError::InvalidFieldValue {
                field: "response".to_string(),
                reason: "Expected JSON object".to_string(),
            })?;

        // Extract id
        let id = obj
            .get("id")
            .cloned()
            .unwrap_or_else(|| json!("chatcmpl-unknown"));

        // Extract model
        let model = obj
            .get("model")
            .cloned()
            .unwrap_or_else(|| json!("unknown"));

        // Extract content text from Anthropic's content array
        let content_text = obj
            .get("content")
            .and_then(|c| c.as_array())
            .map(|blocks| {
                blocks
                    .iter()
                    .filter_map(|block| {
                        if block.get("type").and_then(|t| t.as_str()) == Some("text") {
                            block.get("text").and_then(|t| t.as_str())
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("")
            })
            .unwrap_or_default();

        // Map stop_reason → finish_reason
        let stop_reason = obj.get("stop_reason").and_then(|s| s.as_str());
        let finish_reason = Self::map_stop_reason(stop_reason);

        // Extract usage
        let usage = obj.get("usage").cloned().unwrap_or_else(|| {
            json!({
                "input_tokens": 0,
                "output_tokens": 0
            })
        });

        let input_tokens = usage
            .get("input_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let output_tokens = usage
            .get("output_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);

        // Build OpenAI format response
        let openai_response = json!({
            "id": id,
            "object": "chat.completion",
            "created": 0,
            "model": model,
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": content_text
                },
                "finish_reason": finish_reason
            }],
            "usage": {
                "prompt_tokens": input_tokens,
                "completion_tokens": output_tokens,
                "total_tokens": input_tokens + output_tokens
            }
        });

        Ok(openai_response)
    }

    fn translate_chunk(&self, chunk: &Value) -> Result<Value, TranspilerError> {
        let obj = chunk
            .as_object()
            .ok_or_else(|| TranspilerError::InvalidFieldValue {
                field: "chunk".to_string(),
                reason: "Expected JSON object".to_string(),
            })?;

        let chunk_type = obj.get("type").and_then(|t| t.as_str()).unwrap_or("");

        match chunk_type {
            "content_block_delta" => {
                // Extract text from delta
                let text = obj
                    .get("delta")
                    .and_then(|d| d.get("text"))
                    .and_then(|t| t.as_str())
                    .unwrap_or("");

                Ok(json!({
                    "id": "chatcmpl-stream",
                    "object": "chat.completion.chunk",
                    "created": 0,
                    "model": "",
                    "choices": [{
                        "index": 0,
                        "delta": {
                            "content": text
                        },
                        "finish_reason": null
                    }]
                }))
            }
            "message_start" => {
                // First chunk - includes role
                let model = obj
                    .get("message")
                    .and_then(|m| m.get("model"))
                    .and_then(|m| m.as_str())
                    .unwrap_or("");

                Ok(json!({
                    "id": "chatcmpl-stream",
                    "object": "chat.completion.chunk",
                    "created": 0,
                    "model": model,
                    "choices": [{
                        "index": 0,
                        "delta": {
                            "role": "assistant"
                        },
                        "finish_reason": null
                    }]
                }))
            }
            "message_delta" => {
                // Final chunk - includes stop_reason
                let stop_reason = obj
                    .get("delta")
                    .and_then(|d| d.get("stop_reason"))
                    .and_then(|s| s.as_str());
                let finish_reason = Self::map_stop_reason(stop_reason);

                Ok(json!({
                    "id": "chatcmpl-stream",
                    "object": "chat.completion.chunk",
                    "created": 0,
                    "model": "",
                    "choices": [{
                        "index": 0,
                        "delta": {},
                        "finish_reason": finish_reason
                    }]
                }))
            }
            "message_stop" => {
                // Stream end marker
                Ok(json!({
                    "id": "chatcmpl-stream",
                    "object": "chat.completion.chunk",
                    "created": 0,
                    "model": "",
                    "choices": [{
                        "index": 0,
                        "delta": {},
                        "finish_reason": "stop"
                    }]
                }))
            }
            _ => {
                // Other chunk types (content_block_start, ping, etc.) - pass through as empty delta
                warn!(
                    chunk_type = %chunk_type,
                    "Unhandled Anthropic chunk type, translating as empty delta"
                );
                Ok(json!({
                    "id": "chatcmpl-stream",
                    "object": "chat.completion.chunk",
                    "created": 0,
                    "model": "",
                    "choices": [{
                        "index": 0,
                        "delta": {},
                        "finish_reason": null
                    }]
                }))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_to_native_basic_conversion() {
        let transpiler = AnthropicTranspiler;
        let openai_request = json!({
            "model": "claude-sonnet-4-20250514",
            "messages": [
                {"role": "user", "content": "Hello, how are you?"}
            ],
            "max_tokens": 1024,
            "temperature": 0.7
        });

        let native = transpiler.to_native(&openai_request).unwrap();

        assert_eq!(native["model"], "claude-sonnet-4-20250514");
        assert_eq!(native["max_tokens"], 1024);
        assert_eq!(native["temperature"], 0.7);
        assert_eq!(native["messages"][0]["role"], "user");
        assert_eq!(native["messages"][0]["content"], "Hello, how are you?");
        // No system field when there's no system message
        assert!(native.get("system").is_none());
    }

    #[test]
    fn test_to_native_system_message_extraction() {
        let transpiler = AnthropicTranspiler;
        let openai_request = json!({
            "model": "claude-sonnet-4-20250514",
            "messages": [
                {"role": "system", "content": "You are a helpful assistant."},
                {"role": "user", "content": "Hello!"}
            ],
            "max_tokens": 512
        });

        let native = transpiler.to_native(&openai_request).unwrap();

        // System message extracted to top-level field
        assert_eq!(native["system"], "You are a helpful assistant.");
        // Messages array should NOT contain system message
        assert_eq!(native["messages"].as_array().unwrap().len(), 1);
        assert_eq!(native["messages"][0]["role"], "user");
        assert_eq!(native["messages"][0]["content"], "Hello!");
    }

    #[test]
    fn test_to_native_multiple_system_messages_concatenated() {
        let transpiler = AnthropicTranspiler;
        let openai_request = json!({
            "model": "claude-sonnet-4-20250514",
            "messages": [
                {"role": "system", "content": "You are helpful."},
                {"role": "system", "content": "Be concise."},
                {"role": "user", "content": "Hi"}
            ],
            "max_tokens": 256
        });

        let native = transpiler.to_native(&openai_request).unwrap();

        assert_eq!(native["system"], "You are helpful.\nBe concise.");
        assert_eq!(native["messages"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn test_to_native_max_tokens_defaults_to_4096() {
        let transpiler = AnthropicTranspiler;
        let openai_request = json!({
            "model": "claude-sonnet-4-20250514",
            "messages": [
                {"role": "user", "content": "Hello"}
            ]
        });

        let native = transpiler.to_native(&openai_request).unwrap();

        // max_tokens defaults to 4096 when not specified
        assert_eq!(native["max_tokens"], 4096);
    }

    #[test]
    fn test_to_native_stop_sequences_from_array() {
        let transpiler = AnthropicTranspiler;
        let openai_request = json!({
            "model": "claude-sonnet-4-20250514",
            "messages": [
                {"role": "user", "content": "Hello"}
            ],
            "max_tokens": 100,
            "stop": ["END", "STOP"]
        });

        let native = transpiler.to_native(&openai_request).unwrap();

        let stop_sequences = native["stop_sequences"].as_array().unwrap();
        assert_eq!(stop_sequences.len(), 2);
        assert_eq!(stop_sequences[0], "END");
        assert_eq!(stop_sequences[1], "STOP");
    }

    #[test]
    fn test_to_native_stop_sequences_from_string() {
        let transpiler = AnthropicTranspiler;
        let openai_request = json!({
            "model": "claude-sonnet-4-20250514",
            "messages": [
                {"role": "user", "content": "Hello"}
            ],
            "max_tokens": 100,
            "stop": "END"
        });

        let native = transpiler.to_native(&openai_request).unwrap();

        let stop_sequences = native["stop_sequences"].as_array().unwrap();
        assert_eq!(stop_sequences.len(), 1);
        assert_eq!(stop_sequences[0], "END");
    }

    #[test]
    fn test_to_native_unsupported_fields_omitted() {
        let transpiler = AnthropicTranspiler;
        let openai_request = json!({
            "model": "claude-sonnet-4-20250514",
            "messages": [
                {"role": "user", "content": "Hello"}
            ],
            "max_tokens": 100,
            "frequency_penalty": 0.5,
            "presence_penalty": 0.3,
            "logprobs": true,
            "top_logprobs": 5
        });

        let native = transpiler.to_native(&openai_request).unwrap();

        // Unsupported fields should NOT appear in the native payload
        assert!(native.get("frequency_penalty").is_none());
        assert!(native.get("presence_penalty").is_none());
        assert!(native.get("logprobs").is_none());
        assert!(native.get("top_logprobs").is_none());
    }

    #[test]
    fn test_from_native_basic_response() {
        let transpiler = AnthropicTranspiler;
        let anthropic_response = json!({
            "id": "msg_01XFDUDYJgAACzvnptvVoYEL",
            "type": "message",
            "role": "assistant",
            "content": [
                {"type": "text", "text": "Hello! How can I help you today?"}
            ],
            "model": "claude-sonnet-4-20250514",
            "stop_reason": "end_turn",
            "usage": {
                "input_tokens": 10,
                "output_tokens": 25
            }
        });

        let openai = transpiler.from_native(&anthropic_response).unwrap();

        assert_eq!(openai["id"], "msg_01XFDUDYJgAACzvnptvVoYEL");
        assert_eq!(openai["object"], "chat.completion");
        assert_eq!(openai["model"], "claude-sonnet-4-20250514");
        assert_eq!(openai["choices"][0]["message"]["role"], "assistant");
        assert_eq!(
            openai["choices"][0]["message"]["content"],
            "Hello! How can I help you today?"
        );
        assert_eq!(openai["choices"][0]["finish_reason"], "stop");
        assert_eq!(openai["usage"]["prompt_tokens"], 10);
        assert_eq!(openai["usage"]["completion_tokens"], 25);
        assert_eq!(openai["usage"]["total_tokens"], 35);
    }

    #[test]
    fn test_from_native_max_tokens_stop_reason() {
        let transpiler = AnthropicTranspiler;
        let anthropic_response = json!({
            "id": "msg_abc",
            "type": "message",
            "role": "assistant",
            "content": [
                {"type": "text", "text": "Truncated response..."}
            ],
            "model": "claude-sonnet-4-20250514",
            "stop_reason": "max_tokens",
            "usage": {
                "input_tokens": 50,
                "output_tokens": 100
            }
        });

        let openai = transpiler.from_native(&anthropic_response).unwrap();

        assert_eq!(openai["choices"][0]["finish_reason"], "length");
    }

    #[test]
    fn test_translate_chunk_content_block_delta() {
        let transpiler = AnthropicTranspiler;
        let anthropic_chunk = json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": {
                "type": "text_delta",
                "text": "Hello"
            }
        });

        let openai_chunk = transpiler.translate_chunk(&anthropic_chunk).unwrap();

        assert_eq!(openai_chunk["object"], "chat.completion.chunk");
        assert_eq!(openai_chunk["choices"][0]["delta"]["content"], "Hello");
        assert!(openai_chunk["choices"][0]["finish_reason"].is_null());
    }

    #[test]
    fn test_translate_chunk_message_start() {
        let transpiler = AnthropicTranspiler;
        let anthropic_chunk = json!({
            "type": "message_start",
            "message": {
                "id": "msg_abc",
                "type": "message",
                "role": "assistant",
                "model": "claude-sonnet-4-20250514",
                "content": [],
                "stop_reason": null,
                "usage": {"input_tokens": 10, "output_tokens": 0}
            }
        });

        let openai_chunk = transpiler.translate_chunk(&anthropic_chunk).unwrap();

        assert_eq!(openai_chunk["object"], "chat.completion.chunk");
        assert_eq!(openai_chunk["choices"][0]["delta"]["role"], "assistant");
        assert_eq!(openai_chunk["model"], "claude-sonnet-4-20250514");
    }

    #[test]
    fn test_translate_chunk_message_delta_stop() {
        let transpiler = AnthropicTranspiler;
        let anthropic_chunk = json!({
            "type": "message_delta",
            "delta": {
                "stop_reason": "end_turn"
            },
            "usage": {
                "output_tokens": 50
            }
        });

        let openai_chunk = transpiler.translate_chunk(&anthropic_chunk).unwrap();

        assert_eq!(openai_chunk["choices"][0]["finish_reason"], "stop");
    }

    #[test]
    fn test_translate_chunk_unknown_type() {
        let transpiler = AnthropicTranspiler;
        let anthropic_chunk = json!({
            "type": "ping"
        });

        let openai_chunk = transpiler.translate_chunk(&anthropic_chunk).unwrap();

        // Unknown types translated as empty delta
        assert_eq!(openai_chunk["object"], "chat.completion.chunk");
        assert!(openai_chunk["choices"][0]["finish_reason"].is_null());
    }

    #[test]
    fn test_to_native_role_mapping() {
        let transpiler = AnthropicTranspiler;
        let openai_request = json!({
            "model": "claude-sonnet-4-20250514",
            "messages": [
                {"role": "system", "content": "Be helpful"},
                {"role": "user", "content": "Hello"},
                {"role": "assistant", "content": "Hi there!"},
                {"role": "user", "content": "How are you?"}
            ],
            "max_tokens": 100
        });

        let native = transpiler.to_native(&openai_request).unwrap();
        let messages = native["messages"].as_array().unwrap();

        // System message should not be in messages array
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[1]["role"], "assistant");
        assert_eq!(messages[2]["role"], "user");
    }

    #[test]
    fn test_to_native_stream_passed_through() {
        let transpiler = AnthropicTranspiler;
        let openai_request = json!({
            "model": "claude-sonnet-4-20250514",
            "messages": [
                {"role": "user", "content": "Hello"}
            ],
            "stream": true
        });

        let native = transpiler.to_native(&openai_request).unwrap();

        assert_eq!(native["stream"], true);
    }

    #[test]
    fn test_to_native_missing_messages_returns_error() {
        let transpiler = AnthropicTranspiler;
        let openai_request = json!({
            "model": "claude-sonnet-4-20250514"
        });

        let result = transpiler.to_native(&openai_request);
        assert!(result.is_err());
        match result.unwrap_err() {
            TranspilerError::MissingField { field } => assert_eq!(field, "messages"),
            _ => panic!("Expected MissingField error"),
        }
    }

    #[test]
    fn test_to_native_missing_model_returns_error() {
        let transpiler = AnthropicTranspiler;
        let openai_request = json!({
            "messages": [
                {"role": "user", "content": "Hello"}
            ]
        });

        let result = transpiler.to_native(&openai_request);
        assert!(result.is_err());
        match result.unwrap_err() {
            TranspilerError::MissingField { field } => assert_eq!(field, "model"),
            _ => panic!("Expected MissingField error"),
        }
    }

    #[test]
    fn test_from_native_multiple_content_blocks() {
        let transpiler = AnthropicTranspiler;
        let anthropic_response = json!({
            "id": "msg_xyz",
            "type": "message",
            "role": "assistant",
            "content": [
                {"type": "text", "text": "First part. "},
                {"type": "text", "text": "Second part."}
            ],
            "model": "claude-sonnet-4-20250514",
            "stop_reason": "end_turn",
            "usage": {
                "input_tokens": 10,
                "output_tokens": 20
            }
        });

        let openai = transpiler.from_native(&anthropic_response).unwrap();

        assert_eq!(
            openai["choices"][0]["message"]["content"],
            "First part. Second part."
        );
    }
}
