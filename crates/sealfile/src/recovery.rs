// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Recovery words: five words from the BIP-39 English list that open
//! `content.recovery.json`, a copy of the content secret wrapped in the
//! phones' `vault_recovery.json` shape.
//!
//! The words, lower-case and joined by single spaces (UTF-8), are stretched
//! with PBKDF2-HMAC-SHA256 (600,000 iterations, 16-byte random salt) to a
//! 32-byte key, which seals the secret with AES-256-GCM (12-byte random
//! nonce, no associated data). The file is compact sorted JSON.

use std::sync::OnceLock;

use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use zeroize::Zeroizing;

use crate::{gcm_open, gcm_seal, random_array, SealError};

pub const WORD_COUNT: usize = 5;
pub const ITERATIONS: u32 = 600_000;
const KDF: &str = "pbkdf2-sha256";
const FILE_VERSION: u32 = 1;

/// BIP-39 English (`data/bip39-english.txt`, MIT): 2048 words, each unique
/// in its first four letters.
const WORD_LIST: &str = include_str!("../data/bip39-english.txt");

pub fn word_list() -> &'static [&'static str] {
    static LIST: OnceLock<Vec<&'static str>> = OnceLock::new();
    LIST.get_or_init(|| WORD_LIST.lines().filter(|l| !l.is_empty()).collect())
}

/// Five random words; each is 11 bits, so all 2048 are equally likely.
pub fn generate_words() -> [String; WORD_COUNT] {
    let bytes = Zeroizing::new(random_array::<{ WORD_COUNT * 2 }>());
    let list = word_list();
    std::array::from_fn(|i| {
        let index = (usize::from(bytes[2 * i]) << 8 | usize::from(bytes[2 * i + 1])) & 0x7ff;
        list[index].to_string()
    })
}

/// The list word `token` is, or the one it is a prefix of when the prefix
/// has four or more letters.
fn complete(token: &str) -> Option<&'static str> {
    let list = word_list();
    if let Some(w) = list.iter().find(|w| **w == token) {
        return Some(w);
    }
    if token.chars().count() < 4 {
        return None;
    }
    let mut matches = list.iter().filter(|w| w.starts_with(token));
    let first = matches.next()?;
    matches.next().is_none().then_some(*first)
}

/// The five words typed text stands for, lower-case and joined by single
/// spaces, or `None` when it is not five list words. Case is ignored and
/// anything that is not a letter separates words.
pub fn normalize_entry(input: &str) -> Option<String> {
    let lower = input.to_lowercase();
    let tokens: Vec<&str> = lower.split(|c: char| !c.is_alphabetic()).filter(|t| !t.is_empty()).collect();
    if tokens.len() != WORD_COUNT {
        return None;
    }
    let words: Option<Vec<&str>> = tokens.iter().map(|t| complete(t)).collect();
    words.map(|w| w.join(" "))
}

/// PBKDF2-HMAC-SHA256 of `phrase` over `salt`: the 32-byte wrapping key.
pub fn wrapping_key(phrase: &str, salt: &[u8]) -> Zeroizing<[u8; 32]> {
    let mut out = Zeroizing::new([0u8; 32]);
    pbkdf2::pbkdf2_hmac::<Sha256>(phrase.as_bytes(), salt, ITERATIONS, &mut *out);
    out
}

/// `content.recovery.json`; fields in sorted order.
#[derive(Serialize, Deserialize)]
struct RecoveryFile {
    created_unix_seconds: i64,
    iterations: u32,
    kdf: String,
    nonce_b64: String,
    salt_b64: String,
    v: u32,
    wrapped_b64: String,
}

/// The bytes of `content.recovery.json`: `secret_bytes` wrapped under
/// `words` with a fresh salt and nonce.
pub fn wrap_with_words(words: &str, secret_bytes: &[u8; 32]) -> Vec<u8> {
    let created = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    wrap_with_params(words, secret_bytes, random_array(), random_array(), created)
}

/// [`wrap_with_words`] with the salt, nonce and time given.
pub fn wrap_with_params(words: &str, secret_bytes: &[u8; 32], salt: [u8; 16], nonce: [u8; 12], created_unix_seconds: i64) -> Vec<u8> {
    let key = wrapping_key(words, &salt);
    let wrapped = gcm_seal(&key, &nonce, &[], secret_bytes);
    serde_json::to_vec(&RecoveryFile {
        created_unix_seconds,
        iterations: ITERATIONS,
        kdf: KDF.to_string(),
        nonce_b64: B64.encode(nonce),
        salt_b64: B64.encode(salt),
        v: FILE_VERSION,
        wrapped_b64: B64.encode(wrapped),
    })
    .expect("recovery file serializes")
}

/// The secret in `file`, opened with typed `words` (normalized first). Any
/// wrong entry, an unreadable file included, is [`SealError::Auth`].
pub fn unwrap_with_words(words: &str, file: &[u8]) -> Result<[u8; 32], SealError> {
    let phrase = Zeroizing::new(normalize_entry(words).ok_or(SealError::Auth)?);
    let file: RecoveryFile = serde_json::from_slice(file).map_err(|_| SealError::Auth)?;
    if file.v != FILE_VERSION || file.kdf != KDF || file.iterations != ITERATIONS {
        return Err(SealError::Auth);
    }
    let salt = B64.decode(&file.salt_b64).map_err(|_| SealError::Auth)?;
    let nonce: [u8; 12] = B64
        .decode(&file.nonce_b64)
        .ok()
        .and_then(|n| n.try_into().ok())
        .ok_or(SealError::Auth)?;
    let wrapped = B64.decode(&file.wrapped_b64).map_err(|_| SealError::Auth)?;
    let plain = Zeroizing::new(gcm_open(&wrapping_key(&phrase, &salt), &nonce, &[], &wrapped)?);
    plain.as_slice().try_into().map_err(|_| SealError::Auth)
}
