//! Core data models for the OpenAI-compatible API.
//!
//! Uses `Cow<'a, str>` extensively for zero-copy deserialization,
//! minimizing heap allocations in the hot path.

use serde::{Deserialize, Serialize};
use std::borrow::Cow;

/// OpenAI Chat Completions request payload with zero-copy deserialization.
#[derive(Debug, Deserialize)]
pub struct OpenAiRequest<'a> {
    #[serde(borrow)]
    pub model: Cow<'a, str>,
    #[serde(borrow)]
    pub messages: Vec<Message<'a>>,
    #[serde(default)]
    pub temperature: Option<f64>,
    #[serde(default)]
    pub max_tokens: Option<u64>,
    #[serde(default, borrow)]
    pub stop: Option<Vec<Cow<'a, str>>>,
    #[serde(default)]
    pub stream: Option<bool>,
}

/// A single message in the conversation history.
///
/// Needs both Serialize and Deserialize since it appears in both
/// requests (incoming) and responses (outgoing).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message<'a> {
    #[serde(borrow)]
    pub role: Cow<'a, str>,
    #[serde(borrow)]
    pub content: Cow<'a, str>,
}

/// Non-streaming OpenAI Chat Completions response.
#[derive(Debug, Serialize)]
pub struct OpenAiResponse<'a> {
    pub id: Cow<'a, str>,
    pub object: &'static str,
    pub created: u64,
    pub model: Cow<'a, str>,
    pub choices: Vec<Choice<'a>>,
    pub usage: Usage,
}

/// A single choice in a non-streaming response.
#[derive(Debug, Serialize)]
pub struct Choice<'a> {
    pub index: u32,
    pub message: Message<'a>,
    pub finish_reason: Option<Cow<'a, str>>,
}

/// Token usage statistics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Usage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
}

/// Streaming SSE chunk in OpenAI format.
#[derive(Debug, Serialize)]
pub struct OpenAiChunk<'a> {
    pub id: Cow<'a, str>,
    pub object: &'static str,
    pub created: u64,
    pub model: Cow<'a, str>,
    pub choices: Vec<ChunkChoice<'a>>,
}

/// A single choice within a streaming chunk.
#[derive(Debug, Serialize)]
pub struct ChunkChoice<'a> {
    pub index: u32,
    pub delta: Delta<'a>,
    pub finish_reason: Option<Cow<'a, str>>,
}

/// Delta content within a streaming chunk choice.
#[derive(Debug, Serialize)]
pub struct Delta<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<Cow<'a, str>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<Cow<'a, str>>,
}
