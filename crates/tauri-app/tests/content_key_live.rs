// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Another desktop's content key syncing into a running app: its sessions
//! list as waiting while its vault file is on the way, and open, without a
//! relaunch, once the key watch finds the file. Runs against Tauri's mock
//! runtime.
mod keys;

use tauri::Manager as _;
use tauri_app_core::commands::library::list_sessions_impl;
use tauri_app_core::commands::lifecycle::{init_vault_impl, open_content_key, spawn_key_watch, vault_opened, ContentKeyOpen};
use tauri_app_core::profile::ProfileDir;
use tauri_app_core::state::{AppState, VaultState};

const PASS: &str = "correct-horse-battery-staple-extended-22";
const THEIR_ID: &str = "daisy-1700000500";

#[test]
fn a_key_that_syncs_into_a_running_app_opens_its_sessions() {
    let _g = keys::exclusive();
    // Their desktop mints its own key and records a meeting.
    let their_dir = tempfile::tempdir().unwrap();
    let theirs = AppState::new(ProfileDir::at(their_dir.path().join("daisy")).unwrap());
    let their_vs = VaultState::new();
    sealfile::keyring::lock();
    init_vault_impl(&theirs, &their_vs, PASS).unwrap();
    assert!(matches!(open_content_key(&theirs, &their_vs).unwrap(), ContentKeyOpen::Minted(_)));
    let session = theirs.profile.sessions_dir().join(THEIR_ID);
    std::fs::create_dir_all(&session).unwrap();
    let manifest = format!(r#"{{"session_id":"{THEIR_ID}","created_at_unix_seconds":1700000500,"title":"Venue walk-through"}}"#);
    sealfile::fs::write(session.join("manifest.json"), manifest).unwrap();
    let their_key = sealfile::keyset::list(theirs.profile.root()).pop().unwrap();
    sealfile::keyring::lock();
    sealfile::keyring::forget_content_public();

    // Ours: a running app with its own key.
    let our_dir = tempfile::tempdir().unwrap();
    let app = tauri::test::mock_app();
    app.manage(AppState::new(ProfileDir::at(our_dir.path().join("daisy")).unwrap()));
    app.manage(VaultState::new());
    app.manage(tauri_app_core::mcp::server::McpHandle::default());
    app.manage(tauri_app_core::sync::server::SyncServerHandle::on_port(0));
    app.manage(tauri_app_core::sync::peripheral::PeripheralHandle::default());
    app.manage(tauri_app_core::commands::recording::ActiveRecordingHandle(std::sync::Mutex::new(None)));
    let handle = app.handle().clone();
    init_vault_impl(&app.state::<AppState>(), &app.state::<VaultState>(), PASS).unwrap();
    vault_opened(&handle);
    assert_eq!(sealfile::keyring::open_publics().len(), 1);
    let ours = app.state::<AppState>();
    let row = || list_sessions_impl(&ours).unwrap().into_iter().find(|r| r.session_id == THEIR_ID);

    // Their session and public key sync in before their vault file.
    let copy = |from: &std::path::Path, to: &std::path::Path| {
        std::fs::create_dir_all(to.parent().unwrap()).unwrap();
        std::fs::copy(from, to).unwrap();
    };
    for name in ["key.json", "manifest.json"] {
        copy(&session.join(name), &ours.profile.sessions_dir().join(THEIR_ID).join(name));
    }
    copy(&their_key.pub_path(), &ours.profile.root().join(their_key.pub_path().file_name().unwrap()));
    let waiting = row().expect("their session lists");
    assert!(waiting.locked && waiting.waiting_for_key, "waits for their key");

    // Their vault file arrives while the app runs; the key watch opens it.
    spawn_key_watch(handle.clone());
    copy(&their_key.vault_path(), &ours.profile.root().join(their_key.vault_path().file_name().unwrap()));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while sealfile::keyring::open_publics().len() < 2 {
        assert!(std::time::Instant::now() < deadline, "the key watch opened their key");
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let opened = row().expect("their session lists");
    assert!(!opened.locked);
    assert_eq!(opened.title.as_deref(), Some("Venue walk-through"));
    sealfile::keyring::lock();
}
