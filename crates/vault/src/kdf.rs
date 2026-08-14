//! Argon2id wrapper. Derives a 32-byte key from a passphrase + salt.

use crate::envelope::KdfParams;
use crate::error::{Result, VaultError};
use argon2::{Algorithm, Argon2, Params, Version};
use zeroize::Zeroizing;

pub fn derive_key(passphrase: &str, salt: &[u8], params: KdfParams) -> Result<Zeroizing<[u8; 32]>> {
    let p = Params::new(params.memory_kib, params.iterations, params.parallelism, Some(32))
        .map_err(|e| VaultError::Kdf(format!("invalid params: {e}")))?;
    let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, p);
    let mut key = Zeroizing::new([0u8; 32]);
    argon
        .hash_password_into(passphrase.as_bytes(), salt, &mut *key)
        .map_err(|e| VaultError::Kdf(format!("hash_password_into: {e}")))?;
    Ok(key)
}
