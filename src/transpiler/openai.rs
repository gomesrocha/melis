//! OpenAI passthrough transpiler.
//!
//! No-op transpiler for requests directed at OpenAI-compatible providers.
//! Since the gateway's canonical format is OpenAI, this transpiler simply
//! passes payloads through without modification.

use serde_json::Value;

use super::{PayloadTranspiler, TranspilerError};

/// Passthrough transpiler for OpenAI-compatible providers.
///
/// Returns payloads unchanged since the gateway already uses OpenAI format
/// as its canonical representation.
pub struct OpenAiTranspiler;

impl PayloadTranspiler for OpenAiTranspiler {
    fn to_native(&self, request: &Value) -> Result<Value, TranspilerError> {
        Ok(request.clone())
    }

    fn from_native(&self, response: &Value) -> Result<Value, TranspilerError> {
        Ok(response.clone())
    }

    fn translate_chunk(&self, chunk: &Value) -> Result<Value, TranspilerError> {
        Ok(chunk.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transpiler::get_transpiler;
    use serde_json::json;

    /// Builds a realistic OpenAI chat completion request payload.
    fn realistic_openai_request() -> Value {
        json!({
            "model": "gpt-4o",
            "messages": [
                {"role": "system", "content": "You are a helpful assistant."},
                {"role": "user", "content": "Explain quantum computing in simple terms."}
            ],
            "temperature": 0.7,
            "max_tokens": 1024,
            "top_p": 0.95,
            "frequency_penalty": 0.0,
            "presence_penalty": 0.0,
            "stop": ["\n\n"],
            "stream": false
        })
    }

    /// Builds a realistic OpenAI chat completion response payload.
    fn realistic_openai_response() -> Value {
        json!({
            "id": "chatcmpl-abc123def456",
            "object": "chat.completion",
            "created": 1700000000,
            "model": "gpt-4o-2024-05-13",
            "choices": [
                {
                    "index": 0,
                    "message": {
                        "role": "assistant",
                        "content": "Quantum computing uses qubits that can be in multiple states simultaneously."
                    },
                    "finish_reason": "stop"
                }
            ],
            "usage": {
                "prompt_tokens": 25,
                "completion_tokens": 15,
                "total_tokens": 40
            }
        })
    }

    /// Builds a realistic OpenAI SSE streaming chunk payload.
    fn realistic_openai_chunk() -> Value {
        json!({
            "id": "chatcmpl-abc123def456",
            "object": "chat.completion.chunk",
            "created": 1700000000,
            "model": "gpt-4o-2024-05-13",
            "choices": [
                {
                    "index": 0,
                    "delta": {
                        "role": "assistant",
                        "content": "Quantum"
                    },
                    "finish_reason": null
                }
            ]
        })
    }

    #[test]
    fn to_native_returns_input_unchanged() {
        let transpiler = OpenAiTranspiler;
        let request = realistic_openai_request();

        let result = transpiler.to_native(&request).unwrap();

        assert_eq!(result, request);
    }

    #[test]
    fn from_native_returns_input_unchanged() {
        let transpiler = OpenAiTranspiler;
        let response = realistic_openai_response();

        let result = transpiler.from_native(&response).unwrap();

        assert_eq!(result, response);
    }

    #[test]
    fn translate_chunk_returns_input_unchanged() {
        let transpiler = OpenAiTranspiler;
        let chunk = realistic_openai_chunk();

        let result = transpiler.translate_chunk(&chunk).unwrap();

        assert_eq!(result, chunk);
    }

    #[test]
    fn to_native_preserves_all_fields_in_complex_request() {
        let transpiler = OpenAiTranspiler;
        let request = json!({
            "model": "gpt-4o-mini",
            "messages": [
                {"role": "system", "content": "You are a coding assistant."},
                {"role": "user", "content": "Write a fibonacci function in Rust."},
                {"role": "assistant", "content": "Here's a Fibonacci function:\n```rust\nfn fib(n: u64) -> u64 { ... }\n```"},
                {"role": "user", "content": "Now make it iterative."}
            ],
            "temperature": 0.3,
            "max_tokens": 2048,
            "top_p": 1.0,
            "frequency_penalty": 0.5,
            "presence_penalty": 0.2,
            "stop": ["```", "\n\n\n"],
            "stream": true,
            "n": 1,
            "logprobs": true,
            "top_logprobs": 5
        });

        let result = transpiler.to_native(&request).unwrap();

        assert_eq!(result, request);
        // Verify specific fields preserved exactly
        assert_eq!(result["model"], "gpt-4o-mini");
        assert_eq!(result["messages"].as_array().unwrap().len(), 4);
        assert_eq!(result["temperature"], 0.3);
        assert_eq!(result["max_tokens"], 2048);
        assert_eq!(result["stream"], true);
        assert_eq!(result["stop"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn from_native_preserves_usage_and_choices() {
        let transpiler = OpenAiTranspiler;
        let response = json!({
            "id": "chatcmpl-xyz789",
            "object": "chat.completion",
            "created": 1700001234,
            "model": "gpt-4o",
            "choices": [
                {
                    "index": 0,
                    "message": {
                        "role": "assistant",
                        "content": "Here is a long response with multiple paragraphs.\n\nParagraph two."
                    },
                    "finish_reason": "stop",
                    "logprobs": null
                },
                {
                    "index": 1,
                    "message": {
                        "role": "assistant",
                        "content": "Alternative response."
                    },
                    "finish_reason": "stop",
                    "logprobs": null
                }
            ],
            "usage": {
                "prompt_tokens": 100,
                "completion_tokens": 250,
                "total_tokens": 350
            },
            "system_fingerprint": "fp_abc123"
        });

        let result = transpiler.from_native(&response).unwrap();

        assert_eq!(result, response);
        assert_eq!(result["choices"].as_array().unwrap().len(), 2);
        assert_eq!(result["usage"]["total_tokens"], 350);
        assert_eq!(result["system_fingerprint"], "fp_abc123");
    }

    #[test]
    fn translate_chunk_preserves_delta_and_finish_reason() {
        let transpiler = OpenAiTranspiler;

        // Final chunk with finish_reason
        let chunk = json!({
            "id": "chatcmpl-abc123",
            "object": "chat.completion.chunk",
            "created": 1700000000,
            "model": "gpt-4o",
            "choices": [
                {
                    "index": 0,
                    "delta": {},
                    "finish_reason": "stop"
                }
            ]
        });

        let result = transpiler.translate_chunk(&chunk).unwrap();

        assert_eq!(result, chunk);
        assert_eq!(result["choices"][0]["finish_reason"], "stop");
        assert_eq!(result["choices"][0]["delta"], json!({}));
    }

    #[test]
    fn get_transpiler_openai_uses_passthrough() {
        let transpiler = get_transpiler("openai");
        let request = realistic_openai_request();

        let native = transpiler.to_native(&request).unwrap();
        assert_eq!(native, request);

        let response = realistic_openai_response();
        let converted = transpiler.from_native(&response).unwrap();
        assert_eq!(converted, response);

        let chunk = realistic_openai_chunk();
        let translated = transpiler.translate_chunk(&chunk).unwrap();
        assert_eq!(translated, chunk);
    }

    #[test]
    fn get_transpiler_ollama_uses_passthrough() {
        let transpiler = get_transpiler("ollama");
        let request = realistic_openai_request();

        let native = transpiler.to_native(&request).unwrap();
        assert_eq!(native, request);

        let response = realistic_openai_response();
        let converted = transpiler.from_native(&response).unwrap();
        assert_eq!(converted, response);

        let chunk = realistic_openai_chunk();
        let translated = transpiler.translate_chunk(&chunk).unwrap();
        assert_eq!(translated, chunk);
    }

    #[test]
    fn get_transpiler_oci_genai_uses_passthrough() {
        let transpiler = get_transpiler("oci_genai");
        let request = realistic_openai_request();

        let native = transpiler.to_native(&request).unwrap();
        assert_eq!(native, request);

        let response = realistic_openai_response();
        let converted = transpiler.from_native(&response).unwrap();
        assert_eq!(converted, response);

        let chunk = realistic_openai_chunk();
        let translated = transpiler.translate_chunk(&chunk).unwrap();
        assert_eq!(translated, chunk);
    }
}
