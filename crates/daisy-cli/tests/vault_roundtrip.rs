//! `daisy set-key` must round-trip every vault field it does not model:
//! full-shape vault in, one provider key changed, everything else byte-value
//! identical and no phantom keys added.

use std::io::Write as _;
use std::process::{Command, Stdio};

const PASSPHRASE: &str = "quartz-heron-migrates-9-baskets";

/// Full-shape decrypted payload matching the app's `DecryptedKeys` (v4),
/// entirely fictional data.
fn full_shape_payload() -> serde_json::Value {
    serde_json::json!({
        "schema_version": 4,
        "providers": {
            "openai": {
                "api_key": "sk-fictional-openai-000",
                "model": "gpt-4o-mini"
            }
        },
        "integrations": [{
            "id": "b2f9c1d4-0000-4000-8000-000000000001",
            "kind": "webhook",
            "name": "Fictional CRM",
            "url": "https://example.invalid/hook",
            "enabled": true
        }],
        "voiceprints": [{
            "id": "b2f9c1d4-0000-4000-8000-000000000002",
            "display_name": "Pat Example",
            "email": "pat@example.invalid",
            "vector": [0.25, -0.5, 0.125],
            "vectors": [[0.1, 0.2, 0.3], [0.4, 0.5, 0.6]]
        }],
        "calendar_subscriptions": [{
            "id": "b2f9c1d4-0000-4000-8000-000000000003",
            "name": "Work",
            "url": "https://example.invalid/cal.ics?token=fictional",
            "enabled": true,
            "color_hex": "#3366ff",
            "tag_id": null
        }],
        "install_privkey": "ZmljdGlvbmFsLXNlZWQtMzItYnl0ZXMtLS0tLS0tLQ==",
        "mcp_token": "mcp-fictional-token-0001"
    })
}

#[test]
fn set_key_preserves_all_unmodelled_vault_fields() {
    let dir = tempfile::tempdir().expect("tempdir");
    let vault_path = dir.path().join("keys.vault.json");
    let key_path = dir.path().join("keyfile");

    let original = full_shape_payload();
    let plaintext = serde_json::to_vec(&original).expect("serialize payload");
    let envelope = vault::encrypt(&plaintext, PASSPHRASE).expect("encrypt");
    std::fs::write(&vault_path, serde_json::to_vec_pretty(&envelope).expect("envelope json"))
        .expect("write vault");
    std::fs::write(&key_path, "sk-fictional-anthropic-999\n").expect("write keyfile");

    let mut child = Command::new(env!("CARGO_BIN_EXE_daisy"))
        .args([
            "set-key",
            "--provider",
            "anthropic",
            "--key-file",
            key_path.to_str().unwrap(),
            "--vault",
            vault_path.to_str().unwrap(),
            "--passphrase-stdin",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn daisy set-key");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(format!("{PASSPHRASE}\n").as_bytes())
        .expect("write passphrase");
    let out = child.wait_with_output().expect("wait");
    assert!(
        out.status.success(),
        "set-key failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let bytes = std::fs::read(&vault_path).expect("read vault");
    let envelope: vault::Envelope = serde_json::from_slice(&bytes).expect("parse envelope");
    let plaintext = vault::decrypt(&envelope, PASSPHRASE).expect("decrypt");
    let after: serde_json::Value = serde_json::from_slice(&plaintext).expect("parse payload");

    // Every original top-level key survives; the only difference anywhere is
    // the added anthropic provider entry.
    let orig_map = original.as_object().unwrap();
    let after_map = after.as_object().unwrap();
    for (key, value) in orig_map {
        if key == "providers" {
            continue;
        }
        assert_eq!(
            after_map.get(key),
            Some(value),
            "vault field {key:?} did not round-trip intact"
        );
    }
    for key in after_map.keys() {
        assert!(
            orig_map.contains_key(key.as_str()),
            "phantom top-level key {key:?} injected"
        );
    }

    let providers = after_map["providers"].as_object().expect("providers object");
    assert_eq!(
        providers.get("openai"),
        original["providers"].get("openai"),
        "untouched provider entry changed"
    );
    assert_eq!(
        providers["anthropic"]["api_key"],
        serde_json::json!("sk-fictional-anthropic-999")
    );
}
