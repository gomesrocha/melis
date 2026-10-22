//! Amazon Bedrock through the OpenAI-compatible Runtime API.
//!
//! `provider_type = bedrock` is first-class. The chat wire format reuses the
//! OpenAI-compatible parser, and the only authentication mode in this cut is a
//! bearer token read from the environment at request time. A later signer
//! (for example SigV4) can implement [`ProviderCredential`] without changing
//! the chat request or the no-fallback rule.

use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION};
use serde_json::{json, Map, Value};

use crate::config::{ProviderAuthConfig, ProviderConfig};
use crate::error::GatewayError;

pub const PROVIDER_TYPE: &str = "bedrock";
pub const DEFAULT_AUTH_ENV: &str = "AWS_BEARER_TOKEN_BEDROCK";
pub const DEFAULT_REGION: &str = "us-east-1";
const REDACTED: &str = "[REDACTED]";

/// Authentication modes this provider can resolve.
///
/// Only bearer-from-environment is implemented. The enum is the extension
/// point for a future AWS signer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderCredential {
    BearerEnv { env_name: String },
}

impl ProviderCredential {
    pub fn from_provider(provider: &ProviderConfig) -> Self {
        let env_name = provider
            .auth
            .as_ref()
            .and_then(|auth| auth.env.clone())
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_AUTH_ENV.to_string());
        ProviderCredential::BearerEnv { env_name }
    }

    /// Reads the bearer token. The returned string is the secret; callers must
    /// not log it. Errors name only the environment variable.
    pub fn resolve(&self) -> Result<String, GatewayError> {
        match self {
            ProviderCredential::BearerEnv { env_name } => match std::env::var(env_name) {
                Ok(value) if !value.trim().is_empty() => Ok(value),
                _ => Err(GatewayError::Unauthorized(format!(
                    "Bedrock bearer token is not available from {env_name}"
                ))),
            },
        }
    }
}

pub fn validate_provider(provider: &ProviderConfig, index: usize) -> Vec<String> {
    let mut errors = Vec::new();
    let region = provider.region.as_deref().unwrap_or("").trim();
    if region.is_empty() {
        errors.push(format!("providers[{index}].region is required for bedrock"));
    }
    let host = format!("bedrock-runtime.{region}.amazonaws.com");
    if region.is_empty()
        || !provider.base_url.contains(&host)
        || !provider
            .base_url
            .trim_end_matches('/')
            .ends_with("/openai/v1")
    {
        errors.push(format!(
            "providers[{index}].base_url must be the {host} OpenAI-compatible /openai/v1 endpoint"
        ));
    }
    match provider.auth.as_ref().map(|auth| auth.mode.as_str()) {
        Some("bearer_env") => {}
        _ => errors.push(format!(
            "providers[{index}].auth.mode must be bearer_env for bedrock"
        )),
    }
    if !provider.api_key.trim().is_empty() {
        errors.push(format!(
            "providers[{index}].api_key must be empty for bedrock; the bearer token is read from the environment at runtime"
        ));
    }
    errors
}

pub fn provider_for_exact_model<'a>(
    providers: &'a [ProviderConfig],
    model: &str,
) -> Option<&'a ProviderConfig> {
    providers.iter().find(|provider| {
        provider.provider_type == PROVIDER_TYPE
            && provider.models.iter().any(|candidate| candidate == model)
    })
}

pub fn retries_another_provider(provider_type: &str) -> bool {
    provider_type != PROVIDER_TYPE
}

pub fn chat_completions_url(base_url: &str) -> String {
    format!("{}/chat/completions", base_url.trim_end_matches('/'))
}

pub fn prepare_chat_body(payload: &Value, model: &str) -> Value {
    let mut body = payload.clone();
    if let Some(obj) = body.as_object_mut() {
        obj.insert("model".to_string(), Value::String(model.to_string()));
    }
    body
}

/// Inserts the bearer token into `Authorization` and nowhere else.
pub fn insert_bearer_header(headers: &mut HeaderMap, token: &str) -> Result<(), GatewayError> {
    let value = HeaderValue::from_str(&format!("Bearer {token}")).map_err(|_| {
        GatewayError::Unauthorized(
            "Bedrock bearer token is not a valid Authorization header value".to_string(),
        )
    })?;
    headers.insert(AUTHORIZATION, value);
    Ok(())
}

pub fn redact(text: &str, secret: &str) -> String {
    if secret.is_empty() {
        return text.to_string();
    }
    text.replace(secret, REDACTED)
        .replace(&format!("Bearer {secret}"), &format!("Bearer {REDACTED}"))
}

pub fn normalize_chat_response(response: &Value) -> Value {
    let mut out = response.clone();
    let Some(usage) = out.get_mut("usage").and_then(Value::as_object_mut) else {
        return out;
    };
    let prompt = first_u64(usage, &["prompt_tokens", "input_tokens", "inputTokens"]);
    let completion = first_u64(
        usage,
        &["completion_tokens", "output_tokens", "outputTokens"],
    );
    if let Some(prompt) = prompt {
        usage.insert("prompt_tokens".to_string(), json!(prompt));
    }
    if let Some(completion) = completion {
        usage.insert("completion_tokens".to_string(), json!(completion));
    }
    if usage.get("total_tokens").and_then(Value::as_u64).is_none() {
        if let (Some(prompt), Some(completion)) = (prompt, completion) {
            usage.insert("total_tokens".to_string(), json!(prompt + completion));
        }
    }
    out
}

pub fn http_status_error(status: u16, body: &str, secret: &str, model: &str) -> GatewayError {
    let safe = redact(body, secret);
    match status {
        401 => GatewayError::Unauthorized(format!("Provider auth error: {safe}")),
        403 => GatewayError::Forbidden(format!("Provider auth error: {safe}")),
        404 => GatewayError::BadRequest(format!("Model '{model}' not found at provider")),
        429 => GatewayError::RateLimited {
            retry_after_secs: 30,
        },
        500..=599 => GatewayError::ServiceUnavailable(format!("Provider error {status}: {safe}")),
        _ => GatewayError::ProviderError(format!("HTTP {status}: {safe}")),
    }
}

pub fn timeout_error() -> GatewayError {
    GatewayError::ServiceUnavailable("Provider timeout".to_string())
}

fn first_u64(usage: &Map<String, Value>, names: &[&str]) -> Option<u64> {
    names
        .iter()
        .find_map(|name| usage.get(*name).and_then(Value::as_u64))
}

pub fn bearer_auth(env_name: &str) -> ProviderAuthConfig {
    ProviderAuthConfig {
        mode: "bearer_env".to_string(),
        env: Some(env_name.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::response::IntoResponse;
    use reqwest::header::CONTENT_TYPE;
    use serde_json::json;

    const SECRET: &str = "BESMART_BEDROCK_TEST_SECRET_VALUE";

    fn bedrock_provider(models: Vec<&str>) -> ProviderConfig {
        ProviderConfig {
            id: "bedrock".to_string(),
            provider_type: PROVIDER_TYPE.to_string(),
            base_url: "https://bedrock-runtime.us-east-1.amazonaws.com/openai/v1".to_string(),
            api_key: String::new(),
            weight: 1,
            timeout_secs: 30,
            models: models.into_iter().map(str::to_string).collect(),
            region: Some(DEFAULT_REGION.to_string()),
            auth: Some(bearer_auth(DEFAULT_AUTH_ENV)),
        }
    }

    fn openrouter_provider() -> ProviderConfig {
        ProviderConfig {
            id: "openrouter".to_string(),
            provider_type: "openai".to_string(),
            base_url: "https://openrouter.ai/api/v1".to_string(),
            api_key: "sk-test".to_string(),
            weight: 1,
            timeout_secs: 30,
            models: vec!["qwen/qwen3-coder".to_string()],
            region: None,
            auth: None,
        }
    }

    #[test]
    fn provider_type_and_exact_model_route_do_not_select_openrouter() {
        let providers = vec![
            openrouter_provider(),
            bedrock_provider(vec![
                "qwen.qwen3-next-80b-a3b",
                "qwen.qwen3-coder-next",
                "moonshotai.kimi-k2.5",
            ]),
        ];
        let selected = provider_for_exact_model(&providers, "qwen.qwen3-coder-next").unwrap();
        assert_eq!(selected.provider_type, PROVIDER_TYPE);
        assert_eq!(selected.id, "bedrock");
        assert_eq!(selected.region.as_deref(), Some(DEFAULT_REGION));
        assert!(provider_for_exact_model(&providers, "qwen/qwen3-coder").is_none());
        assert!(provider_for_exact_model(&providers, "moonshotai.kimi-k2.5")
            .unwrap()
            .models
            .iter()
            .any(|model| model == "moonshotai.kimi-k2.5"));
    }

    #[test]
    fn bearer_token_is_only_in_the_authorization_header() {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        insert_bearer_header(&mut headers, SECRET).unwrap();
        let body = prepare_chat_body(
            &json!({"model": "other", "messages": [{"role": "user", "content": "ping"}]}),
            "qwen.qwen3-coder-next",
        );
        let body_text = serde_json::to_string(&body).unwrap();
        assert_eq!(body["model"], "qwen.qwen3-coder-next");
        assert!(!body_text.contains(SECRET));
        assert_eq!(
            headers.get(AUTHORIZATION).unwrap().to_str().unwrap(),
            format!("Bearer {SECRET}")
        );
        let leaks = headers
            .iter()
            .filter(|(name, value)| {
                value.to_str().unwrap_or("").contains(SECRET) && *name != AUTHORIZATION
            })
            .count();
        assert_eq!(leaks, 0);
        assert_eq!(
            chat_completions_url("https://bedrock-runtime.us-east-1.amazonaws.com/openai/v1"),
            "https://bedrock-runtime.us-east-1.amazonaws.com/openai/v1/chat/completions"
        );
    }

    #[test]
    fn secret_never_appears_in_errors_or_debug() {
        let credential = ProviderCredential::BearerEnv {
            env_name: DEFAULT_AUTH_ENV.to_string(),
        };
        let debug = format!("{:?}", credential);
        assert!(!debug.contains(SECRET));
        let missing = credential.resolve().unwrap_err();
        assert!(!missing.to_string().contains(SECRET));
        let body = format!("denied token={SECRET}");
        let error = http_status_error(401, &body, SECRET, "qwen.qwen3-coder-next");
        let rendered = error.to_string();
        assert!(!rendered.contains(SECRET));
        assert!(rendered.contains(REDACTED));
    }

    #[test]
    fn success_response_normalizes_usage_and_keeps_the_model() {
        let raw = json!({
            "id": "resp-1",
            "model": "qwen.qwen3-coder-next",
            "choices": [{"message": {"role": "assistant", "content": "ok"}}],
            "usage": {"input_tokens": 4, "output_tokens": 6}
        });
        let normalized = normalize_chat_response(&raw);
        assert_eq!(normalized["model"], "qwen.qwen3-coder-next");
        assert_eq!(normalized["choices"][0]["message"]["content"], "ok");
        assert_eq!(normalized["usage"]["prompt_tokens"], 4);
        assert_eq!(normalized["usage"]["completion_tokens"], 6);
        assert_eq!(normalized["usage"]["total_tokens"], 10);
    }

    #[test]
    fn auth_throttling_server_and_timeout_errors_are_normalized() {
        let denied = http_status_error(401, "no", SECRET, "qwen.qwen3-coder-next");
        let forbidden = http_status_error(403, "forbidden", SECRET, "qwen.qwen3-coder-next");
        let throttled = http_status_error(429, "slow down", SECRET, "qwen.qwen3-coder-next");
        let server = http_status_error(503, "unavailable", SECRET, "qwen.qwen3-coder-next");
        let timed_out = timeout_error();

        assert_eq!(denied.into_response().status(), 401);
        assert_eq!(forbidden.into_response().status(), 403);
        let throttled_response = throttled.into_response();
        assert_eq!(throttled_response.status(), 429);
        assert_eq!(server.into_response().status(), 503);
        assert_eq!(timed_out.into_response().status(), 503);
        assert!(!retries_another_provider(PROVIDER_TYPE));
        assert!(retries_another_provider("openai"));
    }

    #[test]
    fn bedrock_config_rejects_an_inline_secret_and_a_missing_region() {
        let mut provider = bedrock_provider(vec!["qwen.qwen3-coder-next"]);
        assert!(validate_provider(&provider, 0).is_empty());
        provider.api_key = SECRET.to_string();
        let errors = validate_provider(&provider, 0);
        assert!(errors
            .iter()
            .any(|error| error.contains("api_key must be empty")));
        assert!(errors.iter().all(|error| !error.contains(SECRET)));
        provider.api_key.clear();
        provider.region = None;
        assert!(validate_provider(&provider, 0)
            .iter()
            .any(|error| error.contains("region")));
    }
}
