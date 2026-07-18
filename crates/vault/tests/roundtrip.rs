use vault::{decrypt, encrypt};

#[test]
fn roundtrip_recovers_plaintext() {
    let plaintext = br#"{"providers":{"groq":{"api_key":"gsk_test"}}}"#;
    let pass = "correct-horse-battery-staple-extended-22"; // 22 chars
    let env = encrypt(plaintext, pass).unwrap();
    let recovered = decrypt(&env, pass).unwrap();
    assert_eq!(&*recovered, plaintext);
}

#[test]
fn rejects_short_passphrase() {
    // "tooshort" fails both checks; the strength check runs first and
    // yields WeakPassphrase.
    let err = encrypt(b"x", "tooshort").unwrap_err();
    assert!(
        matches!(err, vault::VaultError::WeakPassphrase { .. }),
        "expected WeakPassphrase, got {err:?}"
    );
}

#[test]
fn rejects_weak_but_long_passphrase() {
    // Meets the length minimum but not the strength minimum.
    let pass = "aaaaaaaaaaaaaaaaaaaaaa"; // 22 chars, zxcvbn score 0
    let err = encrypt(b"x", pass).unwrap_err();
    assert!(
        matches!(err, vault::VaultError::WeakPassphrase { .. }),
        "expected WeakPassphrase, got {err:?}"
    );
}

#[test]
fn envelope_serializes_to_json_and_back() {
    let env = encrypt(b"hello", "correct-horse-battery-staple-extended-22").unwrap();
    let json = serde_json::to_string_pretty(&env).unwrap();
    let back: vault::Envelope = serde_json::from_str(&json).unwrap();
    assert_eq!(env, back);
}
