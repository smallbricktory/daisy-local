// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Section 4 of the sync protocol: the string every request after `/pair`
//! signs, the nonce that guards a replay, and the signature the desktop puts
//! on its reply.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::AppError;
use crate::state::AppState;

/// `sha256:<lowercase hex>` of the body bytes, as every hash is written.
pub fn body_hash(body: &[u8]) -> String {
    let digest = Sha256::digest(body);
    let mut out = String::from("sha256:");
    for b in digest {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// What a phone signs: method, path, its device id, the nonce and the body
/// hash, one per line.
pub fn signing_string(method: &str, path: &str, device_id: &str, nonce_b64: &str, body: &[u8]) -> String {
    format!("{method}\n{path}\n{device_id}\n{nonce_b64}\n{}", body_hash(body))
}

/// What the desktop signs on the way back: the request's nonce, its own
/// status and path, and the reply body.
pub fn reply_signing_string(status: u16, path: &str, nonce_b64: &str, body: &[u8]) -> String {
    format!("{status}\n{path}\n{nonce_b64}\n{}", body_hash(body))
}


/// True when the phone signed exactly this string. `presented` is the
/// signing string rebuilt from the request as it arrived, so a body edited in
/// flight no longer matches what the phone signed.
pub fn verify_request(phone_ed25519_pub_b64: &str, signature_b64: &str, presented: &str) -> bool {
    let verify = || {
        let key: [u8; 32] = B64.decode(phone_ed25519_pub_b64).ok()?.try_into().ok()?;
        let key = ed25519_dalek::VerifyingKey::from_bytes(&key).ok()?;
        let sig: [u8; 64] = B64.decode(signature_b64).ok()?.try_into().ok()?;
        key.verify_strict(presented.as_bytes(), &ed25519_dalek::Signature::from_bytes(&sig)).ok()
    };
    verify().is_some()
}

/// Every nonce this desktop has accepted, per device. The protocol has no
/// timestamp and iOS sends background uploads out of order, so a set is the
/// only replay guard available; it lives beside the identity, per machine.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct NonceSet {
    #[serde(default)]
    seen: BTreeMap<String, BTreeSet<String>>,
}

impl NonceSet {
    /// True when the nonce is new for that device, which also records it.
    pub fn accept(&mut self, device_id: &str, nonce_b64: &str) -> bool {
        self.seen.entry(device_id.to_string()).or_default().insert(nonce_b64.to_string())
    }

    /// A missing or unreadable file starts an empty set: the routes are still
    /// behind mutual TLS and a signature, and refusing every request because
    /// one file is corrupt would strand the phone.
    pub fn load(path: &Path) -> Self {
        match syncsafe::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
                log::warn!("sync: nonce file unreadable ({e}); starting empty");
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self, path: &Path) -> Result<(), AppError> {
        syncsafe::write_atomic(path, &serde_json::to_vec(self)?)?;
        Ok(())
    }

    /// Drops the sets of devices that are no longer paired.
    pub fn retain_devices(&mut self, paired: &[String]) {
        self.seen.retain(|id, _| paired.iter().any(|p| p == id));
    }
}

/// Where this machine keeps its nonce sets: beside the identity, named for
/// the machine, because a nonce another desktop accepted says nothing here.
pub fn nonce_path(app: &AppState) -> std::path::PathBuf {
    let name = crate::hardware::sanitized_machine_name();
    app.profile.root().join(format!("sync.{name}.nonces.json"))
}

/// Held across every read-modify-write of the nonce file. A phone opens
/// several connections at once and each is served on its own task, so
/// unserialized writers overwrite each other's records and the nonces that
/// go missing can be replayed.
static NONCE_FILE: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Records the nonce, refusing one this device has used before. The set is
/// read and written per request: a phone sends a handful per wake, and a
/// nonce that outlives a crash is worth more than the write it costs.
pub fn accept_nonce(app: &AppState, device: &str, nonce: &str) -> bool {
    let path = nonce_path(app);
    let _guard = NONCE_FILE.lock().unwrap_or_else(|e| e.into_inner());
    let mut set = NonceSet::load(&path);
    if !set.accept(device, nonce) {
        return false;
    }
    if let Err(e) = set.save(&path) {
        log::warn!("sync: nonce not recorded: {e}");
        return false;
    }
    true
}

/// Forgets every device that is no longer in `paired`.
pub fn prune_nonces(app: &AppState, paired: &[String]) {
    let path = nonce_path(app);
    let _guard = NONCE_FILE.lock().unwrap_or_else(|e| e.into_inner());
    let mut set = NonceSet::load(&path);
    set.retain_devices(paired);
    if let Err(e) = set.save(&path) {
        log::warn!("sync: nonce file not pruned: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A phone key and the signature it puts on one request.
    fn phone() -> (ed25519_dalek::SigningKey, String) {
        let key = ed25519_dalek::SigningKey::from_bytes(&[5u8; 32]);
        let pub_b64 = B64.encode(key.verifying_key().to_bytes());
        (key, pub_b64)
    }

    fn sign(key: &ed25519_dalek::SigningKey, s: &str) -> String {
        use ed25519_dalek::Signer;
        B64.encode(key.sign(s.as_bytes()).to_bytes())
    }

    #[test]
    fn a_request_signed_by_the_paired_phone_verifies() {
        let (key, pub_b64) = phone();
        let s = signing_string("PUT", "/file/sha256:aa", "dev-1", "bm9uY2U=", b"body");
        assert!(verify_request(&pub_b64, &sign(&key, &s), &s));
    }

    #[test]
    fn a_body_changed_after_signing_is_refused() {
        let (key, pub_b64) = phone();
        let signed = signing_string("PUT", "/file/sha256:aa", "dev-1", "bm9uY2U=", b"body");
        let presented = signing_string("PUT", "/file/sha256:aa", "dev-1", "bm9uY2U=", b"other");
        assert!(!verify_request(&pub_b64, &sign(&key, &signed), &presented));
    }

    #[test]
    fn another_phones_signature_is_refused() {
        let (_, pub_b64) = phone();
        let other = ed25519_dalek::SigningKey::from_bytes(&[6u8; 32]);
        let s = signing_string("POST", "/hello", "dev-1", "bm9uY2U=", b"{}");
        assert!(!verify_request(&pub_b64, &sign(&other, &s), &s));
    }

    #[test]
    fn a_signature_that_is_not_base64_is_refused() {
        let (_, pub_b64) = phone();
        let s = signing_string("POST", "/hello", "dev-1", "bm9uY2U=", b"{}");
        assert!(!verify_request(&pub_b64, "not base64!", &s));
    }

    #[test]
    fn a_nonce_is_accepted_once_per_device() {
        let mut n = NonceSet::default();
        assert!(n.accept("dev-1", "bm9uY2Ux"));
        assert!(!n.accept("dev-1", "bm9uY2Ux"), "a repeat is a replay");
        assert!(n.accept("dev-2", "bm9uY2Ux"), "another device has its own set");
        assert!(n.accept("dev-1", "bm9uY2Uy"));
    }

    #[test]
    fn nonces_survive_a_round_trip_through_their_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nonces.json");
        let mut n = NonceSet::load(&path);
        assert!(n.accept("dev-1", "bm9uY2Ux"));
        n.save(&path).unwrap();
        let mut again = NonceSet::load(&path);
        assert!(!again.accept("dev-1", "bm9uY2Ux"), "a restart does not forget");
        assert!(again.accept("dev-1", "bm9uY2Uy"));
    }

    #[test]
    fn the_signing_string_is_the_five_lines_the_phone_signs() {
        assert_eq!(
            signing_string("POST", "/hello", "abc", "bm9uY2U=", b"{}"),
            format!("POST\n/hello\nabc\nbm9uY2U=\n{}", body_hash(b"{}"))
        );
    }

    #[test]
    fn an_empty_body_hashes_as_the_empty_string() {
        assert_eq!(body_hash(b""), "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
    }

    #[test]
    fn a_reply_is_signed_over_status_path_nonce_and_body() {
        assert_eq!(
            reply_signing_string(200, "/hello", "bm9uY2U=", b"{}"),
            format!("200\n/hello\nbm9uY2U=\n{}", body_hash(b"{}"))
        );
    }
}
