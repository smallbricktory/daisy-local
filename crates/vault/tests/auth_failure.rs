use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use vault::{decrypt, encrypt, VaultError};

#[test]
fn wrong_passphrase_fails_cleanly() {
    let env = encrypt(b"secret", "this-is-22-chars-long!").unwrap();
    let err = decrypt(&env, "this-is-22-chars-WRONG").unwrap_err();
    assert!(matches!(err, VaultError::AuthenticationFailed));
}

#[test]
fn corrupted_ciphertext_fails_authentication() {
    let mut env = encrypt(b"secret", "this-is-22-chars-long!").unwrap();
    let mut bytes = B64.decode(env.ciphertext_b64.as_bytes()).unwrap();
    bytes[0] ^= 0x01;
    env.ciphertext_b64 = B64.encode(&bytes);
    let err = decrypt(&env, "this-is-22-chars-long!").unwrap_err();
    assert!(matches!(err, VaultError::AuthenticationFailed));
}
