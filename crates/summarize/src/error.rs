//! Errors for the summarize crate.
pub type Result<T> = std::result::Result<T, SummarizeError>;

#[derive(Debug, thiserror::Error)]
pub enum SummarizeError {
    #[error("missing env var {0}")]
    MissingEnv(&'static str),
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    // `body` carries the contextual message built by `friendly_http_error`
    // (reason + endpoint + bounded provider body); Display is that line.
    // `status` is a discrete field for callers that branch on the code.
    #[error("{body}")]
    BadStatus { status: u16, body: String },
    #[error("could not parse provider response: {0}")]
    Decode(String),
    #[error("model output failed validation after retries: {0}")]
    InvalidOutput(String),
}

/// Truncate a provider HTTP error body before it is included in a
/// user-facing error string.
pub fn truncate_body(s: String) -> String {
    const MAX: usize = 500;
    if s.chars().count() <= MAX {
        return s;
    }
    let mut out: String = s.chars().take(MAX).collect();
    out.push_str("… (truncated)");
    out
}

/// Plain-language reason for a non-2xx provider HTTP status.
pub fn status_reason(status: u16) -> &'static str {
    match status {
        400 => "the request was rejected — likely an unsupported model or bad parameters",
        401 => "the API key was rejected (unauthorized)",
        403 => "access was forbidden — the key may lack permission, or the model/region is blocked",
        404 => "the endpoint or model wasn't found — check the base URL and model id",
        408 => "the provider timed out",
        413 | 422 => "the provider rejected the request — the transcript may be too long for this model",
        429 => "rate limited by the provider — too many requests",
        500..=599 => "the provider is having trouble (server error) — try again shortly",
        _ => "the provider returned an error",
    }
}

/// Build a contextual one-line error for a failed LLM HTTP call. `feature` =
/// the feature name ("Summary", "Chapters", "Q&A"); `endpoint` = the URL
/// path; `body` = the provider's error body (length-bounded here). Never
/// includes the request content or API key.
pub fn friendly_http_error(feature: &str, endpoint: &str, status: u16, body: &str) -> String {
    let detail = body.trim();
    let detail = if detail.is_empty() {
        String::new()
    } else {
        format!(" — {}", truncate_body(detail.to_string()))
    };
    format!(
        "{feature} failed: {} (HTTP {status} from {endpoint}){detail}",
        status_reason(status)
    )
}

/// Debug-only trace of an outbound LLM HTTP call: endpoint + status only, never
/// request/response content. Visible only when debug logging is enabled.
pub fn log_http(feature: &str, method: &str, url: &str, status: u16) {
    log::debug!("http {feature}: {method} {url} -> {status}");
}

/// True when a 400 response body says the model rejects a non-default
/// `temperature` (some OpenAI models accept only the default). Callers retry
/// once without the parameter.
pub fn temperature_rejected(status: u16, body: &str) -> bool {
    status == 400 && body.contains("temperature") && body.contains("unsupported_value")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn friendly_http_error_is_contextual_and_bounded() {
        let msg = friendly_http_error("Summary", "/v1/messages", 400, "{\"error\":\"bad model\"}");
        assert!(msg.starts_with("Summary failed:"), "{msg}");
        assert!(msg.contains("HTTP 400 from /v1/messages"), "{msg}");
        assert!(msg.contains("unsupported model"), "{msg}");
        assert!(msg.contains("bad model"), "{msg}");
    }

    #[test]
    fn friendly_http_error_handles_empty_body() {
        let msg = friendly_http_error("Q&A", "/chat/completions", 401, "   ");
        assert!(msg.contains("HTTP 401 from /chat/completions"), "{msg}");
        assert!(msg.contains("unauthorized"), "{msg}");
        // No dangling separator when the provider gave no body.
        assert!(!msg.contains("— "), "{msg}");
    }

    #[test]
    fn status_reason_covers_server_error_range() {
        assert!(status_reason(503).contains("server error"));
        assert_eq!(status_reason(429), status_reason(429));
    }

    #[test]
    fn temperature_rejection_detected() {
        let openai = r#"{ "error": { "message": "Unsupported value: 'temperature' does not support 0.2 with this model. Only the default (1) value is supported.", "type": "invalid_request_error", "param": "temperature", "code": "unsupported_value" } }"#;
        assert!(temperature_rejected(400, openai));
        assert!(!temperature_rejected(500, openai));
        assert!(!temperature_rejected(400, r#"{"error":{"message":"context length exceeded"}}"#));
        assert!(!temperature_rejected(400, ""));
    }
}
