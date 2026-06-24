//! Payload Transpiler module.
//!
//! Handles bidirectional translation between OpenAI format and
//! native provider formats (Anthropic, Vertex AI, etc.).
//!
//! Uses `serde_json::Value` for flexibility in the initial implementation.
//! Zero-copy optimizations with lifetimed structs will be introduced later.

pub mod anthropic;
pub mod openai;
pub mod vertex;

use serde_json::Value;
use thiserror::Error;

/// Errors that can occur during payload transpilation.
#[derive(Debug, Error)]
pub enum TranspilerError {
    /// A required field is missing from the input payload.
    #[error("Missing required field: {field}")]
    MissingField { field: String },

    /// A field has an invalid type or value.
    #[error("Invalid field value for '{field}': {reason}")]
    InvalidFieldValue { field: String, reason: String },

    /// A field mapping between formats failed.
    #[error("Field mapping error: {source_field} -> {target_field}: {reason}")]
    FieldMappingError {
        source_field: String,
        target_field: String,
        reason: String,
    },

    /// The provider does not support a given field or feature.
    #[error("Unsupported field '{field}' for provider")]
    UnsupportedField { field: String },

    /// JSON serialization/deserialization error.
    #[error("JSON error: {0}")]
    JsonError(#[from] serde_json::Error),
}

/// Trait for bidirectional payload translation between OpenAI format
/// and native provider formats.
///
/// Implementors handle the specifics of each provider's API format.
pub trait PayloadTranspiler: Send + Sync {
    /// Converts an OpenAI-format request payload into the provider's native format.
    fn to_native(&self, request: &Value) -> Result<Value, TranspilerError>;

    /// Converts a provider's native response into OpenAI format.
    fn from_native(&self, response: &Value) -> Result<Value, TranspilerError>;

    /// Converts a native SSE streaming chunk into OpenAI chunk format.
    fn translate_chunk(&self, chunk: &Value) -> Result<Value, TranspilerError>;
}

/// Factory function that returns the appropriate transpiler for a given provider type.
///
/// # Arguments
/// * `provider_type` - The provider identifier string (e.g., "openai", "anthropic", "google_vertex_ai")
///
/// # Returns
/// A boxed transpiler implementation. Defaults to the OpenAI passthrough transpiler
/// for unknown provider types (since many are OpenAI-compatible).
pub fn get_transpiler(provider_type: &str) -> Box<dyn PayloadTranspiler> {
    match provider_type {
        "anthropic" => Box::new(anthropic::AnthropicTranspiler),
        "google_vertex_ai" => Box::new(vertex::VertexTranspiler),
        // OpenAI, Ollama, OCI GenAI, and unknown providers use passthrough
        // since they are typically OpenAI-compatible
        _ => Box::new(openai::OpenAiTranspiler),
    }
}

#[cfg(test)]
mod proptest_tests {
    use super::*;
    use proptest::prelude::*;
    use serde_json::json;

    /// Strategy to generate a valid model name.
    fn model_strategy() -> impl Strategy<Value = String> {
        prop_oneof![
            Just("gpt-4o".to_string()),
            Just("claude-sonnet-4-20250514".to_string()),
            Just("gemini-pro".to_string()),
            Just("gpt-4o-mini".to_string()),
            "[a-z][a-z0-9-]{2,20}".prop_map(|s| s),
        ]
    }

    /// Strategy to generate a valid message role.
    fn role_strategy() -> impl Strategy<Value = String> {
        prop_oneof![
            3 => Just("user".to_string()),
            3 => Just("assistant".to_string()),
            1 => Just("system".to_string()),
        ]
    }

    /// Strategy to generate message content.
    fn content_strategy() -> impl Strategy<Value = String> {
        "[a-zA-Z0-9 .,!?]{5,100}"
    }

    /// Strategy to generate a list of messages (at least 1 user message).
    fn messages_strategy() -> impl Strategy<Value = Vec<Value>> {
        let msg_strategy = (role_strategy(), content_strategy()).prop_map(|(role, content)| {
            json!({ "role": role, "content": content })
        });

        proptest::collection::vec(msg_strategy, 1..6).prop_map(|mut msgs| {
            // Ensure at least one user message exists
            let has_user = msgs.iter().any(|m| m["role"] == "user");
            if !has_user {
                msgs.push(json!({ "role": "user", "content": "Hello there" }));
            }
            msgs
        })
    }

    /// Strategy to generate a valid OpenAI payload.
    fn openai_payload_strategy() -> impl Strategy<Value = Value> {
        (
            model_strategy(),
            messages_strategy(),
            proptest::option::of(0.0f64..=2.0f64),
            proptest::option::of(1u64..=4096u64),
            proptest::option::of(proptest::collection::vec("[a-z]{1,5}", 1..3)),
        )
            .prop_map(|(model, messages, temperature, max_tokens, stop)| {
                let mut payload = json!({
                    "model": model,
                    "messages": messages,
                });
                if let Some(t) = temperature {
                    payload["temperature"] = json!(t);
                }
                if let Some(mt) = max_tokens {
                    payload["max_tokens"] = json!(mt);
                }
                if let Some(s) = stop {
                    payload["stop"] = json!(s);
                }
                payload
            })
    }

    /// **Validates: Requirements 2.5**
    ///
    /// Property 2: Round-Trip do Payload Transpiler
    /// For any valid OpenAI payload, converting to native and back preserves key fields.
    mod property_transpiler_roundtrip {
        use super::*;

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(100))]

            #[test]
            fn openai_passthrough_roundtrip_preserves_all_fields(
                payload in openai_payload_strategy(),
            ) {
                let transpiler = get_transpiler("openai");

                let native = transpiler.to_native(&payload).unwrap();
                let back = transpiler.from_native(&native).unwrap();

                // OpenAI passthrough should be exact identity
                prop_assert_eq!(&payload, &back);
            }

            #[test]
            fn anthropic_roundtrip_preserves_model(
                payload in openai_payload_strategy(),
            ) {
                let transpiler = get_transpiler("anthropic");

                let native = transpiler.to_native(&payload).unwrap();

                // Simulate a realistic Anthropic response that from_native expects
                let content_text = "Response from model";
                let anthropic_response = json!({
                    "id": "msg_test123",
                    "type": "message",
                    "role": "assistant",
                    "content": [
                        {"type": "text", "text": content_text}
                    ],
                    "model": payload["model"],
                    "stop_reason": "end_turn",
                    "usage": {
                        "input_tokens": 10,
                        "output_tokens": 5
                    }
                });

                let openai_response = transpiler.from_native(&anthropic_response).unwrap();

                // Model should be preserved in the response
                prop_assert_eq!(
                    openai_response["model"].as_str().unwrap(),
                    payload["model"].as_str().unwrap(),
                    "Model not preserved in Anthropic round-trip"
                );

                // Response should have correct structure
                prop_assert_eq!(openai_response["object"].as_str().unwrap(), "chat.completion");
                prop_assert!(openai_response["choices"].is_array());
                prop_assert_eq!(
                    openai_response["choices"][0]["message"]["content"].as_str().unwrap(),
                    content_text
                );
            }

            #[test]
            fn anthropic_to_native_preserves_messages_content(
                payload in openai_payload_strategy(),
            ) {
                let transpiler = get_transpiler("anthropic");

                let native = transpiler.to_native(&payload).unwrap();

                // Verify model is preserved
                prop_assert_eq!(
                    native["model"].as_str().unwrap(),
                    payload["model"].as_str().unwrap(),
                    "Model not preserved in to_native"
                );

                // Verify non-system messages content is preserved
                let original_messages = payload["messages"].as_array().unwrap();
                let native_messages = native["messages"].as_array().unwrap();

                let non_system_originals: Vec<&Value> = original_messages
                    .iter()
                    .filter(|m| m["role"].as_str().unwrap() != "system")
                    .collect();

                prop_assert_eq!(
                    non_system_originals.len(),
                    native_messages.len(),
                    "Non-system message count differs"
                );

                for (orig, native_msg) in non_system_originals.iter().zip(native_messages.iter()) {
                    prop_assert_eq!(
                        orig["content"].as_str().unwrap(),
                        native_msg["content"].as_str().unwrap(),
                        "Message content not preserved"
                    );
                }
            }

            #[test]
            fn vertex_to_native_preserves_messages_content(
                payload in openai_payload_strategy(),
            ) {
                let transpiler = get_transpiler("google_vertex_ai");

                let native = transpiler.to_native(&payload).unwrap();

                // Verify non-system messages content is preserved in contents
                let original_messages = payload["messages"].as_array().unwrap();
                let contents = native["contents"].as_array().unwrap();

                let non_system_originals: Vec<&Value> = original_messages
                    .iter()
                    .filter(|m| m["role"].as_str().unwrap() != "system")
                    .collect();

                prop_assert_eq!(
                    non_system_originals.len(),
                    contents.len(),
                    "Non-system message count differs in Vertex contents"
                );

                for (orig, content_item) in non_system_originals.iter().zip(contents.iter()) {
                    let original_content = orig["content"].as_str().unwrap();
                    let vertex_text = content_item["parts"][0]["text"].as_str().unwrap();
                    prop_assert_eq!(
                        original_content,
                        vertex_text,
                        "Message content not preserved in Vertex format"
                    );
                }
            }
        }
    }

    /// **Validates: Requirements 2.6**
    ///
    /// Property 4: Campos Não-Suportados São Omitidos Graciosamente
    /// For any valid OpenAI payload with extra unsupported fields,
    /// conversion to native format omits those fields without error.
    mod property_unsupported_fields_omitted {
        use super::*;

        /// Strategy to generate extra unsupported fields as key-value pairs.
        fn extra_fields_strategy() -> impl Strategy<Value = Vec<(String, Value)>> {
            let field_names = prop_oneof![
                Just("frequency_penalty".to_string()),
                Just("presence_penalty".to_string()),
                Just("logit_bias".to_string()),
                Just("n".to_string()),
                Just("seed".to_string()),
                Just("user".to_string()),
                Just("tools".to_string()),
            ];

            let field_values = prop_oneof![
                (0.0f64..=2.0f64).prop_map(|v| json!(v)),
                (1u64..=10u64).prop_map(|v| json!(v)),
                Just(json!({"100": 5, "200": -3})),
                Just(json!("test-user-id")),
                Just(json!([{"type": "function", "function": {"name": "test"}}])),
            ];

            proptest::collection::vec((field_names, field_values), 1..=7)
        }

        /// Strategy to generate a valid OpenAI payload with extra fields.
        fn payload_with_extras_strategy() -> impl Strategy<Value = Value> {
            (openai_payload_strategy(), extra_fields_strategy()).prop_map(
                |(mut payload, extras)| {
                    let obj = payload.as_object_mut().unwrap();
                    for (key, value) in extras {
                        obj.insert(key, value);
                    }
                    payload
                },
            )
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(100))]

            #[test]
            fn anthropic_omits_unsupported_fields(
                payload in payload_with_extras_strategy(),
            ) {
                let transpiler = get_transpiler("anthropic");

                let result = transpiler.to_native(&payload);
                // Should not return an error
                prop_assert!(result.is_ok(), "Anthropic transpiler returned error for payload with extra fields: {:?}", result.err());

                let native = result.unwrap();
                let native_obj = native.as_object().unwrap();

                // These unsupported fields must NOT appear in native output
                let unsupported = ["frequency_penalty", "presence_penalty", "logit_bias", "n", "seed", "user", "tools"];
                for field in &unsupported {
                    prop_assert!(
                        !native_obj.contains_key(*field),
                        "Anthropic native output contains unsupported field '{}'",
                        field
                    );
                }
            }

            #[test]
            fn vertex_omits_unsupported_fields(
                payload in payload_with_extras_strategy(),
            ) {
                let transpiler = get_transpiler("google_vertex_ai");

                let result = transpiler.to_native(&payload);
                // Should not return an error
                prop_assert!(result.is_ok(), "Vertex transpiler returned error for payload with extra fields: {:?}", result.err());

                let native = result.unwrap();
                let native_obj = native.as_object().unwrap();

                // These unsupported fields must NOT appear in native output
                let unsupported = ["frequency_penalty", "presence_penalty", "logit_bias", "n", "seed", "user", "tools"];
                for field in &unsupported {
                    prop_assert!(
                        !native_obj.contains_key(*field),
                        "Vertex native output contains unsupported field '{}'",
                        field
                    );
                }

                // Also check generationConfig does not have them
                if let Some(gen_config) = native_obj.get("generationConfig") {
                    let gen_obj = gen_config.as_object().unwrap();
                    for field in &unsupported {
                        prop_assert!(
                            !gen_obj.contains_key(*field),
                            "Vertex generationConfig contains unsupported field '{}'",
                            field
                        );
                    }
                }
            }
        }
    }

    /// **Validates: Requirements 2.1, 2.2**
    ///
    /// Property 3: Validade Estrutural da Saída Nativa do Transpiler
    /// For any valid OpenAI payload, the native output for Anthropic and Vertex
    /// has the correct structural shape.
    mod property_transpiler_structural_validity {
        use super::*;

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(100))]

            #[test]
            fn anthropic_native_has_valid_structure(
                payload in openai_payload_strategy(),
            ) {
                let transpiler = get_transpiler("anthropic");

                let native = transpiler.to_native(&payload).unwrap();

                // Must have `messages` array
                prop_assert!(
                    native.get("messages").is_some() && native["messages"].is_array(),
                    "Anthropic native missing 'messages' array"
                );

                // Must have `model` field
                prop_assert!(
                    native.get("model").is_some() && native["model"].is_string(),
                    "Anthropic native missing 'model' string"
                );

                // Must have `max_tokens` field (required by Anthropic)
                prop_assert!(
                    native.get("max_tokens").is_some(),
                    "Anthropic native missing 'max_tokens' field"
                );
                prop_assert!(
                    native["max_tokens"].is_number(),
                    "Anthropic 'max_tokens' is not a number"
                );

                // If there are system messages in the original, system field should exist
                let has_system = payload["messages"].as_array().unwrap()
                    .iter()
                    .any(|m| m["role"].as_str() == Some("system"));
                if has_system {
                    prop_assert!(
                        native.get("system").is_some(),
                        "Anthropic native missing 'system' field when system messages present"
                    );
                }

                // Messages should only have "user" or "assistant" roles
                let messages = native["messages"].as_array().unwrap();
                for msg in messages {
                    let role = msg["role"].as_str().unwrap_or("");
                    prop_assert!(
                        role == "user" || role == "assistant",
                        "Anthropic native message has invalid role: '{}'",
                        role
                    );
                }
            }

            #[test]
            fn vertex_native_has_valid_structure(
                payload in openai_payload_strategy(),
            ) {
                let transpiler = get_transpiler("google_vertex_ai");

                let native = transpiler.to_native(&payload).unwrap();

                // Must have `contents` array
                prop_assert!(
                    native.get("contents").is_some() && native["contents"].is_array(),
                    "Vertex native missing 'contents' array"
                );

                // Each content item must have `parts` array
                let contents = native["contents"].as_array().unwrap();
                for (i, content_item) in contents.iter().enumerate() {
                    prop_assert!(
                        content_item.get("parts").is_some() && content_item["parts"].is_array(),
                        "Vertex content[{}] missing 'parts' array", i
                    );

                    // Each part should have a "text" field
                    let parts = content_item["parts"].as_array().unwrap();
                    prop_assert!(
                        !parts.is_empty(),
                        "Vertex content[{}] has empty 'parts' array", i
                    );
                    prop_assert!(
                        parts[0].get("text").is_some(),
                        "Vertex content[{}].parts[0] missing 'text' field", i
                    );

                    // Role must be "user" or "model"
                    let role = content_item.get("role")
                        .and_then(|r| r.as_str())
                        .unwrap_or("");
                    prop_assert!(
                        role == "user" || role == "model",
                        "Vertex content[{}] has invalid role: '{}'", i, role
                    );
                }

                // If there are system messages, systemInstruction should be present
                let has_system = payload["messages"].as_array().unwrap()
                    .iter()
                    .any(|m| m["role"].as_str() == Some("system"));
                if has_system {
                    prop_assert!(
                        native.get("systemInstruction").is_some(),
                        "Vertex native missing 'systemInstruction' when system messages present"
                    );
                    // systemInstruction must have parts
                    let sys_instr = native.get("systemInstruction").unwrap();
                    prop_assert!(
                        sys_instr.get("parts").is_some() && sys_instr["parts"].is_array(),
                        "Vertex 'systemInstruction' missing 'parts' array"
                    );
                }

                // If temperature or max_tokens were provided, generationConfig should exist
                let has_gen_params = payload.get("temperature").is_some()
                    || payload.get("max_tokens").is_some()
                    || payload.get("stop").is_some();
                if has_gen_params {
                    prop_assert!(
                        native.get("generationConfig").is_some(),
                        "Vertex native missing 'generationConfig' when generation params present"
                    );
                }
            }
        }
    }
}
