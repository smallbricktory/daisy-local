// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
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
    pub local: Option<std::sync::Arc<summarize::local::LocalChat>>,
    /// The context a local endpoint serves; `None` for cloud providers.
    pub budget: Option<summarize::ContextBudget>,
}

/// Resolve the chat provider and its credentials. `provider_override` falls
/// back to this machine's AI provider; `task` labels the gateway
/// request.
pub fn resolve_chat_target(
    app: &AppState,
    vs: &VaultState,
    provider_override: Option<ProviderId>,
    task: &str,
) -> Result<ChatTarget> {
    let resolved = crate::commands::llm::resolve_provider(app, vs, provider_override, task)?;
    let provider = resolved.provider;
    let budget = resolved.context_budget();
    // Daisy Cloud and the built-in model carry no api key / base url / model.
    let (api_key, base_url, model) = if matches!(provider, ProviderId::DaisyGateway | ProviderId::Local) {
        (None, String::new(), String::new())
    } else {
        crate::commands::summary::resolve_summary_creds(provider, None, None, resolved.cfg.as_ref())?
    };
    if matches!(
        provider,
        ProviderId::Anthropic | ProviderId::Openai | ProviderId::Groq
    ) && api_key.is_none()
    {
        return Err(AppError::Config(format!(
            "{provider} API key not set in the vault. Add it in {}.",
            crate::settings::providers_hint()
        )));
    }
    Ok(ChatTarget { provider, api_key, base_url, model, gateway: resolved.gateway, local: resolved.local, budget })
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
                AppError::Config("Daisy Cloud isn't set up — select it in Settings → Recordings.".into())
            })?;
            call_gateway(creds, system, messages, feature, max_out)
        }
        ProviderId::Local => {
            let chat = target.local.as_ref().ok_or_else(|| {
                let hint = crate::commands::llm_models::download_hint();
                AppError::Config(format!("Built-in model isn't downloaded. {hint}"))
            })?;
            chat.complete_text(system, messages, max_out)
                .map_err(|e| AppError::Provider(format!("{feature} failed: {e}")))
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
    let client = http();
    let resp = summarize::http::send_with_retry(feature, Some((&client, creds)), &|| {
        summarize::gateway::request(&client, creds, &body_bytes)
    })
    .map_err(|f| crate::commands::llm::map_send_failure(true, feature, f))?;
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
    let client = http();
    let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));
    // Second attempt drops `temperature` when the model only accepts the default.
    for attempt in 0..2 {
        let resp = summarize::http::send_with_retry(feature, None, &|| {
            let mut rb = client.post(&url);
            if let Some(k) = api_key {
                rb = rb.header("authorization", format!("Bearer {k}"));
            }
            rb.json(&body)
        });
        let resp = match resp {
            Ok(r) => r,
            Err(summarize::http::SendFailure::Status { status, ref raw, .. })
                if attempt == 0 && summarize::error::temperature_rejected(status, raw) =>
            {
                body.as_object_mut().unwrap().remove("temperature");
                continue;
            }
            Err(f) => return Err(crate::commands::llm::map_send_failure(false, feature, f)),
        };
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
    let client = http();
    let url = format!("{}/messages", base_url.trim_end_matches('/'));
    let resp = summarize::http::send_with_retry(feature, None, &|| {
        client
            .post(&url)
            .header("x-api-key", api_key)
            .header("anthropic-version", "2023-06-01")
            .json(&body)
    })
    .map_err(|f| crate::commands::llm::map_send_failure(false, feature, f))?;
    let v: serde_json::Value = resp.json().map_err(|e| AppError::Config(format!("anthropic decode: {e}")))?;
    Ok(v.get("content")
        .and_then(|c| c.as_array())
        .and_then(|arr| arr.iter().find(|b| b.get("type").and_then(|t| t.as_str()) == Some("text")))
        .and_then(|b| b.get("text"))
        .and_then(|t| t.as_str())
        .unwrap_or("(no answer)")
        .to_string())
}
