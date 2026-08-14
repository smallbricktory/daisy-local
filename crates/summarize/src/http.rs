//! Shared resilient send for every LLM transport, blocking and async:
//! 429/Retry-After backoff, one-shot gateway registration self-heal, and a
//! per-attempt debug trace. Callers keep their own body building and
//! response parsing; only the send policy lives here.

use crate::gateway::{registration_heals, GatewayCreds};

/// Attempt cap for the 429 backoff. Retry-After is honoured when present;
/// otherwise the wait is 2^attempt seconds, capped at 30.
pub const MAX_ATTEMPTS: u32 = 4;

/// Terminal failure from [`send_with_retry`] / [`send_with_retry_async`].
#[derive(Debug)]
pub enum SendFailure {
    /// The request never produced an HTTP response.
    Transport(reqwest::Error),
    /// Non-2xx after retries/heal. `endpoint` is the URL path; `raw` is the
    /// provider's error body.
    Status {
        status: u16,
        endpoint: String,
        raw: String,
    },
}

impl SendFailure {
    /// Convert to a [`crate::SummarizeError`], building the friendly one-line
    /// message for `feature`.
    pub fn into_error(self, feature: &str) -> crate::SummarizeError {
        match self {
            Self::Transport(e) => crate::SummarizeError::Http(e),
            Self::Status { status, endpoint, raw } => crate::SummarizeError::BadStatus {
                status,
                body: crate::error::friendly_http_error(feature, &endpoint, status, &raw),
            },
        }
    }
}

fn backoff_secs(retry_after: Option<u64>, attempt: u32) -> u64 {
    retry_after.unwrap_or(2u64.pow(attempt)).min(30)
}

fn retry_after_secs(headers: &reqwest::header::HeaderMap) -> Option<u64> {
    headers
        .get("retry-after")
        .and_then(|h| h.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok())
}

fn log_rate_limited(wait: u64, attempt: u32) {
    log::warn!("chat rate limited (429): waiting {wait}s then retry ({attempt}/{MAX_ATTEMPTS})");
}

fn log_heal(feature: &str) {
    log::info!(
        "gateway {feature}: stale/missing install registration — registered pubkey, retrying once"
    );
}

/// POST with the shared resilience policy: retry 429s with backoff, and —
/// when `heal` carries gateway credentials — re-register the install pubkey
/// at most once per send attempt loop on 403 `install_not_registered` / 401
/// `bad_signature`
/// (never expired/revoked/seat_limit), then retry. `build` is called fresh
/// per attempt. Returns the 2xx response for the caller to parse.
pub fn send_with_retry(
    feature: &str,
    heal: Option<(&reqwest::blocking::Client, &GatewayCreds)>,
    build: &dyn Fn() -> reqwest::blocking::RequestBuilder,
) -> Result<reqwest::blocking::Response, SendFailure> {
    let mut attempt = 0;
    let mut tried_register = false;
    loop {
        attempt += 1;
        let resp = build().send().map_err(SendFailure::Transport)?;
        let status = resp.status().as_u16();
        let url = resp.url().clone();
        crate::error::log_http(feature, "POST", url.as_str(), status);
        if resp.status().is_success() {
            return Ok(resp);
        }
        if status == 429 && attempt < MAX_ATTEMPTS {
            let wait = backoff_secs(retry_after_secs(resp.headers()), attempt);
            log_rate_limited(wait, attempt);
            std::thread::sleep(std::time::Duration::from_secs(wait));
            continue;
        }
        let raw = resp.text().unwrap_or_default();
        if let Some((client, creds)) = heal {
            if !tried_register && registration_heals(status, &raw) {
                tried_register = true;
                if let Ok(true) = crate::gateway::register_install(client, creds) {
                    log_heal(feature);
                    continue;
                }
                // register declined/failed → fall through and surface the error.
            }
        }
        return Err(SendFailure::Status {
            status,
            endpoint: url.path().to_string(),
            raw,
        });
    }
}

/// Async sibling of [`send_with_retry`] for the streaming paths. The policy
/// applies to the initial request/response only — once a 2xx response is
/// returned, streaming its body is the caller's business.
pub async fn send_with_retry_async<F>(
    feature: &str,
    heal: Option<(&reqwest::Client, &GatewayCreds)>,
    build: F,
) -> Result<reqwest::Response, SendFailure>
where
    F: Fn() -> reqwest::RequestBuilder + Sync,
{
    let mut attempt = 0;
    let mut tried_register = false;
    loop {
        attempt += 1;
        let resp = build().send().await.map_err(SendFailure::Transport)?;
        let status = resp.status().as_u16();
        let url = resp.url().clone();
        crate::error::log_http(feature, "POST", url.as_str(), status);
        if resp.status().is_success() {
            return Ok(resp);
        }
        if status == 429 && attempt < MAX_ATTEMPTS {
            let wait = backoff_secs(retry_after_secs(resp.headers()), attempt);
            log_rate_limited(wait, attempt);
            tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
            continue;
        }
        let raw = resp.text().await.unwrap_or_default();
        if let Some((client, creds)) = heal {
            if !tried_register && registration_heals(status, &raw) {
                tried_register = true;
                if let Ok(true) = crate::gateway::register_install_async(client, creds).await {
                    log_heal(feature);
                    continue;
                }
            }
        }
        return Err(SendFailure::Status {
            status,
            endpoint: url.path().to_string(),
            raw,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_honours_retry_after_and_caps_at_30() {
        assert_eq!(backoff_secs(Some(7), 1), 7);
        assert_eq!(backoff_secs(Some(90), 1), 30);
        assert_eq!(backoff_secs(None, 1), 2);
        assert_eq!(backoff_secs(None, 3), 8);
        assert_eq!(backoff_secs(None, 5), 30);
    }

    #[test]
    fn send_failure_status_converts_to_friendly_bad_status() {
        let f = SendFailure::Status {
            status: 401,
            endpoint: "/v1/messages".into(),
            raw: "denied".into(),
        };
        match f.into_error("Summary") {
            crate::SummarizeError::BadStatus { status, body } => {
                assert_eq!(status, 401);
                assert!(body.contains("Summary failed:"), "{body}");
                assert!(body.contains("HTTP 401 from /v1/messages"), "{body}");
                assert!(body.contains("denied"), "{body}");
            }
            other => panic!("expected BadStatus, got {other:?}"),
        }
    }

    #[test]
    fn retries_429_then_succeeds() {
        let mut server = mockito::Server::new();
        let limited = server
            .mock("POST", "/chat/completions")
            .with_status(429)
            .with_header("retry-after", "0")
            .expect(1)
            .create();
        let ok = server
            .mock("POST", "/chat/completions")
            .with_status(200)
            .with_body("{}")
            .expect(1)
            .create();
        let client = reqwest::blocking::Client::new();
        let url = format!("{}/chat/completions", server.url());
        let resp = send_with_retry("test", None, &|| client.post(&url)).expect("2xx after retry");
        assert!(resp.status().is_success());
        limited.assert();
        ok.assert();
    }

    #[test]
    fn non_429_fails_without_retry() {
        let mut server = mockito::Server::new();
        let m = server
            .mock("POST", "/chat/completions")
            .with_status(400)
            .with_body("bad request")
            .expect(1)
            .create();
        let client = reqwest::blocking::Client::new();
        let url = format!("{}/chat/completions", server.url());
        let err = send_with_retry("test", None, &|| client.post(&url)).unwrap_err();
        match err {
            SendFailure::Status { status, raw, .. } => {
                assert_eq!(status, 400);
                assert_eq!(raw, "bad request");
            }
            other => panic!("expected Status, got {other:?}"),
        }
        m.assert();
    }
}
