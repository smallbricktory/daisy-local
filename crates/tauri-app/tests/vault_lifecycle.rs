// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
use tauri_app_core::commands::lifecycle::{
    init_vault_impl, list_providers_impl, lock_vault_impl, reset_vault_impl, set_provider_impl,
    unlock_vault_impl, vault_status_impl,
};
use tauri_app_core::commands::voiceprints::list_voiceprints_impl;
use tauri_app_core::profile::ProfileDir;
use tauri_app_core::state::{AppState, ProviderConfig, ProviderId, VaultState, Voiceprint};
use tempfile::TempDir;

const PASS: &str = "correct-horse-battery-staple-extended-22";

fn fresh(td: &TempDir) -> (AppState, VaultState) {
    let p = ProfileDir::at(td.path().join("daisy")).unwrap();
    (AppState::new(p), VaultState::new())
}

#[test]
fn full_lifecycle_init_unlock_set_lock_unlock() {
    let td = TempDir::new().unwrap();
    let (app, vs) = fresh(&td);

    // Initially: no vault, locked.
    let s = vault_status_impl(&app, &vs).unwrap();
    assert!(!s.vault_exists);
    assert!(!s.unlocked);

    // Init creates vault and leaves us unlocked.
    init_vault_impl(&app, &vs, PASS).unwrap();
    let s = vault_status_impl(&app, &vs).unwrap();
    assert!(s.vault_exists);
    assert!(s.unlocked);

    // Set a provider.
    set_provider_impl(
        &app,
        &vs,
        ProviderId::Groq,
        ProviderConfig {
            api_key: Some("gsk_test".into()),
            model: None,
            base_url: None,
            context_tokens: None,
        },
    )
    .unwrap();
    // list_providers_impl adds a zero-config provider entry; the real provider is found by name.
    let entries = list_providers_impl(&vs).unwrap();
    let groq = entries
        .iter()
        .find(|e| e.name == ProviderId::Groq)
        .expect("Groq present");
    assert!(groq.has_key);

    // Lock and verify provider list refuses.
    lock_vault_impl(&app, &vs).unwrap();
    assert!(!vs.is_unlocked());
    assert!(list_providers_impl(&vs).is_err());

    // Unlock with the correct passphrase: restores access.
    unlock_vault_impl(&app, &vs, PASS).unwrap();
    let entries = list_providers_impl(&vs).unwrap();
    let groq = entries
        .iter()
        .find(|e| e.name == ProviderId::Groq)
        .expect("Groq present after unlock");
    assert!(groq.has_key);
}

#[test]
fn unlock_with_wrong_passphrase_fails() {
    let td = TempDir::new().unwrap();
    let (app, vs) = fresh(&td);
    init_vault_impl(&app, &vs, PASS).unwrap();
    lock_vault_impl(&app, &vs).unwrap();

    let err = unlock_vault_impl(&app, &vs, "WRONG-22-char-passphrase!").unwrap_err();
    assert!(format!("{err}").contains("vault"));
    assert!(!vs.is_unlocked());
}

#[test]
fn init_refuses_when_vault_already_exists() {
    let td = TempDir::new().unwrap();
    let (app, vs) = fresh(&td);
    init_vault_impl(&app, &vs, PASS).unwrap();
    let err = init_vault_impl(&app, &vs, PASS).unwrap_err();
    assert!(format!("{err}").contains("already exists"));
}

#[test]
fn unlock_fails_when_vault_missing() {
    let td = TempDir::new().unwrap();
    let (app, vs) = fresh(&td);
    let err = unlock_vault_impl(&app, &vs, PASS).unwrap_err();
    assert!(format!("{err}").contains("does not exist"));
}

#[test]
fn set_provider_persists_across_lock_cycle() {
    let td = TempDir::new().unwrap();
    let (app, vs) = fresh(&td);
    init_vault_impl(&app, &vs, PASS).unwrap();
    set_provider_impl(
        &app,
        &vs,
        ProviderId::Openai,
        ProviderConfig {
            api_key: Some("sk-test".into()),
            model: Some("whisper-1".into()),
            base_url: None,
            context_tokens: None,
        },
    )
    .unwrap();
    lock_vault_impl(&app, &vs).unwrap();
    unlock_vault_impl(&app, &vs, PASS).unwrap();
    let entries = list_providers_impl(&vs).unwrap();
    let openai = entries
        .iter()
        .find(|e| e.name == ProviderId::Openai)
        .expect("Openai persisted across lock cycle");
    assert_eq!(openai.model, Some("whisper-1".into()));
    assert!(openai.has_key);
}

/// The reset modal tells the user what a reset costs. Voiceprints live inside `DecryptedKeys`, so they go with the vault file; this test checks that.
#[test]
fn resetting_the_vault_destroys_every_enrolled_voiceprint() {
    let td = TempDir::new().unwrap();
    let (app, vs) = fresh(&td);
    init_vault_impl(&app, &vs, PASS).unwrap();

    {
        let mut g = vs.keys.lock().unwrap();
        let keys = g.as_mut().unwrap();
        keys.voiceprints.push(Voiceprint {
            id: "vp-rowan".into(),
            display_name: "Rowan Vance".into(),
            email: None,
            vector: Vec::new(),
            vectors: vec![vec![0.5; 4]],
            samples: Vec::new(),
            created_at_unix_seconds: 1_788_200_000,
            updated_at_unix_seconds: None,
            session_ids: Vec::new(),
            contact_id: None,
            extra: Default::default(),
        });
    }
    // Any vault write persists the whole key blob, the voiceprint with it.
    set_provider_impl(&app, &vs, ProviderId::LmStudio, ProviderConfig::default()).unwrap();
    lock_vault_impl(&app, &vs).unwrap();
    unlock_vault_impl(&app, &vs, PASS).unwrap();
    assert_eq!(
        list_voiceprints_impl(&app, &vs).unwrap().len(),
        1,
        "the enrollment has to survive an ordinary lock cycle first"
    );

    reset_vault_impl(&app, &vs).unwrap();
    init_vault_impl(&app, &vs, PASS).unwrap();
    assert!(
        list_voiceprints_impl(&app, &vs).unwrap().is_empty(),
        "a reset destroys the vault file, and the enrolled voices are in it"
    );
}

/// Opening and closing the vault has one owner, `commands/lifecycle.rs`:
/// only there are the keys and the passphrase set or cleared, and only there
/// and in the pairing commands (`commands/sync.rs`, which open and close the
/// window) is the pairing window replaced. Test modules are exempt: they set
/// the vault up by hand.
#[test]
fn only_the_lifecycle_module_opens_or_closes_the_vault() {
    fn rust_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                rust_files(&p, out);
            } else if p.extension().is_some_and(|x| x == "rs") {
                out.push(p);
            }
        }
    }
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_files(&src, &mut files);
    let owner = |rel: &str, field: &str| {
        rel == "commands/lifecycle.rs" || (field == "sync_window" && rel == "commands/sync.rs")
    };
    let mut found = Vec::new();
    for path in &files {
        let rel = path.strip_prefix(&src).unwrap().to_string_lossy().replace('\\', "/");
        let text = std::fs::read_to_string(path).unwrap();
        // A `#[cfg(test)]` item runs to the next line back at column 0.
        let mut in_test = false;
        let mut opened = false;
        for (i, line) in text.lines().enumerate() {
            if line.trim_start().starts_with("#[cfg(test)]") {
                in_test = true;
                opened = false;
                continue;
            }
            if in_test {
                if !opened {
                    opened = true;
                    continue;
                }
                if line.is_empty() || line.starts_with([' ', '\t', '}']) || line.starts_with("//") || line.starts_with("#[") {
                    continue;
                }
                in_test = false;
            }
            for field in ["keys", "passphrase", "sync_window"] {
                let lock = format!("{field}.lock()");
                let Some(at) = line.find(&lock) else { continue };
                let rest = &line[at..];
                let writes = (line.trim_start().starts_with('*') && rest.contains(") = "))
                    || rest.contains(".take()")
                    || rest.contains(".replace(");
                if writes && !owner(&rel, field) {
                    found.push(format!("  src/{rel}:{}  {}", i + 1, line.trim()));
                }
            }
        }
    }
    assert!(
        found.is_empty(),
        "the vault's keys, passphrase and pairing window are set and cleared in commands/lifecycle.rs:\n{}",
        found.join("\n")
    );
}
