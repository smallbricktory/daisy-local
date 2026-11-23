// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! `<session>/key.json`: the session key wrapped to the content public key
//! with envelope v2's X25519 construction. HKDF-SHA256 over the X25519
//! shared secret, salt `daisy-envelope-v2`, info
//! `label || ephemeral_pub || recipient_pub`, then AES-256-GCM with a
//! random 12-byte nonce and no associated data.
//!
//! The same construction seals files outside a session ([`seal_to`]), with
//! the caller's label in the HKDF info.

use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::{gcm_open, gcm_seal, random_array, ContentPublic, ContentSecret, SealError, SessionKey};

/// HKDF salt shared with envelope v2.
const WRAP_SALT: &[u8] = b"daisy-envelope-v2";
/// HKDF info label for a session key wrap.
const SESSION_KEY_LABEL: &[u8] = b"x25519-session-key";
/// HKDF salt for a session key derived from the content secret.
const DERIVE_SALT: &[u8] = b"daisy-session-key-v1";
/// Leading bytes of a file sealed with [`seal_to`].
pub const SEALED_TO_MAGIC: &[u8; 4] = b"DSE1";
const SEALED_TO_VERSION: u8 = 1;
const SEALED_TO_HEADER: usize = 4 + 1 + 32 + 32 + 12;

/// One X25519 seal: the ephemeral public key, the nonce, and the ciphertext
/// with its tag.
pub struct X25519Sealed {
    pub ephemeral_pub: [u8; 32],
    pub nonce: [u8; 12],
    pub ciphertext: Vec<u8>,
}

/// Seals `plain` to `recipient` under a fresh ephemeral key.
pub fn x25519_seal(recipient: &ContentPublic, label: &[u8], plain: &[u8]) -> X25519Sealed {
    let ephemeral = x25519_dalek::StaticSecret::from(*Zeroizing::new(random_array::<32>()));
    let ephemeral_pub = x25519_dalek::PublicKey::from(&ephemeral).to_bytes();
    let shared = ephemeral.diffie_hellman(&x25519_dalek::PublicKey::from(recipient.0));
    let key = wrap_key(shared.as_bytes(), label, &ephemeral_pub, &recipient.0);
    let nonce = random_array::<12>();
    let ciphertext = gcm_seal(&key, &nonce, &[], plain);
    X25519Sealed { ephemeral_pub, nonce, ciphertext }
}

fn x25519_open(
    secret: &ContentSecret,
    label: &[u8],
    ephemeral_pub: &[u8; 32],
    nonce: &[u8; 12],
    ciphertext: &[u8],
) -> Result<Zeroizing<Vec<u8>>, SealError> {
    let shared = secret.static_secret().diffie_hellman(&x25519_dalek::PublicKey::from(*ephemeral_pub));
    if !shared.was_contributory() {
        return Err(SealError::Auth);
    }
    let key = wrap_key(shared.as_bytes(), label, ephemeral_pub, &secret.public().0);
    gcm_open(&key, nonce, &[], ciphertext).map(Zeroizing::new)
}

fn wrap_key(shared: &[u8; 32], label: &[u8], ephemeral_pub: &[u8; 32], recipient_pub: &[u8; 32]) -> Zeroizing<[u8; 32]> {
    let mut key = Zeroizing::new([0u8; 32]);
    hkdf::Hkdf::<sha2::Sha256>::new(Some(WRAP_SALT), shared)
        .expand(&[label, ephemeral_pub, recipient_pub].concat(), &mut *key)
        .expect("32 bytes is a valid HKDF-SHA256 output length");
    key
}

/// `key.json`; fields in sorted order, written as compact JSON.
#[derive(Serialize, Deserialize)]
struct KeyFile {
    content_pub_b64: String,
    ephemeral_pub_b64: String,
    nonce_b64: String,
    v: u32,
    wrapped_b64: String,
}

const KEY_FILE_VERSION: u32 = 1;

/// The bytes of `key.json` for `key`, wrapped to `content`.
pub fn wrap_session_key(content: &ContentPublic, key: &SessionKey) -> Vec<u8> {
    let sealed = x25519_seal(content, SESSION_KEY_LABEL, key.as_bytes());
    serde_json::to_vec(&KeyFile {
        content_pub_b64: B64.encode(content.0),
        ephemeral_pub_b64: B64.encode(sealed.ephemeral_pub),
        nonce_b64: B64.encode(sealed.nonce),
        v: KEY_FILE_VERSION,
        wrapped_b64: B64.encode(&sealed.ciphertext),
    })
    .expect("key.json serializes")
}

fn parse_key_file(key_json: &[u8]) -> Result<KeyFile, SealError> {
    let file: KeyFile = serde_json::from_slice(key_json).map_err(|e| SealError::Format(format!("key.json: {e}")))?;
    if file.v != KEY_FILE_VERSION {
        return Err(SealError::Format(format!("key.json version {}", file.v)));
    }
    Ok(file)
}

/// The session key in `key_json`. [`SealError::OtherContentKey`] when it is
/// wrapped to a content key other than `secret`'s.
pub fn unwrap_session_key(secret: &ContentSecret, key_json: &[u8]) -> Result<SessionKey, SealError> {
    let file = parse_key_file(key_json)?;
    let content_pub: [u8; 32] = decode_array(&file.content_pub_b64, "content_pub_b64")?;
    if content_pub != secret.public().0 {
        return Err(SealError::OtherContentKey);
    }
    let ephemeral_pub: [u8; 32] = decode_array(&file.ephemeral_pub_b64, "ephemeral_pub_b64")?;
    let nonce: [u8; 12] = decode_array(&file.nonce_b64, "nonce_b64")?;
    let wrapped = B64
        .decode(&file.wrapped_b64)
        .map_err(|e| SealError::Format(format!("wrapped_b64: {e}")))?;
    let plain = x25519_open(secret, SESSION_KEY_LABEL, &ephemeral_pub, &nonce, &wrapped)?;
    let bytes: [u8; 32] = plain.as_slice().try_into().map_err(|_| SealError::Format("session key length".into()))?;
    Ok(SessionKey::from_bytes(bytes))
}

/// The content public key `key_json` is wrapped to.
pub fn wrapped_to(key_json: &[u8]) -> Result<ContentPublic, SealError> {
    let file = parse_key_file(key_json)?;
    decode_array(&file.content_pub_b64, "content_pub_b64").map(ContentPublic)
}

/// The content public key in a `content.pub` file's text (base64).
pub fn parse_content_pub(text: &str) -> Option<ContentPublic> {
    B64.decode(text.trim()).ok().and_then(|b| <[u8; 32]>::try_from(b).ok()).map(ContentPublic)
}

fn decode_array<const N: usize>(b64: &str, field: &str) -> Result<[u8; N], SealError> {
    B64.decode(b64)
        .ok()
        .and_then(|v| v.try_into().ok())
        .ok_or_else(|| SealError::Format(format!("{field}: not {N} bytes")))
}

/// The session key for `session_id` derived from the content secret:
/// HKDF-SHA256, salt `daisy-session-key-v1`, info = the session id.
pub fn derive_session_key(secret: &ContentSecret, session_id: &str) -> SessionKey {
    let ikm = secret.to_bytes();
    let mut out = Zeroizing::new([0u8; 32]);
    hkdf::Hkdf::<sha2::Sha256>::new(Some(DERIVE_SALT), &*ikm)
        .expand(session_id.as_bytes(), &mut *out)
        .expect("32 bytes is a valid HKDF-SHA256 output length");
    SessionKey::from_bytes(*out)
}

/// Seals a file outside a session to the content public key:
/// `DSE1 | version | content_pub | ephemeral_pub | nonce | ciphertext+tag`.
/// `label` goes into the HKDF info, so it must match on open.
pub fn seal_to(content: &ContentPublic, label: &str, plain: &[u8]) -> Vec<u8> {
    let sealed = x25519_seal(content, label.as_bytes(), plain);
    let mut out = Vec::with_capacity(SEALED_TO_HEADER + sealed.ciphertext.len());
    out.extend_from_slice(SEALED_TO_MAGIC);
    out.push(SEALED_TO_VERSION);
    out.extend_from_slice(&content.0);
    out.extend_from_slice(&sealed.ephemeral_pub);
    out.extend_from_slice(&sealed.nonce);
    out.extend_from_slice(&sealed.ciphertext);
    out
}

/// True when the bytes start like a [`seal_to`] file.
pub fn is_sealed_to(prefix: &[u8]) -> bool {
    prefix.starts_with(SEALED_TO_MAGIC)
}

/// Opens a [`seal_to`] file. [`SealError::OtherContentKey`] when it was
/// sealed to another content key; [`SealError::Auth`] for a wrong label or
/// changed bytes.
pub fn open_with(secret: &ContentSecret, label: &str, sealed: &[u8]) -> Result<Vec<u8>, SealError> {
    if sealed.len() < SEALED_TO_HEADER + 16 || !is_sealed_to(sealed) {
        return Err(SealError::Format("not a sealed file".into()));
    }
    if sealed[4] != SEALED_TO_VERSION {
        return Err(SealError::Format(format!("sealed file version {}", sealed[4])));
    }
    let content_pub: [u8; 32] = sealed[5..37].try_into().expect("32-byte slice");
    if content_pub != secret.public().0 {
        return Err(SealError::OtherContentKey);
    }
    let ephemeral_pub: [u8; 32] = sealed[37..69].try_into().expect("32-byte slice");
    let nonce: [u8; 12] = sealed[69..81].try_into().expect("12-byte slice");
    x25519_open(secret, label.as_bytes(), &ephemeral_pub, &nonce, &sealed[SEALED_TO_HEADER..]).map(|p| p.to_vec())
}
