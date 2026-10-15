// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Schema-2 envelopes: a random data key seals the file, and that key is
//! wrapped once per recipient. A phone seals a file while locked and hands
//! it over unopened; this desktop unwraps with its X25519 key.

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::error::{Result, VaultError};

/// HKDF salt for every envelope-v2 key wrap.
const WRAP_SALT: &[u8] = b"daisy-envelope-v2";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvelopeV2 {
    pub schema_version: u32,
    pub cipher: String,
    pub nonce_b64: String,
    pub ciphertext_b64: String,
    pub recipients: Vec<Recipient>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Recipient {
    pub kind: String,
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ephemeral_pub_b64: Option<String>,
    pub nonce_b64: String,
    pub wrapped_b64: String,
}

impl EnvelopeV2 {
    pub const SCHEMA: u32 = 2;
}

/// True when the bytes are a schema-2 envelope, which is how a reader tells
/// a sealed file from one written in the clear.
pub fn is_v2(bytes: &[u8]) -> bool {
    serde_json::from_slice::<EnvelopeV2>(bytes).is_ok_and(|e| e.schema_version == EnvelopeV2::SCHEMA)
}

/// Opens a file sealed to this desktop: unwraps the data key with the
/// machine's X25519 secret, then opens the body.
pub fn open_v2(bytes: &[u8], x25519_secret: &[u8; 32], device_id: &str) -> Result<Zeroizing<Vec<u8>>> {
    // The version is read on its own: a schema-1 envelope has none of the
    // fields below, and its version is the useful thing to report.
    #[derive(Deserialize)]
    struct Version {
        schema_version: u32,
    }
    let version: Version = serde_json::from_slice(bytes)?;
    if version.schema_version != EnvelopeV2::SCHEMA {
        return Err(VaultError::UnsupportedVersion(version.schema_version));
    }
    let envelope: EnvelopeV2 = serde_json::from_slice(bytes)?;
    let recipient = envelope
        .recipients
        .iter()
        .find(|r| r.kind == "x25519" && r.id == device_id)
        .ok_or(VaultError::NoRecipient)?;
    let ephemeral = recipient.ephemeral_pub_b64.as_deref().ok_or(VaultError::NoRecipient)?;
    let ephemeral: [u8; 32] = B64
        .decode(ephemeral)?
        .try_into()
        .map_err(|_| VaultError::Cipher("ephemeral public key length".into()))?;

    let secret = x25519_dalek::StaticSecret::from(*x25519_secret);
    let recipient_pub = x25519_dalek::PublicKey::from(&secret).to_bytes();
    let shared = secret.diffie_hellman(&x25519_dalek::PublicKey::from(ephemeral));
    let mut wrap_key = Zeroizing::new([0u8; 32]);
    hkdf::Hkdf::<sha2::Sha256>::new(Some(WRAP_SALT), shared.as_bytes())
        .expand(&[b"x25519-wrap".as_slice(), &ephemeral, &recipient_pub].concat(), &mut *wrap_key)
        .map_err(|e| VaultError::Kdf(format!("wrap key: {e}")))?;

    let data_key = open_gcm(&recipient.nonce_b64, &recipient.wrapped_b64, &wrap_key)?;
    let data_key: [u8; 32] = data_key
        .as_slice()
        .try_into()
        .map_err(|_| VaultError::Cipher("data key length".into()))?;
    open_gcm(&envelope.nonce_b64, &envelope.ciphertext_b64, &data_key)
}

/// AES-256-GCM over base64 fields, with the tag on the end of the
/// ciphertext. Any failure is one answer: this key does not open this file.
fn open_gcm(nonce_b64: &str, boxed_b64: &str, key: &[u8; 32]) -> Result<Zeroizing<Vec<u8>>> {
    let nonce = B64.decode(nonce_b64)?;
    let boxed = B64.decode(boxed_b64)?;
    if nonce.len() != 12 {
        return Err(VaultError::Cipher("nonce length".into()));
    }
    let cipher = Aes256Gcm::new_from_slice(key).map_err(|e| VaultError::Cipher(format!("key init: {e}")))?;
    cipher
        .decrypt(Nonce::from_slice(&nonce), boxed.as_ref())
        .map(Zeroizing::new)
        .map_err(|_| VaultError::AuthenticationFailed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::general_purpose::STANDARD as B64;

    /// Seals like the phone does: a random data key, wrapped to the
    /// desktop's X25519 public key under an ephemeral one.
    fn sealed_to(recipient_pub: [u8; 32], recipient_id: &str, plaintext: &[u8]) -> Vec<u8> {
        use aes_gcm::aead::{Aead, KeyInit};
        let data_key = [7u8; 32];
        let cipher = aes_gcm::Aes256Gcm::new_from_slice(&data_key).unwrap();
        let nonce = [1u8; 12];
        let body = cipher.encrypt(aes_gcm::Nonce::from_slice(&nonce), plaintext).unwrap();

        let ephemeral = x25519_dalek::StaticSecret::from([9u8; 32]);
        let ephemeral_pub = x25519_dalek::PublicKey::from(&ephemeral);
        let shared = ephemeral.diffie_hellman(&x25519_dalek::PublicKey::from(recipient_pub));
        let mut wrap_key = [0u8; 32];
        hkdf::Hkdf::<sha2::Sha256>::new(Some(b"daisy-envelope-v2"), shared.as_bytes())
            .expand(&[b"x25519-wrap".as_slice(), ephemeral_pub.as_bytes(), &recipient_pub].concat(), &mut wrap_key)
            .unwrap();
        let wrap = aes_gcm::Aes256Gcm::new_from_slice(&wrap_key).unwrap();
        let wrap_nonce = [2u8; 12];
        let wrapped = wrap.encrypt(aes_gcm::Nonce::from_slice(&wrap_nonce), data_key.as_slice()).unwrap();

        serde_json::to_vec(&serde_json::json!({
            "schema_version": 2,
            "cipher": "aes-256-gcm",
            "nonce_b64": B64.encode(nonce),
            "ciphertext_b64": B64.encode(&body),
            "recipients": [
                { "kind": "device_key", "id": "phone-1", "nonce_b64": B64.encode([3u8; 12]), "wrapped_b64": B64.encode([4u8; 48]) },
                {
                    "kind": "x25519",
                    "id": recipient_id,
                    "ephemeral_pub_b64": B64.encode(ephemeral_pub.as_bytes()),
                    "nonce_b64": B64.encode(wrap_nonce),
                    "wrapped_b64": B64.encode(&wrapped),
                },
            ],
        }))
        .unwrap()
    }

    fn desktop() -> ([u8; 32], [u8; 32]) {
        let secret = [5u8; 32];
        let public = x25519_dalek::PublicKey::from(&x25519_dalek::StaticSecret::from(secret)).to_bytes();
        (secret, public)
    }

    #[test]
    fn the_desktop_opens_a_file_sealed_to_its_key() {
        let (secret, public) = desktop();
        let sealed = sealed_to(public, "mac-1", b"the manifest");
        assert_eq!(&*open_v2(&sealed, &secret, "mac-1").unwrap(), b"the manifest");
    }

    #[test]
    fn an_envelope_addressed_to_another_desktop_is_not_ours_to_open() {
        let (secret, public) = desktop();
        let sealed = sealed_to(public, "mac-2", b"the manifest");
        assert!(matches!(open_v2(&sealed, &secret, "mac-1"), Err(VaultError::NoRecipient)));
    }

    #[test]
    fn another_desktops_key_does_not_open_it() {
        let (_, public) = desktop();
        let sealed = sealed_to(public, "mac-1", b"the manifest");
        assert!(matches!(open_v2(&sealed, &[6u8; 32], "mac-1"), Err(VaultError::AuthenticationFailed)));
    }

    #[test]
    fn a_body_edited_after_sealing_is_refused() {
        let (secret, public) = desktop();
        let sealed = sealed_to(public, "mac-1", b"the manifest");
        let mut v: serde_json::Value = serde_json::from_slice(&sealed).unwrap();
        v["ciphertext_b64"] = serde_json::json!(B64.encode([8u8; 40]));
        let tampered = serde_json::to_vec(&v).unwrap();
        assert!(matches!(open_v2(&tampered, &secret, "mac-1"), Err(VaultError::AuthenticationFailed)));
    }

    #[test]
    fn a_schema_one_envelope_is_not_a_schema_two_envelope() {
        let (secret, _) = desktop();
        let v1 = serde_json::to_vec(&serde_json::json!({
            "schema_version": 1, "kdf": "argon2id",
            "kdf_params": { "memory_kib": 65536, "iterations": 3, "parallelism": 4 },
            "salt_b64": "", "nonce_b64": "", "ciphertext_b64": "",
        }))
        .unwrap();
        assert!(matches!(open_v2(&v1, &secret, "mac-1"), Err(VaultError::UnsupportedVersion(1))));
    }

    #[test]
    fn a_sealed_file_is_told_apart_from_a_plain_one() {
        let (_, public) = desktop();
        assert!(is_v2(&sealed_to(public, "mac-1", b"x")));
        assert!(!is_v2(b"{\"chunks\": []}"));
        assert!(!is_v2(b"not json"));
    }
}
