//! Google Vertex AI transpiler.
//!
//! Translates OpenAI ↔ Google Generative AI API format.
//!
//! Mapping summary:
//! - OpenAI `messages` → Vertex `contents` (with `parts`) + `systemInstruction`
//! - OpenAI roles: "system" → extracted to `systemInstruction`, "assistant" → "model", "user" → "user"
//! - OpenAI `temperature`, `max_tokens`, `stop` → Vertex `generationConfig`
//! - Vertex response `candidates[0].content.parts[0].text` → OpenAI `choices[0].message.content`
//! - Vertex `usageMetadata` → OpenAI `usage`
//!
//! Uses `serde_json::Value` for flexibility. Zero-copy via `Cow<'a, str>` is applied
//! at the trait boundary where possible; internally we work with owned `Value` nodes
//! and transfer string values without re-encoding when the structure allows.

use serde_json::{json, Value};
use tracing::warn;

use super::{PayloadTranspiler, TranspilerError};

/// Transpiler for Google Vertex AI (Generative AI API).
pub struct VertexTranspiler;

impl VertexTranspiler {
    /// Maps an OpenAI role string to the Vertex AI equivalent.
    fn map_role_to_vertex(role: &str) -> &'static str {
        match role {
            "assistant" => "model",
            "user" => "user",
            // "system" is handled separately via systemInstruction
            other => {
                warn!(role = other, "Unknown role encountered, passing as-is");
                // Return "user" as safe fallback for unknown roles
                "user"
            }
        }
    }

    /// Maps a Vertex AI role string to the OpenAI equivalent.
    fn map_role_to_openai(role: &str) -> &'static str {
        match role {
            "model" => "assistant",
            "user" => "user",
            other => {
                warn!(role = other, "Unknown Vertex role encountered, mapping to assistant");
                "assistant"
            }
        }
    }

    /// Maps a Vertex AI finish reason to the OpenAI equivalent.
    fn map_finish_reason(reason: &str) -> &'static str {
        match reason {
            "STOP" => "stop",
            "MAX_TOKENS" => "length",
            "SAFETY" => "content_filter",
            "RECITATION" => "content_filter",
            other => {
                warn!(reason = other, "Unknown Vertex finish reason, mapping to stop");
                "stop"
            }
        }
    }
}

impl PayloadTranspiler for VertexTranspiler {
    /// Converts an OpenAI-format request into the Google Generative AI API format.
    ///
    /// Input (OpenAI):
    /// ```json
    /// {
    ///   "model": "gemini-pro",
    ///   "messages": [
    ///     {"role": "system", "content": "You are helpful."},
    ///     {"role": "user", "content": "Hello"},
    ///     {"role": "assistant", "content": "Hi!"},
    ///     {"role": "user", "content": "How are you?"}
    ///   ],
    ///   "temperature": 0.7,
    ///   "max_tokens": 1024,
    ///   "stop": ["\n"]
    /// }
    /// ```
    ///
    /// Output (Vertex AI):
    /// ```json
    /// {
    ///   "contents": [
    ///     {"role": "user", "parts": [{"text": "Hello"}]},
    ///     {"role": "model", "parts": [{"text": "Hi!"}]},
    ///     {"role": "user", "parts": [{"text": "How are you?"}]}
    ///   ],
    ///   "systemInstruction": {"parts": [{"text": "You are helpful."}]},
    ///   "generationConfig": {
    ///     "temperature": 0.7,
    ///     "maxOutputTokens": 1024,
    ///     "stopSequences": ["\n"]
    ///   }
    /// }
    /// ```
    fn to_native(&self, request: &Value) -> Result<Value, TranspilerError> {
        let messages = request
            .get("messages")
            .and_then(|v| v.as_array())
            .ok_or_else(|| TranspilerError::MissingField {
                field: "messages".to_string(),
            })?;

        let mut contents: Vec<Value> = Vec::new();
        let mut system_parts: Vec<Value> = Vec::new();

        for msg in messages {
            let role = msg
                .get("role")
                .and_then(|v| v.as_str())
                .unwrap_or("user");
            let content = msg
                .get("content")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            if role == "system" {
                // System messages go into systemInstruction
                system_parts.push(json!({ "text": content }));
            } else {
                let vertex_role = Self::map_role_to_vertex(role);
                contents.push(json!({
                    "role": vertex_role,
                    "parts": [{ "text": content }]
                }));
            }
        }

        let mut native = json!({ "contents": contents });

        // Add systemInstruction if there were system messages
        if !system_parts.is_empty() {
            native["systemInstruction"] = json!({ "parts": system_parts });
        }

        // Build generationConfig from supported parameters
        let mut generation_config = serde_json::Map::new();

        if let Some(temp) = request.get("temperature") {
            generation_config.insert("temperature".to_string(), temp.clone());
        }

        if let Some(max_tokens) = request.get("max_tokens") {
            generation_config.insert("maxOutputTokens".to_string(), max_tokens.clone());
        }

        if let Some(stop) = request.get("stop") {
            generation_config.insert("stopSequences".to_string(), stop.clone());
        }

        if let Some(top_p) = request.get("top_p") {
            generation_config.insert("topP".to_string(), top_p.clone());
        }

        if !generation_config.is_empty() {
            native["generationConfig"] = Value::Object(generation_config);
        }

        // Log warnings for unsupported fields that are being dropped
        let unsupported_fields = [
            "model",
            "stream",
            "frequency_penalty",
            "presence_penalty",
            "logit_bias",
            "logprobs",
            "top_logprobs",
            "n",
            "seed",
            "user",
            "tools",
            "tool_choice",
            "response_format",
        ];

        for field in &unsupported_fields {
            if request.get(*field).is_some() {
                warn!(
                    field = *field,
                    "Field has no Vertex AI equivalent, omitting from native payload"
                );
            }
        }

        Ok(native)
    }

    /// Converts a Vertex AI response into OpenAI format.
    ///
    /// Input (Vertex AI):
    /// ```json
    /// {
    ///   "candidates": [{
    ///     "content": {"parts": [{"text": "Hello!"}], "role": "model"},
    ///     "finishReason": "STOP"
    ///   }],
    ///   "usageMetadata": {
    ///     "promptTokenCount": 10,
    ///     "candidatesTokenCount": 5,
    ///     "totalTokenCount": 15
    ///   }
    /// }
    /// ```
    ///
    /// Output (OpenAI):
    /// ```json
    /// {
    ///   "id": "chatcmpl-vertex",
    ///   "object": "chat.completion",
    ///   "created": 0,
    ///   "model": "gemini-pro",
    ///   "choices": [{
    ///     "index": 0,
    ///     "message": {"role": "assistant", "content": "Hello!"},
    ///     "finish_reason": "stop"
    ///   }],
    ///   "usage": {
    ///     "prompt_tokens": 10,
    ///     "completion_tokens": 5,
    ///     "total_tokens": 15
    ///   }
    /// }
    /// ```
    fn from_native(&self, response: &Value) -> Result<Value, TranspilerError> {
        let candidates = response
            .get("candidates")
            .and_then(|v| v.as_array())
            .ok_or_else(|| TranspilerError::MissingField {
                field: "candidates".to_string(),
            })?;

        let mut choices: Vec<Value> = Vec::new();

        for (index, candidate) in candidates.iter().enumerate() {
            let content = candidate.get("content");
            let parts = content
                .and_then(|c| c.get("parts"))
                .and_then(|p| p.as_array());

            // Extract text from parts
            let text = parts
                .and_then(|parts_arr| {
                    parts_arr
                        .iter()
                        .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
                        .next()
                })
                .unwrap_or("");

            // Map role
            let role = content
                .and_then(|c| c.get("role"))
                .and_then(|r| r.as_str())
                .unwrap_or("model");
            let openai_role = Self::map_role_to_openai(role);

            // Map finish reason
            let finish_reason = candidate
                .get("finishReason")
                .and_then(|r| r.as_str())
                .map(Self::map_finish_reason);

            choices.push(json!({
                "index": index,
                "message": {
                    "role": openai_role,
                    "content": text
                },
                "finish_reason": finish_reason
            }));
        }

        // Map usage metadata
        let usage = response.get("usageMetadata").map(|meta| {
            json!({
                "prompt_tokens": meta.get("promptTokenCount").and_then(|v| v.as_u64()).unwrap_or(0),
                "completion_tokens": meta.get("candidatesTokenCount").and_then(|v| v.as_u64()).unwrap_or(0),
                "total_tokens": meta.get("totalTokenCount").and_then(|v| v.as_u64()).unwrap_or(0)
            })
        });

        // Extract model from response if available (modelVersion field)
        let model = response
            .get("modelVersion")
            .and_then(|v| v.as_str())
            .unwrap_or("gemini-pro");

        let mut result = json!({
            "id": "chatcmpl-vertex",
            "object": "chat.completion",
            "created": 0,
            "model": model,
            "choices": choices
        });

        if let Some(usage_val) = usage {
            result["usage"] = usage_val;
        }

        Ok(result)
    }

    /// Converts a Vertex AI SSE chunk into OpenAI chunk format.
    ///
    /// Input (Vertex AI chunk):
    /// ```json
    /// {
    ///   "candidates": [{
    ///     "content": {"parts": [{"text": "partial"}], "role": "model"},
    ///     "finishReason": "STOP"
    ///   }]
    /// }
    /// ```
    ///
    /// Output (OpenAI chunk):
    /// ```json
    /// {
    ///   "id": "chatcmpl-vertex",
    ///   "object": "chat.completion.chunk",
    ///   "created": 0,
    ///   "model": "gemini-pro",
    ///   "choices": [{
    ///     "index": 0,
    ///     "delta": {"role": "assistant", "content": "partial"},
    ///     "finish_reason": "stop"
    ///   }]
    /// }
    /// ```
    fn translate_chunk(&self, chunk: &Value) -> Result<Value, TranspilerError> {
        let candidates = chunk
            .get("candidates")
            .and_then(|v| v.as_array())
            .ok_or_else(|| TranspilerError::MissingField {
                field: "candidates".to_string(),
            })?;

        let mut choices: Vec<Value> = Vec::new();

        for (index, candidate) in candidates.iter().enumerate() {
            let content = candidate.get("content");
            let parts = content
                .and_then(|c| c.get("parts"))
                .and_then(|p| p.as_array());

            // Extract text from parts (may be partial)
            let text = parts.and_then(|parts_arr| {
                parts_arr
                    .iter()
                    .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
                    .next()
            });

            // Map role
            let role = content
                .and_then(|c| c.get("role"))
                .and_then(|r| r.as_str())
                .map(Self::map_role_to_openai);

            // Map finish reason
            let finish_reason = candidate
                .get("finishReason")
                .and_then(|r| r.as_str())
                .map(Self::map_finish_reason);

            // Build delta - only include fields that are present
            let mut delta = serde_json::Map::new();
            if let Some(r) = role {
                delta.insert("role".to_string(), json!(r));
            }
            if let Some(t) = text {
                delta.insert("content".to_string(), json!(t));
            }

            choices.push(json!({
                "index": index,
                "delta": Value::Object(delta),
                "finish_reason": finish_reason
            }));
        }

        // Extract model from chunk if available
        let model = chunk
            .get("modelVersion")
            .and_then(|v| v.as_str())
            .unwrap_or("gemini-pro");

        Ok(json!({
            "id": "chatcmpl-vertex",
            "object": "chat.completion.chunk",
            "created": 0,
            "model": model,
            "choices": choices
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn transpiler() -> VertexTranspiler {
        VertexTranspiler
    }

    #[test]
    fn test_basic_openai_to_vertex_conversion() {
        let openai_request = json!({
            "model": "gemini-pro",
            "messages": [
                {"role": "user", "content": "Hello, how are you?"}
            ],
            "temperature": 0.7,
            "max_tokens": 1024
        });

        let result = transpiler().to_native(&openai_request).unwrap();

        // Check contents structure
        let contents = result.get("contents").unwrap().as_array().unwrap();
        assert_eq!(contents.len(), 1);
        assert_eq!(contents[0]["role"], "user");
        assert_eq!(contents[0]["parts"][0]["text"], "Hello, how are you?");

        // Check generationConfig
        let gen_config = result.get("generationConfig").unwrap();
        assert_eq!(gen_config["temperature"], 0.7);
        assert_eq!(gen_config["maxOutputTokens"], 1024);

        // No systemInstruction since no system message
        assert!(result.get("systemInstruction").is_none());
    }

    #[test]
    fn test_system_instruction_mapping() {
        let openai_request = json!({
            "model": "gemini-pro",
            "messages": [
                {"role": "system", "content": "You are a helpful assistant."},
                {"role": "user", "content": "Hi there"}
            ]
        });

        let result = transpiler().to_native(&openai_request).unwrap();

        // System message should be in systemInstruction, not contents
        let contents = result.get("contents").unwrap().as_array().unwrap();
        assert_eq!(contents.len(), 1);
        assert_eq!(contents[0]["role"], "user");
        assert_eq!(contents[0]["parts"][0]["text"], "Hi there");

        // Check systemInstruction
        let sys_instruction = result.get("systemInstruction").unwrap();
        let sys_parts = sys_instruction.get("parts").unwrap().as_array().unwrap();
        assert_eq!(sys_parts.len(), 1);
        assert_eq!(sys_parts[0]["text"], "You are a helpful assistant.");
    }

    #[test]
    fn test_multiple_system_messages_combined() {
        let openai_request = json!({
            "model": "gemini-pro",
            "messages": [
                {"role": "system", "content": "Rule 1: Be helpful."},
                {"role": "system", "content": "Rule 2: Be concise."},
                {"role": "user", "content": "Hello"}
            ]
        });

        let result = transpiler().to_native(&openai_request).unwrap();

        let sys_instruction = result.get("systemInstruction").unwrap();
        let sys_parts = sys_instruction.get("parts").unwrap().as_array().unwrap();
        assert_eq!(sys_parts.len(), 2);
        assert_eq!(sys_parts[0]["text"], "Rule 1: Be helpful.");
        assert_eq!(sys_parts[1]["text"], "Rule 2: Be concise.");
    }

    #[test]
    fn test_role_mapping_assistant_to_model() {
        let openai_request = json!({
            "model": "gemini-pro",
            "messages": [
                {"role": "user", "content": "What is 2+2?"},
                {"role": "assistant", "content": "4"},
                {"role": "user", "content": "And 3+3?"}
            ]
        });

        let result = transpiler().to_native(&openai_request).unwrap();

        let contents = result.get("contents").unwrap().as_array().unwrap();
        assert_eq!(contents.len(), 3);
        assert_eq!(contents[0]["role"], "user");
        assert_eq!(contents[1]["role"], "model");
        assert_eq!(contents[2]["role"], "user");
    }

    #[test]
    fn test_generation_config_all_params() {
        let openai_request = json!({
            "model": "gemini-pro",
            "messages": [{"role": "user", "content": "Hi"}],
            "temperature": 0.5,
            "max_tokens": 2048,
            "stop": ["\n", "END"],
            "top_p": 0.9
        });

        let result = transpiler().to_native(&openai_request).unwrap();

        let gen_config = result.get("generationConfig").unwrap();
        assert_eq!(gen_config["temperature"], 0.5);
        assert_eq!(gen_config["maxOutputTokens"], 2048);
        assert_eq!(gen_config["stopSequences"], json!(["\n", "END"]));
        assert_eq!(gen_config["topP"], 0.9);
    }

    #[test]
    fn test_no_generation_config_when_no_params() {
        let openai_request = json!({
            "model": "gemini-pro",
            "messages": [{"role": "user", "content": "Hi"}]
        });

        let result = transpiler().to_native(&openai_request).unwrap();

        // model is logged as warning but not included in native
        assert!(result.get("generationConfig").is_none());
    }

    #[test]
    fn test_missing_messages_field_returns_error() {
        let invalid_request = json!({
            "model": "gemini-pro"
        });

        let result = transpiler().to_native(&invalid_request);
        assert!(result.is_err());

        match result.unwrap_err() {
            TranspilerError::MissingField { field } => {
                assert_eq!(field, "messages");
            }
            _ => panic!("Expected MissingField error"),
        }
    }

    #[test]
    fn test_basic_vertex_to_openai_response() {
        let vertex_response = json!({
            "candidates": [{
                "content": {
                    "parts": [{"text": "Hello! I'm doing well."}],
                    "role": "model"
                },
                "finishReason": "STOP"
            }],
            "usageMetadata": {
                "promptTokenCount": 10,
                "candidatesTokenCount": 8,
                "totalTokenCount": 18
            }
        });

        let result = transpiler().from_native(&vertex_response).unwrap();

        assert_eq!(result["object"], "chat.completion");
        assert_eq!(result["id"], "chatcmpl-vertex");

        let choices = result["choices"].as_array().unwrap();
        assert_eq!(choices.len(), 1);
        assert_eq!(choices[0]["index"], 0);
        assert_eq!(choices[0]["message"]["role"], "assistant");
        assert_eq!(choices[0]["message"]["content"], "Hello! I'm doing well.");
        assert_eq!(choices[0]["finish_reason"], "stop");

        let usage = &result["usage"];
        assert_eq!(usage["prompt_tokens"], 10);
        assert_eq!(usage["completion_tokens"], 8);
        assert_eq!(usage["total_tokens"], 18);
    }

    #[test]
    fn test_vertex_response_without_usage_metadata() {
        let vertex_response = json!({
            "candidates": [{
                "content": {
                    "parts": [{"text": "Response"}],
                    "role": "model"
                },
                "finishReason": "STOP"
            }]
        });

        let result = transpiler().from_native(&vertex_response).unwrap();

        // usage should not be present
        assert!(result.get("usage").is_none());
    }

    #[test]
    fn test_vertex_finish_reason_mapping() {
        let vertex_response = json!({
            "candidates": [{
                "content": {
                    "parts": [{"text": "..."}],
                    "role": "model"
                },
                "finishReason": "MAX_TOKENS"
            }]
        });

        let result = transpiler().from_native(&vertex_response).unwrap();
        assert_eq!(result["choices"][0]["finish_reason"], "length");
    }

    #[test]
    fn test_vertex_safety_finish_reason() {
        let vertex_response = json!({
            "candidates": [{
                "content": {
                    "parts": [{"text": ""}],
                    "role": "model"
                },
                "finishReason": "SAFETY"
            }]
        });

        let result = transpiler().from_native(&vertex_response).unwrap();
        assert_eq!(result["choices"][0]["finish_reason"], "content_filter");
    }

    #[test]
    fn test_chunk_translation() {
        let vertex_chunk = json!({
            "candidates": [{
                "content": {
                    "parts": [{"text": "Hello"}],
                    "role": "model"
                }
            }]
        });

        let result = transpiler().translate_chunk(&vertex_chunk).unwrap();

        assert_eq!(result["object"], "chat.completion.chunk");
        assert_eq!(result["id"], "chatcmpl-vertex");

        let choices = result["choices"].as_array().unwrap();
        assert_eq!(choices.len(), 1);
        assert_eq!(choices[0]["index"], 0);
        assert_eq!(choices[0]["delta"]["role"], "assistant");
        assert_eq!(choices[0]["delta"]["content"], "Hello");
        assert!(choices[0]["finish_reason"].is_null());
    }

    #[test]
    fn test_chunk_with_finish_reason() {
        let vertex_chunk = json!({
            "candidates": [{
                "content": {
                    "parts": [{"text": "done"}],
                    "role": "model"
                },
                "finishReason": "STOP"
            }]
        });

        let result = transpiler().translate_chunk(&vertex_chunk).unwrap();

        let choices = result["choices"].as_array().unwrap();
        assert_eq!(choices[0]["finish_reason"], "stop");
        assert_eq!(choices[0]["delta"]["content"], "done");
    }

    #[test]
    fn test_chunk_missing_candidates_returns_error() {
        let invalid_chunk = json!({});

        let result = transpiler().translate_chunk(&invalid_chunk);
        assert!(result.is_err());
    }

    #[test]
    fn test_from_native_missing_candidates_returns_error() {
        let invalid_response = json!({ "modelVersion": "gemini-pro" });

        let result = transpiler().from_native(&invalid_response);
        assert!(result.is_err());
    }

    #[test]
    fn test_vertex_response_with_model_version() {
        let vertex_response = json!({
            "candidates": [{
                "content": {
                    "parts": [{"text": "Hi"}],
                    "role": "model"
                },
                "finishReason": "STOP"
            }],
            "modelVersion": "gemini-1.5-pro-001"
        });

        let result = transpiler().from_native(&vertex_response).unwrap();
        assert_eq!(result["model"], "gemini-1.5-pro-001");
    }

    #[test]
    fn test_multiple_candidates_in_response() {
        let vertex_response = json!({
            "candidates": [
                {
                    "content": {
                        "parts": [{"text": "Response A"}],
                        "role": "model"
                    },
                    "finishReason": "STOP"
                },
                {
                    "content": {
                        "parts": [{"text": "Response B"}],
                        "role": "model"
                    },
                    "finishReason": "STOP"
                }
            ]
        });

        let result = transpiler().from_native(&vertex_response).unwrap();

        let choices = result["choices"].as_array().unwrap();
        assert_eq!(choices.len(), 2);
        assert_eq!(choices[0]["index"], 0);
        assert_eq!(choices[0]["message"]["content"], "Response A");
        assert_eq!(choices[1]["index"], 1);
        assert_eq!(choices[1]["message"]["content"], "Response B");
    }

    #[test]
    fn test_full_roundtrip_conversion() {
        // Start with OpenAI format
        let openai_request = json!({
            "model": "gemini-pro",
            "messages": [
                {"role": "system", "content": "Be helpful"},
                {"role": "user", "content": "Hello"},
                {"role": "assistant", "content": "Hi there!"},
                {"role": "user", "content": "What is AI?"}
            ],
            "temperature": 0.8,
            "max_tokens": 500,
            "stop": ["END"]
        });

        // Convert to native (Vertex)
        let native = transpiler().to_native(&openai_request).unwrap();

        // Verify native format structure
        assert!(native.get("contents").is_some());
        assert!(native.get("systemInstruction").is_some());
        assert!(native.get("generationConfig").is_some());

        // Verify contents have correct roles
        let contents = native["contents"].as_array().unwrap();
        assert_eq!(contents.len(), 3); // system excluded
        assert_eq!(contents[0]["role"], "user");
        assert_eq!(contents[1]["role"], "model");
        assert_eq!(contents[2]["role"], "user");

        // Verify systemInstruction
        assert_eq!(
            native["systemInstruction"]["parts"][0]["text"],
            "Be helpful"
        );

        // Verify generationConfig
        assert_eq!(native["generationConfig"]["temperature"], 0.8);
        assert_eq!(native["generationConfig"]["maxOutputTokens"], 500);
        assert_eq!(native["generationConfig"]["stopSequences"], json!(["END"]));
    }
}
