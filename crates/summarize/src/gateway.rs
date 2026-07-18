//! Shared Daisy Cloud gateway primitives: per-request Ed25519 signing + the
//! six `X-Daisy-*` headers, used by every transport that routes to the
//! gateway (the summary `Summarizer`, the `ChatCompleter`, and the `ask`/qa
//! path).

use crate::error::{Result, SummarizeError};

pub const GATEWAY_BASE: &str = "https://daisy.smbr.app/api/gateway/v1";
const GATEWAY_PATH: &str = "/api/gateway/v1/chat/completions";
/// Seat-claim + pubkey-registration endpoint. Re-POSTing is idempotent: it
/// re-claims the already-held seat and (re)writes the install pubkey.
const ACTIVATE_URL: &str = "https://daisy.smbr.app/api/activate";

/// Per-install credentials needed to sign one gateway request.
#[derive(Clone)]
pub struct GatewayCreds {
    pub install_id: String,
    pub license: String,
    /// 32-byte Ed25519 seed (from the unlocked vault).
    pub seed: Vec<u8>,
    /// `X-Daisy-Task`: summary | chapters | analysis | ask | polish.
    pub task: String,
}

/// The fixed gateway endpoint.
pub fn url() -> String {
    format!("{GATEWAY_BASE}/chat/completions")
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(bytes);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// The exact 5-line canonical string the gateway signs over (no trailing
/// newline): METHOD, path, timestamp, nonce, sha256-hex of the body bytes.
pub fn canonical_string(ts: &str, nonce: &str, body: &[u8]) -> String {
    format!("POST\n{GATEWAY_PATH}\n{ts}\n{nonce}\n{}", sha256_hex(body))
}

/// Ed25519-sign `canon` with a 32-byte seed; return the base64 signature.
pub fn sign(seed: &[u8], canon: &str) -> Result<String> {
    use base64::{engine::general_purpose::STANDARD as B64, Engine};
    use ed25519_dalek::{Signer, SigningKey};
    let arr: [u8; 32] = seed
        .try_into()
        .map_err(|_| SummarizeError::Decode("install seed must be 32 bytes".into()))?;
    let signing = SigningKey::from_bytes(&arr);
    Ok(B64.encode(signing.sign(canon.as_bytes()).to_bytes()))
}

/// Build a fully-signed POST to the gateway for `body_bytes`. A fresh
/// timestamp + nonce is generated each call. The caller MUST send exactly
/// `body_bytes` (it is what was hashed + signed).
pub fn request(
    client: &reqwest::blocking::Client,
    creds: &GatewayCreds,
    body_bytes: &[u8],
) -> reqwest::blocking::RequestBuilder {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
        .to_string();
    let nonce = uuid::Uuid::new_v4().to_string();
    let canon = canonical_string(&ts, &nonce, body_bytes);
    let sig = sign(&creds.seed, &canon).unwrap_or_default();
    client
        .post(url())
        .header("content-type", "application/json")
        .header("X-Daisy-License", &creds.license)
        .header("X-Daisy-Install", &creds.install_id)
        .header("X-Daisy-Task", &creds.task)
        .header("X-Daisy-Timestamp", ts)
        .header("X-Daisy-Nonce", nonce)
        .header("X-Daisy-Signature", sig)
        .body(body_bytes.to_vec())
}

/// Async sibling of [`request`] for the streaming chat path: identical signing
/// + six `X-Daisy-*` headers, on an async [`reqwest::Client`]. Same contract —
/// the caller MUST send exactly `body_bytes` (it's what was hashed + signed).
pub fn request_async(
    client: &reqwest::Client,
    creds: &GatewayCreds,
    body_bytes: &[u8],
) -> reqwest::RequestBuilder {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
        .to_string();
    let nonce = uuid::Uuid::new_v4().to_string();
    let canon = canonical_string(&ts, &nonce, body_bytes);
    let sig = sign(&creds.seed, &canon).unwrap_or_default();
    client
        .post(url())
        .header("content-type", "application/json")
        .header("X-Daisy-License", &creds.license)
        .header("X-Daisy-Install", &creds.install_id)
        .header("X-Daisy-Task", &creds.task)
        .header("X-Daisy-Timestamp", ts)
        .header("X-Daisy-Nonce", nonce)
        .header("X-Daisy-Signature", sig)
        .body(body_bytes.to_vec())
}

/// Base64 of this install's Ed25519 **public** key, derived from the same
/// 32-byte seed used to sign gateway requests. This is exactly what
/// `/api/activate` stores as `install_pubkey`.
pub fn install_pubkey_b64(seed: &[u8]) -> Result<String> {
    use base64::{engine::general_purpose::STANDARD as B64, Engine};
    use ed25519_dalek::SigningKey;
    let arr: [u8; 32] = seed
        .try_into()
        .map_err(|_| SummarizeError::Decode("install seed must be 32 bytes".into()))?;
    Ok(B64.encode(SigningKey::from_bytes(&arr).verifying_key().to_bytes()))
}

/// (Re)register this device's install pubkey by POSTing `/api/activate` with
/// the same license key + install_id. The pubkey derives from the seed that
/// signs gateway requests.
///
/// Returns `Ok(true)` only when the server confirms the seat (2xx). Any other
/// outcome — `seat_limit` / `invalid_key` / `expired` / `revoked` / 5xx /
/// network error, or a seed that cannot yield a pubkey — returns `Ok(false)`
/// (or `Err`). Never POSTs without a real pubkey.
pub fn register_install(client: &reqwest::blocking::Client, creds: &GatewayCreds) -> Result<bool> {
    let pubkey = match install_pubkey_b64(&creds.seed) {
        Ok(p) => p,
        Err(_) => return Ok(false), // no real pubkey: no POST
    };
    let body = serde_json::json!({
        "key": creds.license,
        "install_id": creds.install_id,
        "install_pubkey": pubkey,
    });
    match client
        .post(ACTIVATE_URL)
        .header("content-type", "application/json")
        .json(&body)
        .send()
    {
        Ok(r) => Ok(r.status().is_success()),
        Err(_) => Ok(false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{engine::general_purpose::STANDARD as B64, Engine};
    use ed25519_dalek::{Signature, SigningKey, Verifier, VerifyingKey};

    #[test]
    fn sha256_hex_known_vector() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn canonical_string_shape() {
        let body = br#"{"messages":[]}"#;
        let canon = canonical_string("1780000000", "abc123", body);
        assert_eq!(
            canon,
            format!(
                "POST\n/api/gateway/v1/chat/completions\n1780000000\nabc123\n{}",
                sha256_hex(body)
            )
        );
    }

    #[test]
    fn signature_verifies_with_derived_pubkey() {
        let seed = [7u8; 32];
        let canon = canonical_string("1780000000", "n1", br#"{"x":1}"#);
        let sig_b64 = sign(&seed, &canon).unwrap();
        let sig = Signature::from_slice(&B64.decode(&sig_b64).unwrap()).unwrap();
        let vk: VerifyingKey = SigningKey::from_bytes(&seed).verifying_key();
        assert!(vk.verify(canon.as_bytes(), &sig).is_ok());
    }

    #[test]
    fn sign_rejects_wrong_length_seed() {
        assert!(sign(&[1u8; 8], "x").is_err());
    }

    #[test]
    fn install_pubkey_matches_signing_key_and_rejects_bad_seed() {
        let seed = [9u8; 32];
        let want = B64.encode(SigningKey::from_bytes(&seed).verifying_key().to_bytes());
        assert_eq!(install_pubkey_b64(&seed).unwrap(), want);
        // A non-32-byte seed yields no pubkey.
        assert!(install_pubkey_b64(&[1u8; 8]).is_err());
    }
}
