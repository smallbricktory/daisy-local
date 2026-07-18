//! Vault — symmetric encryption of secrets using a passphrase.
//!
//! AES-256-GCM with a key derived via Argon2id. Decryption with a wrong
//! passphrase returns an error.

pub mod envelope;
pub mod error;
mod kdf;

pub use envelope::{Envelope, KdfParams};
pub use error::{Result, VaultError};

use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Nonce,
};
use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use rand::RngCore;
use zeroize::Zeroizing;

/// Minimum accepted passphrase length.
pub const MIN_PASSPHRASE_LEN: usize = 22;

pub fn encrypt(plaintext: &[u8], passphrase: &str) -> Result<Envelope> {
    let entropy = zxcvbn::zxcvbn(passphrase, &[]);
    if entropy.score() < zxcvbn::Score::Three {
        return Err(VaultError::WeakPassphrase {
            score: entropy.score() as u8,
            feedback: entropy.feedback().map(|f| {
                f.warning().map(|w| w.to_string()).unwrap_or_default()
            }).unwrap_or_default(),
        });
    }
    if passphrase.chars().count() < MIN_PASSPHRASE_LEN {
        return Err(VaultError::PassphraseTooShort(
            passphrase.chars().count(),
            MIN_PASSPHRASE_LEN,
        ));
    }
    let mut salt = [0u8; 16];
    let mut nonce_bytes = [0u8; 12];
    rand::rng().fill_bytes(&mut salt);
    rand::rng().fill_bytes(&mut nonce_bytes);

    let params = KdfParams::default();
    let key = kdf::derive_key(passphrase, &salt, params)?;
    let cipher = Aes256Gcm::new_from_slice(&*key)
        .map_err(|e| VaultError::Cipher(format!("key init: {e}")))?;
    let nonce = Nonce::from_slice(&nonce_bytes);
    let ciphertext = cipher
        .encrypt(nonce, plaintext)
        .map_err(|e| VaultError::Cipher(format!("encrypt: {e}")))?;

    Ok(Envelope {
        schema_version: Envelope::SCHEMA,
        kdf: "argon2id".to_string(),
        kdf_params: params,
        salt_b64: B64.encode(salt),
        nonce_b64: B64.encode(nonce_bytes),
        ciphertext_b64: B64.encode(&ciphertext),
    })
}

pub fn decrypt(envelope: &Envelope, passphrase: &str) -> Result<Zeroizing<Vec<u8>>> {
    if envelope.schema_version != Envelope::SCHEMA {
        return Err(VaultError::UnsupportedVersion(envelope.schema_version));
    }
    let salt = B64.decode(envelope.salt_b64.as_bytes())?;
    let nonce_bytes = B64.decode(envelope.nonce_b64.as_bytes())?;
    let ciphertext = B64.decode(envelope.ciphertext_b64.as_bytes())?;

    let key = kdf::derive_key(passphrase, &salt, envelope.kdf_params)?;
    let cipher = Aes256Gcm::new_from_slice(&*key)
        .map_err(|e| VaultError::Cipher(format!("key init: {e}")))?;
    let nonce = Nonce::from_slice(&nonce_bytes);
    let plaintext = cipher
        .decrypt(nonce, ciphertext.as_ref())
        .map_err(|_| VaultError::AuthenticationFailed)?;
    Ok(Zeroizing::new(plaintext))
}
