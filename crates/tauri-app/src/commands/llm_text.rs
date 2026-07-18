//! Shared free-text LLM chat for the command layer. Resolves provider/creds
//! and speaks three transports: Anthropic /messages, OpenAI-compatible
//! /chat/completions, and the Ed25519-signed Daisy gateway.

use crate::error::{AppError, Result};
use crate::state::{AppState, ProviderId, VaultState};
use serde_json::json;

/// Resolved provider + credentials for a free-text chat call.
pub struct ChatTarget {
    pub provider: ProviderId,
    pub api_key: Option<String>,
    pub base_url: String,
    pub model: String,
    pub gateway: Option<summarize::gateway::GatewayCreds>,
}

/// Resolve the chat provider and its credentials. `provider_override` falls
/// back to `settings.default_summary_provider`; `task` labels the gateway
/// request.
pub fn resolve_chat_target(
    app: &AppState,
    vs: &VaultState,
    provider_override: Option<ProviderId>,
    task: &str,
) -> Result<ChatTarget> {
    let settings = crate::settings::Settings::load_or_default(
        &app.profile.root().join("settings.json"),
    );
    let provider = provider_override
        .or(settings.default_summary_provider)
        .ok_or_else(|| AppError::Config(
            "No AI provider configured. Pick one in Settings → Providers.".into(),
        ))?;
    let (provider_cfg, gateway) = {
        let g = vs.keys.lock().unwrap();
        let keys = g.as_ref().ok_or(AppError::VaultLocked)?;
        let cfg = keys.providers.get(&provider).cloned();
        let gw = if provider == ProviderId::DaisyGateway {
            Some(crate::commands::summary::gateway_creds_from_keys(keys, task)?)
        } else {
            None
        };
        (cfg, gw)
    };
    // Daisy Cloud carries no api key / base url / model.
    let (api_key, base_url, model) = if provider == ProviderId::DaisyGateway {
        (None, String::new(), String::new())
    } else {
        crate::commands::summary::resolve_summary_creds(provider, None, None, provider_cfg.as_ref())?
    };
    if matches!(
        provider,
        ProviderId::Anthropic | ProviderId::Openai | ProviderId::Groq
    ) && api_key.is_none()
    {
        return Err(AppError::Config(format!(
            "{provider} API key not set in the vault — open Settings → Providers"
        )));
    }
    Ok(ChatTarget { provider, api_key, base_url, model, gateway })
}

/// Run a free-text completion: `system` prompt plus a conversation of
/// `(role, content)` turns.
pub fn complete_text(
    target: &ChatTarget,
    system: &str,
    messages: &[(String, String)],
    feature: &str,
    max_out: u32,
) -> Result<String> {
    match target.provider {
        ProviderId::Anthropic => {
            call_anthropic(target.api_key.as_deref().unwrap_or(""), &target.base_url, &target.model, system, messages, feature, max_out)
        }
        ProviderId::Openai | ProviderId::Groq | ProviderId::LmStudio | ProviderId::Ollama => {
            call_openai_compat(target.api_key.as_deref(), &target.base_url, &target.model, system, messages, feature, max_out)
        }
        ProviderId::DaisyGateway => {
            let creds = target.gateway.as_ref().ok_or_else(|| {
                AppError::Config("Daisy Cloud isn't set up — select it in Settings → Providers.".into())
            })?;
            call_gateway(creds, system, messages, feature, max_out)
        }
    }
}

fn http() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .expect("build reqwest")
}

pub(crate) fn wire_json(system: &str, messages: &[(String, String)], include_system: bool) -> Vec<serde_json::Value> {
    let mut out = Vec::with_capacity(messages.len() + 1);
    if include_system {
        out.push(json!({"role": "system", "content": system}));
    }
    for (role, content) in messages {
        out.push(json!({"role": role, "content": content}));
    }
    out
}

pub(crate) fn extract_openai(v: &serde_json::Value) -> String {
    v.get("choices")
        .and_then(|c| c.as_array())
        .and_then(|a| a.first())
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .unwrap_or("(no answer)")
        .to_string()
}

fn call_gateway(
    creds: &summarize::gateway::GatewayCreds,
    system: &str,
    messages: &[(String, String)],
    feature: &str,
    max_out: u32,
) -> Result<String> {
    let body = json!({ "messages": wire_json(system, messages, true), "max_tokens": max_out, "temperature": 0.2 });
    let body_bytes =
        serde_json::to_vec(&body).map_err(|e| AppError::Config(format!("gateway body: {e}")))?;
    let resp = summarize::gateway::request(&http(), creds, &body_bytes)
        .send()
        .map_err(|e| AppError::Provider(format!("{feature} failed: couldn't reach Daisy Cloud. {e}")))?;
    let status = resp.status();
    let endpoint = resp.url().path().to_string();
    summarize::error::log_http(feature, "POST", resp.url().as_str(), status.as_u16());
    if status.as_u16() == 404 {
        return Err(AppError::GatewayNotEntitled);
    }
    if !status.is_success() {
        let raw = resp.text().unwrap_or_default();
        return Err(AppError::Provider(summarize::error::friendly_http_error(feature, &endpoint, status.as_u16(), &raw)));
    }
    let v: serde_json::Value = resp.json().map_err(|e| AppError::Config(format!("chat decode: {e}")))?;
    Ok(extract_openai(&v))
}

fn call_openai_compat(
    api_key: Option<&str>,
    base_url: &str,
    model: &str,
    system: &str,
    messages: &[(String, String)],
    feature: &str,
    max_out: u32,
) -> Result<String> {
    let mut body = json!({ "model": model, "temperature": 0.2, "messages": wire_json(system, messages, true) });
    // The token-limit key differs by server; see
    // summarize::chat::token_limit_key.
    body[summarize::chat::token_limit_key(base_url)] = json!(max_out);
    // Second attempt drops `temperature` when the model only accepts the default.
    for attempt in 0..2 {
        let mut rb = http().post(format!("{}/chat/completions", base_url.trim_end_matches('/')));
        if let Some(k) = api_key {
            rb = rb.header("authorization", format!("Bearer {k}"));
        }
        let resp = rb
            .json(&body)
            .send()
            .map_err(|e| AppError::Provider(format!("{feature} failed: couldn't reach the AI provider. {e}")))?;
        let status = resp.status();
        let endpoint = resp.url().path().to_string();
        summarize::error::log_http(feature, "POST", resp.url().as_str(), status.as_u16());
        if !status.is_success() {
            let raw = resp.text().unwrap_or_default();
            if attempt == 0 && summarize::error::temperature_rejected(status.as_u16(), &raw) {
                body.as_object_mut().unwrap().remove("temperature");
                continue;
            }
            return Err(AppError::Provider(summarize::error::friendly_http_error(feature, &endpoint, status.as_u16(), &raw)));
        }
        let v: serde_json::Value = resp.json().map_err(|e| AppError::Config(format!("chat decode: {e}")))?;
        return Ok(extract_openai(&v));
    }
    unreachable!()
}

fn call_anthropic(
    api_key: &str,
    base_url: &str,
    model: &str,
    system: &str,
    messages: &[(String, String)],
    feature: &str,
    max_out: u32,
) -> Result<String> {
    let body = json!({
        "model": model,
        "max_tokens": max_out,
        "system": system,
        "messages": wire_json(system, messages, false),
    });
    let resp = http()
        .post(format!("{}/messages", base_url.trim_end_matches('/')))
        .header("x-api-key", api_key)
        .header("anthropic-version", "2023-06-01")
        .json(&body)
        .send()
        .map_err(|e| AppError::Provider(format!("{feature} failed: couldn't reach the AI provider. {e}")))?;
    let status = resp.status();
    let endpoint = resp.url().path().to_string();
    summarize::error::log_http(feature, "POST", resp.url().as_str(), status.as_u16());
    if !status.is_success() {
        let raw = resp.text().unwrap_or_default();
        return Err(AppError::Provider(summarize::error::friendly_http_error(feature, &endpoint, status.as_u16(), &raw)));
    }
    let v: serde_json::Value = resp.json().map_err(|e| AppError::Config(format!("anthropic decode: {e}")))?;
    Ok(v.get("content")
        .and_then(|c| c.as_array())
        .and_then(|arr| arr.iter().find(|b| b.get("type").and_then(|t| t.as_str()) == Some("text")))
        .and_then(|b| b.get("text"))
        .and_then(|t| t.as_str())
        .unwrap_or("(no answer)")
        .to_string())
}
