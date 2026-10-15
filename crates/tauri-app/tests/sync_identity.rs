// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
use tauri_app_core::commands::lifecycle::{
    change_vault_passphrase_impl, init_vault_impl, lock_vault_impl, reset_vault_impl,
    switch_vault_mode_impl, unlock_vault_impl,
};
use tauri_app_core::commands::sync::{sync_pairing_begin_impl, sync_status_impl};
use tauri_app_core::profile::ProfileDir;
use tauri_app_core::state::{AppState, VaultState};
use tauri_app_core::sync::identity::{forget_cached, identity_path, load_or_create, save, PairedPhone};

const PASS: &str = "lantern-copper-walrus-orbit-2026";

fn unlocked() -> (AppState, VaultState, tempfile::TempDir) {
    let td = tempfile::tempdir().unwrap();
    let app = AppState::new(ProfileDir::at(td.path().join("daisy")).unwrap());
    let vs = VaultState::new();
    init_vault_impl(&app, &vs, PASS).unwrap();
    (app, vs, td)
}

#[test]
fn identity_is_minted_once_and_persists_encrypted() {
    let (app, vs, _td) = unlocked();
    let a = load_or_create(&app, &vs).unwrap();
    assert_eq!(a.schema_version, 2);
    assert_eq!(a.device_id.len(), 32, "16 random bytes, hex");
    assert!(a.paired.is_empty());
    let b = load_or_create(&app, &vs).unwrap();
    assert_eq!(a, b, "a second load returns the same identity");

    let path = identity_path(&app);
    let name = path.file_name().unwrap().to_string_lossy().into_owned();
    assert!(name.starts_with("sync.") && name.ends_with(".vault.json"), "{name}");
    let raw = std::fs::read_to_string(&path).unwrap();
    assert!(!raw.contains(&a.device_id), "the file is an encrypted envelope");
    assert!(raw.contains("\"kdf\""));
}

#[test]
fn identity_reopens_after_relock() {
    let (app, vs, _td) = unlocked();
    let a = load_or_create(&app, &vs).unwrap();
    let vs2 = VaultState::new();
    unlock_vault_impl(&app, &vs2, PASS).unwrap();
    assert_eq!(load_or_create(&app, &vs2).unwrap(), a);
}

#[test]
fn keys_derive_from_the_stored_seeds() {
    let (app, vs, _td) = unlocked();
    let id = load_or_create(&app, &vs).unwrap();
    assert_eq!(id.signing_key().verifying_key().to_bytes().len(), 32);
    assert_eq!(id.ed25519_pub_b64().len(), 44);
    assert_eq!(id.tls_spki_sha256_b64().len(), 44);
    assert_eq!(id.x25519_pub_b64().len(), 44);
}

/// Schema 1 paired under a protocol the phone never spoke.
#[test]
fn an_older_schema_is_replaced_rather_than_read() {
    let (app, vs, _td) = unlocked();
    let first = load_or_create(&app, &vs).unwrap();
    let mut stale = serde_json::to_value(&first).unwrap();
    stale["schema_version"] = serde_json::json!(1);
    let env = vault::encrypt(&serde_json::to_vec(&stale).unwrap(), PASS).unwrap();
    std::fs::write(identity_path(&app), serde_json::to_vec(&env).unwrap()).unwrap();
    forget_cached();

    let fresh = load_or_create(&app, &vs).unwrap();
    assert_eq!(fresh.schema_version, 2);
    assert_ne!(fresh.device_id, first.device_id, "a new identity, not a patched one");
    assert_eq!(load_or_create(&app, &vs).unwrap(), fresh, "and it persisted");
}

/// Seals `value` as the identity file, as another build would have written it.
fn write_raw(app: &AppState, value: &serde_json::Value) {
    let env = vault::encrypt(&serde_json::to_vec(value).unwrap(), PASS).unwrap();
    std::fs::write(identity_path(app), serde_json::to_vec(&env).unwrap()).unwrap();
    forget_cached();
}

/// A downgrade leaves a newer identity in place: its pairings come back
/// when the newer build does.
#[test]
fn a_newer_schema_is_refused_and_kept() {
    let (app, vs, _td) = unlocked();
    let mut newer = serde_json::to_value(load_or_create(&app, &vs).unwrap()).unwrap();
    newer["schema_version"] = serde_json::json!(3);
    write_raw(&app, &newer);
    let before = std::fs::read(identity_path(&app)).unwrap();

    assert!(load_or_create(&app, &vs).is_err());
    assert_eq!(std::fs::read(identity_path(&app)).unwrap(), before, "the file is untouched");
}

/// A newer build's fields, on the identity and on a phone, survive a save
/// by this one.
#[test]
fn fields_from_a_newer_build_survive_a_save() {
    let (app, vs, _td) = unlocked();
    let mut id = load_or_create(&app, &vs).unwrap();
    id.paired.push(a_phone("phone-9"));
    let mut raw = serde_json::to_value(&id).unwrap();
    raw["future_identity"] = serde_json::json!({"k": 1});
    raw["paired"][0]["future_phone"] = serde_json::json!("x");
    write_raw(&app, &raw);

    let mut id = load_or_create(&app, &vs).unwrap();
    id.paired[0].display_name = Some("Desk phone".into());
    save(&app, &vs, &id).unwrap();
    forget_cached();

    let back = serde_json::to_value(load_or_create(&app, &vs).unwrap()).unwrap();
    assert_eq!(back["future_identity"], serde_json::json!({"k": 1}));
    assert_eq!(back["paired"][0]["future_phone"], serde_json::json!("x"));
    assert_eq!(back["paired"][0]["display_name"], serde_json::json!("Desk phone"));
}

fn a_phone(device_id: &str) -> PairedPhone {
    PairedPhone {
        device_id: device_id.into(),
        name: "Sable's phone".into(),
        platform: "ios".into(),
        ed25519_pub_b64: "AAAA".into(),
        tls_cert_der_b64: "BBBB".into(),
        pairing_key_b64: "CCCC".into(),
        service_uuid: "11111111-1111-4111-8111-111111111111".into(),
        wake_uuid: "22222222-2222-4222-8222-222222222222".into(),
        address_uuid: "33333333-3333-4333-8333-333333333333".into(),
        paired_at_unix: 1_800_000_000,
        display_name: None,
        unknown: Default::default(),
    }
}

#[test]
fn save_round_trips_pairings() {
    let (app, vs, _td) = unlocked();
    let mut id = load_or_create(&app, &vs).unwrap();
    id.paired.push(a_phone("phone-3"));
    save(&app, &vs, &id).unwrap();
    assert_eq!(load_or_create(&app, &vs).unwrap().paired, id.paired);
}

#[test]
fn locked_vault_refuses() {
    let td = tempfile::tempdir().unwrap();
    let app = AppState::new(ProfileDir::at(td.path().join("daisy")).unwrap());
    assert!(load_or_create(&app, &VaultState::new()).is_err());
}

const NEW_PASS: &str = "granite-orchard-signal-velvet-9";

fn reopened(app: &AppState, pass: &str) -> VaultState {
    let vs = VaultState::new();
    unlock_vault_impl(app, &vs, pass).unwrap();
    vs
}

#[test]
fn passphrase_change_reseals_the_identity() {
    let (app, vs, _td) = unlocked();
    let before = load_or_create(&app, &vs).unwrap();
    change_vault_passphrase_impl(&app, &vs, PASS, NEW_PASS).unwrap();
    assert_eq!(load_or_create(&app, &vs).unwrap(), before, "the stashed passphrase moved with it");
    let vs2 = reopened(&app, NEW_PASS);
    assert_eq!(load_or_create(&app, &vs2).unwrap(), before, "and so did the file");
}

#[test]
fn passphrase_change_before_any_identity_is_fine() {
    let (app, vs, _td) = unlocked();
    change_vault_passphrase_impl(&app, &vs, PASS, NEW_PASS).unwrap();
    assert!(!identity_path(&app).is_file());
    load_or_create(&app, &vs).unwrap();
}

#[test]
fn mode_switch_reseals_the_identity() {
    let (app, vs, _td) = unlocked();
    let before = load_or_create(&app, &vs).unwrap();
    switch_vault_mode_impl(&app, &vs, Some(NEW_PASS)).unwrap();
    let vs2 = reopened(&app, NEW_PASS);
    assert_eq!(load_or_create(&app, &vs2).unwrap(), before);
}

#[test]
fn reset_removes_the_identity() {
    let (app, vs, _td) = unlocked();
    load_or_create(&app, &vs).unwrap();
    reset_vault_impl(&app, &vs).unwrap();
    assert!(!identity_path(&app).is_file());
}

#[test]
fn lock_closes_the_pairing_window() {
    let (app, vs, _td) = unlocked();
    sync_pairing_begin_impl(&app, &vs, "studio-mac.local".into(), 1_800_000_000).unwrap();
    assert!(sync_status_impl(&app, &vs, true, "on").unwrap().window.is_some());
    lock_vault_impl(&app, &vs).unwrap();
    let vs2 = reopened(&app, PASS);
    assert!(sync_status_impl(&app, &vs2, true, "on").unwrap().window.is_none());
}

/// Concurrent first callers converge on one identity.
#[test]
fn concurrent_first_reads_agree_on_one_identity() {
    let (app, vs, _td) = unlocked();
    let app = std::sync::Arc::new(app);
    let vs = std::sync::Arc::new(vs);
    let racers: Vec<_> = (0..8)
        .map(|_| {
            let (app, vs) = (app.clone(), vs.clone());
            std::thread::spawn(move || load_or_create(&app, &vs).unwrap().device_id)
        })
        .collect();
    let ids: std::collections::BTreeSet<String> = racers.into_iter().map(|h| h.join().unwrap()).collect();
    assert_eq!(ids.len(), 1, "one identity, not one per caller: {ids:?}");
}
