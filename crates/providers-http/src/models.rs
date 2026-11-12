// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Discover which models a provider exposes. OpenAI-compatible providers
//! (Groq, OpenAI, LM Studio, Ollama) answer `GET {base}/models`. The same
//! call doubles as an API-key test: a bad key yields a non-2xx status.

use crate::error::{ProviderError, Result};
use serde::Deserialize;
use std::time::Duration;

fn default_base_url(provider: &str) -> &'static str {
    match provider {
        "groq" => "https://api.groq.com/openai/v1",
        "openai" => "https://api.openai.com/v1",
        "anthropic" => "https://api.anthropic.com/v1",
        "lm_studio" => "http://localhost:1234/v1",
        "ollama" => "http://localhost:11434/v1",
        _ => "",
    }
}

/// Canonical OpenAI-compatible base URL for `provider`. For the local servers
/// (LM Studio / Ollama), `/v1` is appended when missing. Both the model-list
/// probe and the chat path use this base. Cloud bases already carry their
/// version path and are returned trimmed-only.
pub fn normalize_compat_base(provider: &str, base: &str) -> String {
    let trimmed = base.trim_end_matches('/');
    let needs_v1 = matches!(provider, "lm_studio" | "ollama")
        && !trimmed.ends_with("/v1")
        && !trimmed.contains("/v1/");
    if needs_v1 {
        format!("{trimmed}/v1")
    } else {
        trimmed.to_string()
    }
}

fn build_models_url(provider: &str, base: &str) -> String {
    format!("{}/models", normalize_compat_base(provider, base))
}

const ANTHROPIC_VERSION: &str = "2023-06-01";

/// Return the model IDs `provider` advertises. `api_key` is sent as a bearer
/// token when present. `base_url` overrides the provider default.
pub fn list_models(
    provider: &str,
    api_key: Option<&str>,
    base_url: Option<&str>,
) -> Result<Vec<String>> {
    let base = base_url
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| default_base_url(provider));
    if base.is_empty() {
        return Err(ProviderError::Other(format!(
            "no default base URL for provider {provider:?}; supply one"
        )));
    }
    // Accepts both `http://host:port` and `http://host:port/v1` for the
    // local OpenAI-compat servers; for every other provider, `/models` is
    // appended to the caller's base.
    let url = build_models_url(provider, base);

    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .expect("build reqwest client");
    let mut req = client.get(&url);
    if provider == "anthropic" {
        // Anthropic auth is `x-api-key`, not bearer, plus a version header.
        req = req.header("anthropic-version", ANTHROPIC_VERSION);
        if let Some(k) = api_key.filter(|s| !s.is_empty()) {
            req = req.header("x-api-key", k);
        }
    } else if let Some(k) = api_key.filter(|s| !s.is_empty()) {
        req = req.bearer_auth(k);
    }
    let resp = req.send()?;
    let status = resp.status();
    if !status.is_success() {
        return Err(ProviderError::BadStatus {
            status: status.as_u16(),
            body: resp.text().unwrap_or_default(),
        });
    }

    #[derive(Deserialize)]
    struct ModelList {
        #[serde(default)]
        data: Vec<ModelEntry>,
    }
    #[derive(Deserialize)]
    struct ModelEntry {
        id: String,
    }
    let parsed: ModelList = resp.json()?;
    let mut ids: Vec<String> = parsed.data.into_iter().map(|m| m.id).collect();
    ids.sort();
    ids.dedup();
    Ok(ids)
}

/// LM Studio's native REST base (`/api/v0`) for an OpenAI-compatible base:
/// the same host, `/v1` dropped.
fn lm_studio_rest_base(base: &str) -> String {
    let compat = normalize_compat_base("lm_studio", base);
    let host = compat.strip_suffix("/v1").unwrap_or(&compat);
    format!("{}/api/v0", host.trim_end_matches('/'))
}

/// The context length LM Studio has loaded `model` with, from its native
/// `GET /api/v0/models`. `None` when the model is listed without a loaded
/// context (not loaded) or is absent.
pub fn lm_studio_loaded_context(base_url: Option<&str>, model: &str) -> Result<Option<u32>> {
    let base = base_url
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| default_base_url("lm_studio"));
    let url = format!("{}/models", lm_studio_rest_base(base));
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .expect("build reqwest client");
    let resp = client.get(&url).send()?;
    let status = resp.status();
    if !status.is_success() {
        return Err(ProviderError::BadStatus {
            status: status.as_u16(),
            body: resp.text().unwrap_or_default(),
        });
    }
    Ok(loaded_context_of(&resp.text()?, model))
}

fn loaded_context_of(body: &str, model: &str) -> Option<u32> {
    #[derive(Deserialize)]
    struct List {
        #[serde(default)]
        data: Vec<Entry>,
    }
    #[derive(Deserialize)]
    struct Entry {
        id: String,
        #[serde(default)]
        loaded_context_length: Option<u32>,
    }
    let list: List = serde_json::from_str(body).ok()?;
    list.data
        .into_iter()
        .find(|e| e.id == model)
        .and_then(|e| e.loaded_context_length)
        .filter(|n| *n > 0)
}

#[cfg(test)]
mod tests {
    use super::{build_models_url, lm_studio_rest_base, loaded_context_of, normalize_compat_base};

    #[test]
    fn lm_studio_rest_base_drops_v1_and_adds_api_v0() {
        assert_eq!(lm_studio_rest_base("http://localhost:1234/v1"), "http://localhost:1234/api/v0");
        assert_eq!(lm_studio_rest_base("http://localhost:1234"), "http://localhost:1234/api/v0");
        assert_eq!(lm_studio_rest_base("http://10.0.0.5:1234/v1/"), "http://10.0.0.5:1234/api/v0");
    }

    #[test]
    fn loaded_context_reads_the_chosen_model_only_when_loaded() {
        let body = r#"{"object":"list","data":[
            {"id":"qwen3.5-4b","object":"model","state":"loaded","max_context_length":262144,"loaded_context_length":32768},
            {"id":"phi-4","object":"model","state":"not-loaded","max_context_length":16384}
        ]}"#;
        assert_eq!(loaded_context_of(body, "qwen3.5-4b"), Some(32768));
        assert_eq!(loaded_context_of(body, "phi-4"), None);
        assert_eq!(loaded_context_of(body, "missing"), None);
        assert_eq!(loaded_context_of("not json", "qwen3.5-4b"), None);
    }

    #[test]
    fn normalize_inserts_v1_only_for_local_bare_host() {
        assert_eq!(normalize_compat_base("lm_studio", "http://localhost:1234"), "http://localhost:1234/v1");
        assert_eq!(normalize_compat_base("lm_studio", "http://localhost:1234/v1"), "http://localhost:1234/v1");
        assert_eq!(normalize_compat_base("ollama", "http://localhost:11434/"), "http://localhost:11434/v1");
        // a base ending in /api/v1 is left as-is (it already ends with /v1).
        assert_eq!(
            normalize_compat_base("lm_studio", "http://localhost:1234/api/v1"),
            "http://localhost:1234/api/v1"
        );
        // cloud bases pass through untouched.
        assert_eq!(normalize_compat_base("openai", "https://api.openai.com/v1"), "https://api.openai.com/v1");
    }

    #[test]
    fn lm_studio_appends_v1_when_missing() {
        assert_eq!(
            build_models_url("lm_studio", "http://localhost:1234"),
            "http://localhost:1234/v1/models"
        );
    }

    #[test]
    fn lm_studio_keeps_v1_when_present() {
        assert_eq!(
            build_models_url("lm_studio", "http://localhost:1234/v1"),
            "http://localhost:1234/v1/models"
        );
        // trailing slash too
        assert_eq!(
            build_models_url("lm_studio", "http://localhost:1234/v1/"),
            "http://localhost:1234/v1/models"
        );
    }

    #[test]
    fn ollama_appends_v1_when_missing() {
        assert_eq!(
            build_models_url("ollama", "http://localhost:11434"),
            "http://localhost:11434/v1/models"
        );
    }

    #[test]
    fn cloud_providers_keep_base_verbatim() {
        // Cloud bases already include `/v1`; just append `/models`.
        assert_eq!(
            build_models_url("openai", "https://api.openai.com/v1"),
            "https://api.openai.com/v1/models"
        );
        assert_eq!(
            build_models_url("groq", "https://api.groq.com/openai/v1"),
            "https://api.groq.com/openai/v1/models"
        );
    }
}


/// Probe whether the provider serves `POST /chat/completions` at this base.
/// Applied only to the local servers (LM Studio / Ollama); other providers
/// pass unconditionally. A 404/405 response means the route is not served;
/// any other status (including 400 for the minimal body) means it is.
pub fn probe_chat_path(provider: &str, base_url: Option<&str>, api_key: Option<&str>) -> Result<()> {
    if !matches!(provider, "lm_studio" | "ollama") {
        return Ok(());
    }
    let base = base_url
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| default_base_url(provider));
    let url = format!("{}/chat/completions", normalize_compat_base(provider, base));
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .expect("build reqwest client");
    let mut req = client
        .post(&url)
        .header("content-type", "application/json")
        // The body carries no model field; the server answers 200 or 400,
        // either of which indicates the route exists.
        .body(r#"{"messages":[{"role":"user","content":"ping"}],"max_tokens":1}"#);
    if let Some(k) = api_key.filter(|s| !s.is_empty()) {
        req = req.bearer_auth(k);
    }
    let status = req.send()?.status();
    if status.as_u16() == 404 || status.as_u16() == 405 {
        return Err(ProviderError::Other(format!(
            "this URL has no /chat/completions endpoint (HTTP {}). For LM Studio, use the \
             OpenAI-compatible server base — e.g. http://localhost:1234/v1 — not the \
             /api/v1 REST API.",
            status.as_u16()
        )));
    }
    Ok(())
}
