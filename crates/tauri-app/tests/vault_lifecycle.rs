use tauri_app_core::commands::lifecycle::{
    init_vault_impl, list_providers_impl, lock_vault_impl, set_provider_impl, unlock_vault_impl,
    vault_status_impl,
};
use tauri_app_core::profile::ProfileDir;
use tauri_app_core::state::{AppState, ProviderConfig, ProviderId, VaultState};
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
        },
    )
    .unwrap();
    // list_providers_impl always synthesizes a zero-config DaisyGateway
    // entry; the real provider is found by name.
    let entries = list_providers_impl(&vs).unwrap();
    let groq = entries
        .iter()
        .find(|e| e.name == ProviderId::Groq)
        .expect("Groq present");
    assert!(groq.has_key);

    // Lock and verify provider list refuses.
    lock_vault_impl(&vs).unwrap();
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
    lock_vault_impl(&vs).unwrap();

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
        },
    )
    .unwrap();
    lock_vault_impl(&vs).unwrap();
    unlock_vault_impl(&app, &vs, PASS).unwrap();
    let entries = list_providers_impl(&vs).unwrap();
    let openai = entries
        .iter()
        .find(|e| e.name == ProviderId::Openai)
        .expect("Openai persisted across lock cycle");
    assert_eq!(openai.model, Some("whisper-1".into()));
    assert!(openai.has_key);
}
