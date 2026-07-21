//! Provider-agnostic structured-JSON chat. One trait, two transports:
//!
//!   - Anthropic  → forced single tool call (`tool_choice` pins the tool).
//!   - OpenAI-compatible (OpenAI / Groq / LM Studio / Ollama) → `chat/completions`
//!     with a per-provider `response_format` (see [`ResponseFormat`]) plus the
//!     JSON Schema injected into the system prompt as a contract. Most providers
//!     take `json_object`; LM Studio requires the `json_schema` envelope.
//!     Callers apply their own `normalize()` + `validate()` to the result.
//!
//! Callers (analysis, polish, chapters) build a system prompt, a user message,
//! a tool name, and a JSON Schema, then deserialize the returned `Value`.

use crate::error::{Result, SummarizeError};
use crate::state_provider::{ChatProvider, ResponseFormat};
use serde_json::{json, Value};

/// A single structured-JSON completion. Returns the JSON object the schema
/// describes (already unwrapped from any provider envelope), ready for
/// `serde_json::from_value`.
pub trait ChatCompleter: Send + Sync {
    /// `tool_name` is used as the Anthropic tool name and as a hint in the
    /// OpenAI-compat system contract. `schema` is a plain JSON Schema object
    /// describing the expected output.
    fn complete_json(
        &self,
        system: &str,
        user: &str,
        tool_name: &str,
        schema: &Value,
    ) -> Result<Value>;
}

const ANTHROPIC_VERSION: &str = "2023-06-01";
const MAX_TOKENS: u32 = 8192;
/// Shared 429 backoff cap. Retry-After is honoured when present.
const MAX_ATTEMPTS: u32 = 4;

/// The output-token-limit parameter name for an OpenAI-shaped `chat/completions`
/// body. OpenAI's own API rejects the legacy `max_tokens` on newer models and
/// requires `max_completion_tokens`; OpenAI-compatible servers (Groq, LM Studio,
/// Ollama) still expect `max_tokens`.
pub fn token_limit_key(base_url: &str) -> &'static str {
    if base_url.contains("openai.com") {
        "max_completion_tokens"
    } else {
        "max_tokens"
    }
}

fn http_client(timeout_secs: u64) -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(timeout_secs))
        .build()
        .expect("build reqwest client")
}

// ── Anthropic (forced tool-use) ─────────────────────────────────────────────

pub struct AnthropicChat {
    client: reqwest::blocking::Client,
    base_url: String,
    api_key: String,
    model: String,
}

impl AnthropicChat {
    pub fn new(api_key: String, base_url: String, model: String) -> Self {
        Self { client: http_client(180), base_url, api_key, model }
    }
}

impl ChatCompleter for AnthropicChat {
    fn complete_json(
        &self,
        system: &str,
        user: &str,
        tool_name: &str,
        schema: &Value,
    ) -> Result<Value> {
        let body = json!({
            "model": self.model,
            "max_tokens": MAX_TOKENS,
            "system": system,
            "tools": [{
                "name": tool_name,
                "description": "Emit the structured result.",
                "input_schema": schema,
            }],
            "tool_choice": {"type": "tool", "name": tool_name},
            "messages": [{"role": "user", "content": user}],
        });
        let v = send_with_retry(tool_name, None, || {
            self.client
                .post(format!("{}/messages", self.base_url.trim_end_matches('/')))
                .header("x-api-key", &self.api_key)
                .header("anthropic-version", ANTHROPIC_VERSION)
                .json(&body)
        })?;
        v.get("content")
            .and_then(|c| c.as_array())
            .and_then(|arr| {
                arr.iter()
                    .find(|b| b.get("type").and_then(|t| t.as_str()) == Some("tool_use"))
            })
            .and_then(|b| b.get("input"))
            .cloned()
            .ok_or_else(|| SummarizeError::Decode("no tool_use block in response".into()))
    }
}

// ── OpenAI-compatible (json_object + schema-in-prompt) ───────────────────────

pub struct OpenAiChat {
    client: reqwest::blocking::Client,
    base_url: String,
    api_key: Option<String>,
    model: String,
    response_format: ResponseFormat,
}

impl OpenAiChat {
    pub fn new(
        api_key: Option<String>,
        base_url: String,
        model: String,
        response_format: ResponseFormat,
    ) -> Self {
        Self { client: http_client(180), base_url, api_key, model, response_format }
    }
}

/// Build the `response_format` body field for the chosen mode. `name` labels a
/// `json_schema` request (LM Studio echoes it back); `schema` is the same JSON
/// Schema embedded in the system contract. `strict` is `false`.
fn response_format_body(mode: ResponseFormat, name: &str, schema: &Value) -> Value {
    match mode {
        ResponseFormat::JsonObject => json!({"type": "json_object"}),
        ResponseFormat::JsonSchema => json!({
            "type": "json_schema",
            "json_schema": {"name": name, "strict": false, "schema": schema},
        }),
    }
}

impl ChatCompleter for OpenAiChat {
    fn complete_json(
        &self,
        system: &str,
        user: &str,
        tool_name: &str,
        schema: &Value,
    ) -> Result<Value> {
        // Embed the schema as a contract in the system message.
        let sys = format!(
            "{system}\n\nRespond with a single JSON object and nothing else. \
             It MUST conform to this JSON Schema:\n{}",
            serde_json::to_string(schema).unwrap_or_default(),
        );
        let mut body = json!({
            "model": self.model,
            "messages": [
                {"role": "system", "content": sys},
                {"role": "user", "content": user},
            ],
            "response_format": response_format_body(self.response_format, tool_name, schema),
        });
        // OpenAI's API rejects `max_tokens` on newer models and accepts
        // `max_completion_tokens` on all chat models; OpenAI-compatible
        // endpoints (Groq, LM Studio, Ollama) expect the legacy `max_tokens`.
        // The key switches only for api.openai.com.
        body[token_limit_key(&self.base_url)] = json!(MAX_TOKENS);
        let v = send_with_retry(tool_name, None, || {
            let mut req = self
                .client
                .post(format!("{}/chat/completions", self.base_url.trim_end_matches('/')))
                .json(&body);
            if let Some(k) = &self.api_key {
                req = req.bearer_auth(k);
            }
            req
        })?;
        let content = v
            .get("choices")
            .and_then(|c| c.as_array())
            .and_then(|a| a.first())
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_str())
            .ok_or_else(|| SummarizeError::Decode("no choices[0].message.content".into()))?;
        serde_json::from_str(content)
            .map_err(|e| SummarizeError::Decode(format!("content json: {e}")))
    }
}

// ── Daisy Cloud Gateway (Ed25519-signed OpenAI-compat, no `model`) ───────────
// Signing primitives live in `crate::gateway`. This is the ChatCompleter face
// for analysis/polish/chapters and, via OpenAICompatSummarizer wrapping it,
// summary.

/// Gateway transport. Sends Daisy's existing system prompt + schema as an
/// OpenAI-shaped body (minus `model` — the gateway routes by `X-Daisy-Task`),
/// signed per request with the per-install Ed25519 seed from the vault.
pub struct DaisyGatewayChat {
    client: reqwest::blocking::Client,
    creds: crate::gateway::GatewayCreds,
}

impl DaisyGatewayChat {
    pub fn new(install_id: String, license: String, seed: Vec<u8>, task: String) -> Self {
        Self {
            client: http_client(180),
            creds: crate::gateway::GatewayCreds { install_id, license, seed, task },
        }
    }
}

impl ChatCompleter for DaisyGatewayChat {
    fn complete_json(
        &self,
        system: &str,
        user: &str,
        _tool_name: &str,
        schema: &Value,
    ) -> Result<Value> {
        // Same schema-in-system contract as OpenAiChat, without a `model`
        // key; the gateway routes by task.
        let sys = format!(
            "{system}\n\nRespond with a single JSON object and nothing else. \
             It MUST conform to this JSON Schema:\n{}",
            serde_json::to_string(schema).unwrap_or_default(),
        );
        let body = json!({
            "messages": [
                {"role": "system", "content": sys},
                {"role": "user", "content": user},
            ],
            "response_format": {"type": "json_object"},
            "max_tokens": MAX_TOKENS,
        });
        // Serialized once; the exact bytes are hashed, signed, and sent.
        let body_bytes = serde_json::to_vec(&body)
            .map_err(|e| SummarizeError::Decode(format!("gateway body: {e}")))?;
        let v = send_with_retry(
            &self.creds.task,
            Some(&|| crate::gateway::register_install(&self.client, &self.creds)),
            || crate::gateway::request(&self.client, &self.creds, &body_bytes),
        )?;

        // Parse identically to OpenAiChat: choices[0].message.content is JSON.
        let content = v
            .get("choices")
            .and_then(|c| c.as_array())
            .and_then(|a| a.first())
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_str())
            .ok_or_else(|| SummarizeError::Decode("no choices[0].message.content".into()))?;
        serde_json::from_str(content)
            .map_err(|e| SummarizeError::Decode(format!("content json: {e}")))
    }
}

/// POST with 429 backoff. `feature` labels the call for logs + user-facing
/// errors ("summary", "chapters", …). `build` is called fresh per attempt.
/// Returns the parsed JSON body on 2xx.
fn send_with_retry<F>(
    feature: &str,
    register: Option<&dyn Fn() -> Result<bool>>,
    build: F,
) -> Result<Value>
where
    F: Fn() -> reqwest::blocking::RequestBuilder,
{
    let mut attempt = 0;
    let mut tried_register = false;
    loop {
        attempt += 1;
        let resp = build().send()?;
        let status = resp.status();
        let url = resp.url().clone();
        // Debug-only trace: endpoint + status, no content.
        crate::error::log_http(feature, "POST", url.as_str(), status.as_u16());
        if status.is_success() {
            return Ok(resp.json()?);
        }
        if status.as_u16() == 429 && attempt < MAX_ATTEMPTS {
            let retry_after = resp
                .headers()
                .get("retry-after")
                .and_then(|h| h.to_str().ok())
                .and_then(|s| s.parse::<u64>().ok());
            let wait = retry_after.unwrap_or(2u64.pow(attempt)).min(30);
            log::warn!("chat rate limited (429): waiting {wait}s then retry ({attempt}/{MAX_ATTEMPTS})");
            std::thread::sleep(std::time::Duration::from_secs(wait));
            continue;
        }
        let raw = resp.text().unwrap_or_default();
        // Self-heal a stale install registration exactly once: 403
        // install_not_registered (no pubkey on file) and 401 bad_signature
        // (pubkey on file doesn't match this install's current vault seed)
        // both resolve by re-registering via /api/activate, then retrying.
        // Only these exact errors — never expired/revoked/seat_limit — and
        // only when the register succeeds.
        if !tried_register && registration_heals(status.as_u16(), &raw) {
            if let Some(reg) = register {
                tried_register = true;
                if let Ok(true) = reg() {
                    log::info!(
                        "gateway {feature}: stale/missing install registration — registered pubkey, retrying once"
                    );
                    continue;
                }
                // register declined/failed → fall through and surface the error.
            }
        }
        return Err(SummarizeError::BadStatus {
            status: status.as_u16(),
            body: crate::error::friendly_http_error(feature, url.path(), status.as_u16(), &raw),
        });
    }
}

use crate::gateway::registration_heals;

/// Build the right `ChatCompleter` for a provider. `api_key` is required for
/// cloud providers (Anthropic/OpenAI/Groq); local providers (LM Studio/Ollama)
/// pass `None`.
pub fn build_chat_completer(
    provider: ChatProvider,
    api_key: Option<String>,
    base_url: String,
    model: String,
    response_format: ResponseFormat,
) -> Result<Box<dyn ChatCompleter>> {
    match provider {
        ChatProvider::Anthropic => {
            let key = api_key.ok_or(SummarizeError::MissingEnv("ANTHROPIC_API_KEY"))?;
            Ok(Box::new(AnthropicChat::new(key, base_url, model)))
        }
        ChatProvider::OpenAiCompat => {
            Ok(Box::new(OpenAiChat::new(api_key, base_url, model, response_format)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{response_format_body, token_limit_key};
    use crate::gateway::registration_heals;
    use crate::state_provider::ResponseFormat;
    use serde_json::json;

    #[test]
    fn lm_studio_maps_to_json_schema_others_to_json_object() {
        assert_eq!(ResponseFormat::for_provider("lm_studio"), ResponseFormat::JsonSchema);
        for p in ["ollama", "openai", "groq", "anything-else"] {
            assert_eq!(ResponseFormat::for_provider(p), ResponseFormat::JsonObject);
        }
    }

    #[test]
    fn response_format_body_shapes_per_mode() {
        let schema = json!({"type": "object", "properties": {"x": {"type": "string"}}});

        // json_object: bare type, no schema echoed.
        assert_eq!(
            response_format_body(ResponseFormat::JsonObject, "emit_summary", &schema),
            json!({"type": "json_object"})
        );

        // json_schema: LM Studio envelope with name + nested schema.
        let v = response_format_body(ResponseFormat::JsonSchema, "emit_summary", &schema);
        assert_eq!(v["type"], "json_schema");
        assert_eq!(v["json_schema"]["name"], "emit_summary");
        assert_eq!(v["json_schema"]["strict"], false);
        assert_eq!(v["json_schema"]["schema"], schema);
    }

    #[test]
    fn registration_heals_matches_exact_error_and_status_only() {
        assert!(registration_heals(403, r#"{"error":"install_not_registered"}"#));
        assert!(registration_heals(401, r#"{"error":"bad_signature"}"#));
        // Status/error pairs must match.
        assert!(!registration_heals(401, r#"{"error":"install_not_registered"}"#));
        assert!(!registration_heals(403, r#"{"error":"bad_signature"}"#));
        // Other auth failures do not trigger a retry.
        assert!(!registration_heals(403, r#"{"error":"expired"}"#));
        assert!(!registration_heals(403, r#"{"error":"revoked"}"#));
        assert!(!registration_heals(403, r#"{"error":"seat_limit"}"#));
        assert!(!registration_heals(404, r#"{"error":"not_entitled"}"#));
        assert!(!registration_heals(401, r#"{"error":"stale_timestamp"}"#));
        assert!(!registration_heals(401, r#"{"error":"invalid_license"}"#));
        assert!(!registration_heals(403, "not json"));
        assert!(!registration_heals(401, ""));
    }

    #[test]
    fn openai_uses_max_completion_tokens() {
        assert_eq!(token_limit_key("https://api.openai.com/v1"), "max_completion_tokens");
        assert_eq!(token_limit_key("https://api.openai.com"), "max_completion_tokens");
    }

    #[test]
    fn compatible_endpoints_keep_max_tokens() {
        assert_eq!(token_limit_key("https://api.groq.com/openai/v1"), "max_tokens");
        assert_eq!(token_limit_key("http://localhost:1234/v1"), "max_tokens"); // LM Studio
        assert_eq!(token_limit_key("http://localhost:11434/v1"), "max_tokens"); // Ollama
    }
}
