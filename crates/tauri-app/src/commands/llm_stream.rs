//! Streaming free-text chat — async SSE counterpart to
//! `llm_text::complete_text`. Covers the two OpenAI-chunk SSE transports: the
//! Daisy gateway and OpenAI-compatible servers. Anthropic is not streamed
//! here; callers fall back to the blocking `complete_text` (see
//! [`provider_supports_streaming`]).

use crate::commands::llm_text::{wire_json, ChatTarget};
use crate::error::{AppError, Result};
use crate::state::ProviderId;
use futures_util::StreamExt as _;
use serde_json::json;
use std::time::Duration;

/// True when the provider speaks OpenAI-chunk SSE and is streamed here.
pub fn provider_supports_streaming(provider: ProviderId) -> bool {
    matches!(
        provider,
        ProviderId::DaisyGateway
            | ProviderId::Openai
            | ProviderId::Groq
            | ProviderId::LmStudio
            | ProviderId::Ollama
    )
}

/// One decoded SSE event from an OpenAI-chunk stream.
#[derive(Debug, PartialEq)]
pub(crate) enum SseEvent {
    /// A content delta to append + surface.
    Token(String),
    /// Terminal `[DONE]` sentinel — stop reading.
    Done,
    /// An error object arrived mid-stream — abort with this message.
    Error(String),
}

/// Parse the payload after `data:` in one SSE line (OpenAI chat-completion
/// chunk shape, also what the gateway emits). Returns None for keepalives /
/// empty / shapes carrying no content delta.
pub(crate) fn parse_sse_data(payload: &str) -> Option<SseEvent> {
    let p = payload.trim();
    if p.is_empty() {
        return None;
    }
    if p == "[DONE]" {
        return Some(SseEvent::Done);
    }
    let v: serde_json::Value = serde_json::from_str(p).ok()?;
    if let Some(err) = v.get("error") {
        let msg = err
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("stream error");
        return Some(SseEvent::Error(msg.to_string()));
    }
    let delta = v
        .get("choices")?
        .as_array()?
        .first()?
        .get("delta")?
        .get("content")?
        .as_str()?;
    if delta.is_empty() {
        return None;
    }
    Some(SseEvent::Token(delta.to_string()))
}

fn http_async() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(300))
        .build()
        .map_err(|e| AppError::Config(format!("build http client: {e}")))
}

/// Stream a free-text completion, invoking `on_token` for each content delta
/// and returning the fully-assembled answer. The provider must satisfy
/// [`provider_supports_streaming`]; any other provider is an error.
pub async fn complete_text_streaming(
    target: &ChatTarget,
    system: &str,
    messages: &[(String, String)],
    feature: &str,
    max_out: u32,
    on_token: impl FnMut(&str),
) -> Result<String> {
    complete_text_streaming_inner(target, system, messages, feature, max_out, on_token, false).await
}

async fn complete_text_streaming_inner(
    target: &ChatTarget,
    system: &str,
    messages: &[(String, String)],
    feature: &str,
    max_out: u32,
    mut on_token: impl FnMut(&str),
    drop_temperature: bool,
) -> Result<String> {
    let client = http_async()?;
    let resp = match target.provider {
        ProviderId::Openai | ProviderId::Groq | ProviderId::LmStudio | ProviderId::Ollama => {
            let mut body = json!({
                "model": target.model,
                "temperature": 0.2,
                "stream": true,
                "messages": wire_json(system, messages, true),
            });
            if drop_temperature {
                body.as_object_mut().unwrap().remove("temperature");
            }
            body[summarize::chat::token_limit_key(&target.base_url)] = json!(max_out);
            let mut rb = client.post(format!(
                "{}/chat/completions",
                target.base_url.trim_end_matches('/')
            ));
            if let Some(k) = target.api_key.as_deref() {
                rb = rb.header("authorization", format!("Bearer {k}"));
            }
            rb.json(&body).send().await
        }
        ProviderId::DaisyGateway => {
            let creds = target.gateway.as_ref().ok_or_else(|| {
                AppError::Config("Daisy Cloud isn't set up — select it in Settings → Providers.".into())
            })?;
            let body = json!({
                "messages": wire_json(system, messages, true),
                "max_tokens": max_out,
                "temperature": 0.2,
                "stream": true,
            });
            let body_bytes = serde_json::to_vec(&body)
                .map_err(|e| AppError::Config(format!("gateway body: {e}")))?;
            summarize::gateway::request_async(&client, creds, &body_bytes).send().await
        }
        other => {
            return Err(AppError::Config(format!(
                "{other} does not support streaming"
            )))
        }
    };

    let resp =
        resp.map_err(|e| AppError::Provider(format!("{feature} failed: couldn't reach the AI provider. {e}")))?;
    let status = resp.status();
    let endpoint = resp.url().path().to_string();
    summarize::error::log_http(feature, "POST", resp.url().as_str(), status.as_u16());
    if status.as_u16() == 404 && target.provider == ProviderId::DaisyGateway {
        return Err(AppError::GatewayNotEntitled);
    }
    if !status.is_success() {
        // Pre-stream failure: read the (non-SSE) body for a friendly message.
        let raw = resp.text().await.unwrap_or_default();
        if !drop_temperature && summarize::error::temperature_rejected(status.as_u16(), &raw) {
            // Retry once without `temperature` (some models accept only the default).
            return Box::pin(complete_text_streaming_inner(
                target, system, messages, feature, max_out, on_token, true,
            ))
            .await;
        }
        return Err(AppError::Provider(summarize::error::friendly_http_error(
            feature,
            &endpoint,
            status.as_u16(),
            &raw,
        )));
    }

    // If the server returned a normal JSON completion instead of SSE, parse
    // the whole body and emit it as one token.
    let is_sse = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|ct| ct.contains("text/event-stream"))
        .unwrap_or(false);
    if is_sse {
        read_sse_stream(resp, on_token).await
    } else {
        let v: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| AppError::Config(format!("chat decode: {e}")))?;
        let text = crate::commands::llm_text::extract_openai(&v);
        on_token(&text);
        Ok(text)
    }
}

/// Drive the SSE body: accumulate bytes, split on newlines, parse each
/// `data:` line, call `on_token` per delta, and return the assembled text.
/// Stops on `[DONE]`; a mid-stream error event aborts. A stream that simply
/// closes returns whatever was assembled.
async fn read_sse_stream(
    resp: reqwest::Response,
    mut on_token: impl FnMut(&str),
) -> Result<String> {
    let mut acc = String::new();
    let mut buf = String::new();
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let bytes = chunk.map_err(|e| AppError::Provider(format!("stream read: {e}")))?;
        buf.push_str(&String::from_utf8_lossy(&bytes));
        while let Some(nl) = buf.find('\n') {
            let line: String = buf.drain(..=nl).collect();
            let line = line.trim_end_matches(['\r', '\n']);
            // SSE comment (keepalive ": ping") and blank lines: skip.
            let Some(payload) = line.strip_prefix("data:") else {
                continue;
            };
            match parse_sse_data(payload) {
                Some(SseEvent::Token(t)) => {
                    on_token(&t);
                    acc.push_str(&t);
                }
                Some(SseEvent::Done) => return Ok(acc),
                Some(SseEvent::Error(e)) => return Err(AppError::Provider(e)),
                None => {}
            }
        }
    }
    Ok(acc)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_openai_content_delta() {
        let p = parse_sse_data(r#"{"choices":[{"delta":{"content":"Hel"}}]}"#);
        assert_eq!(p, Some(SseEvent::Token("Hel".into())));
    }

    #[test]
    fn done_sentinel() {
        assert_eq!(parse_sse_data("[DONE]"), Some(SseEvent::Done));
        assert_eq!(parse_sse_data(" [DONE] "), Some(SseEvent::Done));
    }

    #[test]
    fn error_object_surfaces_message() {
        let p = parse_sse_data(r#"{"error":{"message":"upstream boom"}}"#);
        assert_eq!(p, Some(SseEvent::Error("upstream boom".into())));
    }

    #[test]
    fn role_only_and_empty_deltas_ignored() {
        // First chunk often carries role but no content.
        assert_eq!(parse_sse_data(r#"{"choices":[{"delta":{"role":"assistant"}}]}"#), None);
        assert_eq!(parse_sse_data(r#"{"choices":[{"delta":{"content":""}}]}"#), None);
        // finish chunk.
        assert_eq!(parse_sse_data(r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#), None);
    }

    #[test]
    fn blank_and_garbage_ignored() {
        assert_eq!(parse_sse_data(""), None);
        assert_eq!(parse_sse_data("   "), None);
        assert_eq!(parse_sse_data("not json"), None);
    }

    #[test]
    fn streaming_support_matrix() {
        assert!(provider_supports_streaming(ProviderId::DaisyGateway));
        assert!(provider_supports_streaming(ProviderId::Openai));
        assert!(provider_supports_streaming(ProviderId::Groq));
        assert!(provider_supports_streaming(ProviderId::LmStudio));
        assert!(provider_supports_streaming(ProviderId::Ollama));
        // Anthropic speaks its own Messages API, not OpenAI-chunk SSE.
        assert!(!provider_supports_streaming(ProviderId::Anthropic));
    }
}
