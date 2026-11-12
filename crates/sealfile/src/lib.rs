// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Session files sealed at rest: AES-256-GCM under a per-session key, with
//! the file's path relative to the session folder as associated data.
//!
//! - [`fs`]: whole-file reads and writes that seal session files.
//! - [`stream`]: segmented files (audio, video, attachments) and files
//!   written whole (JSON, Markdown).
//! - [`records`]: append-only JSONL, one sealed record per line.
//! - [`keyfile`]: `<session>/key.json`, the session key wrapped to the
//!   profile's content public key.
//! - [`wav_view`]: a WAV reader's view of a sealed PCM stream.
//! - [`keyset`]: the profile's content key pairs, named by fingerprint.
//! - [`keyring`]: the process's content and session keys.
//! - [`recovery`]: the recovery words that reopen the content key.

use x25519_dalek::StaticSecret;
use zeroize::Zeroizing;

pub mod fs;
pub mod keyfile;
pub mod keyring;
pub mod keyset;
pub mod records;
pub mod recovery;
pub mod stream;
pub mod wav_view;

/// A session's 32-byte AES-256-GCM key. Zeroized on drop.
#[derive(Clone)]
pub struct SessionKey(Zeroizing<[u8; 32]>);

impl SessionKey {
    pub fn random() -> Self {
        let mut bytes = Zeroizing::new([0u8; 32]);
        rand::RngCore::fill_bytes(&mut rand::rng(), &mut *bytes);
        Self(bytes)
    }

    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(Zeroizing::new(bytes))
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl PartialEq for SessionKey {
    fn eq(&self, other: &Self) -> bool {
        // Constant-time comparison.
        self.0.iter().zip(other.0.iter()).fold(0u8, |acc, (a, b)| acc | (a ^ b)) == 0
    }
}

impl Eq for SessionKey {}

impl std::fmt::Debug for SessionKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SessionKey(..)")
    }
}

/// A content public key of the profile ([`keyset`]). Writers wrap session
/// keys to it without the vault.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContentPublic(pub [u8; 32]);

/// The private half of the content key pair. Zeroized on drop.
pub struct ContentSecret(StaticSecret);

impl ContentSecret {
    pub fn generate() -> (ContentSecret, ContentPublic) {
        let mut bytes = Zeroizing::new([0u8; 32]);
        rand::RngCore::fill_bytes(&mut rand::rng(), &mut *bytes);
        let secret = ContentSecret::from_bytes(*bytes);
        let public = secret.public();
        (secret, public)
    }

    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(StaticSecret::from(bytes))
    }

    pub fn to_bytes(&self) -> Zeroizing<[u8; 32]> {
        Zeroizing::new(self.0.to_bytes())
    }

    pub fn public(&self) -> ContentPublic {
        ContentPublic(x25519_dalek::PublicKey::from(&self.0).to_bytes())
    }

    pub(crate) fn static_secret(&self) -> &StaticSecret {
        &self.0
    }
}

impl std::fmt::Debug for ContentSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ContentSecret(..)")
    }
}

/// Which sealed format a file is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// `DSS1`: a segmented stream, including files written whole.
    Stream,
    /// `DSR1`: append-only records, one per line.
    Records,
}

/// The sealed format the file's first bytes announce, or `None` for a
/// plaintext file.
pub fn is_sealed(prefix: &[u8]) -> Option<Format> {
    if prefix.starts_with(stream::MAGIC) {
        Some(Format::Stream)
    } else if prefix.starts_with(records::MAGIC) {
        Some(Format::Records)
    } else {
        None
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SealError {
    /// The key does not open the bytes, or they were changed or moved.
    #[error("sealed data did not authenticate")]
    Auth,
    /// The content key is not open in this process.
    #[error("the content key is locked")]
    Locked,
    /// The file is wrapped to a content key whose secret is not held.
    #[error("sealed to another content key")]
    OtherContentKey,
    /// The profile has no content public key to wrap a session key to.
    #[error("this profile has no content key")]
    NoContentKey,
    /// The bytes are not in the expected sealed format.
    #[error("not a sealed file: {0}")]
    Format(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// AES-256-GCM seal with a 12-byte nonce; the tag follows the ciphertext.
pub(crate) fn gcm_seal(key: &[u8; 32], nonce: &[u8; 12], aad: &[u8], plain: &[u8]) -> Vec<u8> {
    use aes_gcm::aead::{Aead, KeyInit, Payload};
    aes_gcm::Aes256Gcm::new(key.into())
        .encrypt(aes_gcm::Nonce::from_slice(nonce), Payload { msg: plain, aad })
        .expect("AES-GCM encryption of an in-memory buffer")
}

/// AES-256-GCM open; any failure is [`SealError::Auth`].
pub(crate) fn gcm_open(key: &[u8; 32], nonce: &[u8; 12], aad: &[u8], boxed: &[u8]) -> Result<Vec<u8>, SealError> {
    use aes_gcm::aead::{Aead, KeyInit, Payload};
    aes_gcm::Aes256Gcm::new(key.into())
        .decrypt(aes_gcm::Nonce::from_slice(nonce), Payload { msg: boxed, aad })
        .map_err(|_| SealError::Auth)
}

pub(crate) fn random_array<const N: usize>() -> [u8; N] {
    let mut out = [0u8; N];
    rand::RngCore::fill_bytes(&mut rand::rng(), &mut out);
    out
}
