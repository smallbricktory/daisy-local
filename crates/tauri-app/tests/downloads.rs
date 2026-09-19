// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! The download queue over a temp profile: the queue file setup resumes
//! from, and the provider status while a model lands.

use tauri_app_core::downloads::{queue_path, DownloadKind, Phase};
use tauri_app_core::profile::ProfileDir;
use tauri_app_core::state::AppState;

/// The queue a launch resumes is the file the last launch left, with the
/// attempt budget reset.
#[test]
fn setup_resumes_the_entries_the_last_launch_left() {
    let dir = tempfile::tempdir().unwrap();
    let profile = ProfileDir::at(dir.path().join("daisy")).unwrap();
    let first = AppState::new(profile);
    first
        .downloads
        .enqueue(DownloadKind::Llm, "gemma-4-e4b-it-q4")
        .unwrap();
    for _ in 0..5 {
        first.downloads.enqueue(DownloadKind::Llm, "gemma-4-e4b-it-q4").unwrap();
    }
    first
        .downloads
        .enqueue(DownloadKind::SaAssets, "speechanalyzer")
        .unwrap();

    // An on-disk entry with attempts spent, an error recorded, and an id that
    // matches no model row.
    let queue_file = queue_path(&dir.path().join("daisy").join("settings.json"));
    let mut on_disk: Vec<serde_json::Value> =
        serde_json::from_slice(&std::fs::read(&queue_file).unwrap()).unwrap();
    on_disk.push(serde_json::json!({
        "kind": "llm",
        "id": "old-failed-model",
        "attempts": 3,
        "last_error": "timed out"
    }));
    std::fs::write(&queue_file, serde_json::to_vec(&on_disk).unwrap()).unwrap();

    let profile = ProfileDir::at(dir.path().join("daisy")).unwrap();
    let second = AppState::new(profile);
    assert!(second.downloads.status().is_empty(), "nothing before load()");
    second.downloads.load();
    let jobs = second.downloads.status();
    assert_eq!(
        jobs.iter().map(|j| j.id.as_str()).collect::<Vec<_>>(),
        vec!["gemma-4-e4b-it-q4", "speechanalyzer"]
    );
    assert_eq!(jobs[0].phase, Phase::Queued);
    assert_eq!(jobs[1].kind, DownloadKind::SaAssets);
    assert!(jobs[1].indeterminate);
    assert!(second.downloads.is_pending("gemma-4-e4b-it-q4"));
    assert!(!second.downloads.is_pending("old-failed-model"));

    let beside = queue_path(&dir.path().join("daisy").join("settings.json"));
    assert!(beside.is_file(), "{} should exist", beside.display());
}

use tauri_app_core::commands::llm_models::{Capability, LocalStatus};
use tauri_app_core::commands::summary::summary_provider_status_impl;
use tauri_app_core::state::{DecryptedKeys, ProviderId};

#[test]
fn a_queued_built_in_model_reads_as_installing() {
    let keys = DecryptedKeys::default();
    let missing = LocalStatus { installed: false, capability: Capability::Fast };

    let queued = summary_provider_status_impl(ProviderId::Local, Some(&keys), &missing, true);
    assert_eq!(queued.state, "Installing");
    assert_eq!(queued.hint.as_deref(), Some("Daisy's model is still downloading."));

    let idle = summary_provider_status_impl(ProviderId::Local, Some(&keys), &missing, false);
    assert_eq!(idle.state, "Missing");

    let installed = LocalStatus { installed: true, capability: Capability::Fast };
    let done = summary_provider_status_impl(ProviderId::Local, Some(&keys), &installed, true);
    assert_eq!(done.state, "Configured");

    let cant = LocalStatus { installed: false, capability: Capability::Unsupported };
    let no = summary_provider_status_impl(ProviderId::Local, Some(&keys), &cant, true);
    assert_eq!(no.state, "Unsupported");
}

use tauri_app_core::commands::summary::summary_provider_status_impl_from_state;
use tauri_app_core::state::VaultState;

/// `DAISY_LLM_BIN` is process-wide, so these tests take it one at a time.
fn one_at_a_time() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// `cargo test` builds every bin target of the workspace member under test's
/// dependencies into the same profile dir as this test binary.
fn fake_worker() -> String {
    let mut p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.pop();
    p.pop(); // workspace root
    let exe = if cfg!(windows) { "fake-llm-worker.exe" } else { "fake-llm-worker" };
    let profile = if cfg!(debug_assertions) { "debug" } else { "release" };
    let target = std::env::var("CARGO_TARGET_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| p.join("target"));
    let path = target.join(profile).join(exe);
    assert!(path.is_file(), "build it first: cargo build -p summarize --bin fake-llm-worker ({})", path.display());
    path.to_string_lossy().into_owned()
}

/// The status endpoint the AI banners poll, through the real queue: enqueued
/// reads as Installing, cancelled drops back out of it.
#[test]
fn a_pending_queue_entry_reads_as_installing_through_the_full_wiring() {
    let _serial = one_at_a_time();
    std::env::set_var("DAISY_LLM_BIN", fake_worker());
    let dir = tempfile::tempdir().unwrap();
    let profile = ProfileDir::at(dir.path().join("daisy")).unwrap();
    let app = AppState::new(profile);
    app.set_ai_provider(Some(ProviderId::Local)).unwrap();
    let vs = VaultState::default();
    *vs.keys.lock().unwrap() = Some(DecryptedKeys::default());

    app.downloads
        .enqueue(DownloadKind::Llm, summarize::local::LOCAL_MODEL_ID)
        .unwrap();
    let installing = summary_provider_status_impl_from_state(&app, &vs);
    assert_eq!(installing.state, "Installing");
    assert_eq!(installing.hint.as_deref(), Some("Daisy's model is still downloading."));

    app.downloads.cancel(summarize::local::LOCAL_MODEL_ID).unwrap();
    let after_cancel = summary_provider_status_impl_from_state(&app, &vs);
    assert_ne!(after_cancel.state, "Installing");

    std::env::remove_var("DAISY_LLM_BIN");
}

/// The status endpoint before any provider is chosen: a pending built-in
/// download still reads as Installing when the vault is unlocked, and as
/// None when it is locked.
#[test]
fn a_pending_download_reads_as_installing_before_a_provider_is_chosen() {
    let _serial = one_at_a_time();
    std::env::set_var("DAISY_LLM_BIN", fake_worker());
    let dir = tempfile::tempdir().unwrap();
    let profile = ProfileDir::at(dir.path().join("daisy")).unwrap();
    let app = AppState::new(profile);
    let vs = VaultState::default();
    *vs.keys.lock().unwrap() = Some(DecryptedKeys::default());

    app.downloads
        .enqueue(DownloadKind::Llm, summarize::local::LOCAL_MODEL_ID)
        .unwrap();
    let installing = summary_provider_status_impl_from_state(&app, &vs);
    assert_eq!(installing.state, "Installing");
    assert_eq!(installing.provider, Some(ProviderId::Local));
    assert_eq!(installing.hint.as_deref(), Some("Daisy's model is still downloading."));

    app.downloads.cancel(summarize::local::LOCAL_MODEL_ID).unwrap();
    let after_cancel = summary_provider_status_impl_from_state(&app, &vs);
    assert_eq!(after_cancel.state, "None");

    std::env::remove_var("DAISY_LLM_BIN");
}

/// Same setup with the vault locked: the pending download does not surface
/// through a locked vault before a provider is chosen.
#[test]
fn a_pending_download_before_a_provider_is_chosen_stays_none_while_locked() {
    let _serial = one_at_a_time();
    std::env::set_var("DAISY_LLM_BIN", fake_worker());
    let dir = tempfile::tempdir().unwrap();
    let profile = ProfileDir::at(dir.path().join("daisy")).unwrap();
    let app = AppState::new(profile);
    let vs = VaultState::default();

    app.downloads
        .enqueue(DownloadKind::Llm, summarize::local::LOCAL_MODEL_ID)
        .unwrap();
    let status = summary_provider_status_impl_from_state(&app, &vs);
    assert_eq!(status.state, "None");

    std::env::remove_var("DAISY_LLM_BIN");
}

use tauri_app_core::commands::asr_models::delete_asr_model_impl;
use tauri_app_core::commands::llm_models::delete_llm_model_impl;

/// A queued-but-never-downloaded LLM id has nothing installed or staged once
/// its queue entry is gone: delete is a successful no-op, not a "not
/// installed" error.
#[test]
fn deleting_a_queued_uninstalled_llm_model_is_a_no_op() {
    let dir = tempfile::tempdir().unwrap();
    let profile = ProfileDir::at(dir.path().join("daisy")).unwrap();
    let app = AppState::new(profile);

    app.downloads
        .enqueue(DownloadKind::Llm, summarize::local::LOCAL_MODEL_ID)
        .unwrap();
    assert!(app.downloads.is_pending(summarize::local::LOCAL_MODEL_ID));

    delete_llm_model_impl(&app, summarize::local::LOCAL_MODEL_ID).unwrap();
    assert!(!app.downloads.is_pending(summarize::local::LOCAL_MODEL_ID));
    assert!(app.downloads.status().is_empty());
}

/// Same for a queued-but-never-downloaded ASR id.
#[test]
fn deleting_a_queued_uninstalled_asr_model_is_a_no_op() {
    let dir = tempfile::tempdir().unwrap();
    let profile = ProfileDir::at(dir.path().join("daisy")).unwrap();
    let app = AppState::new(profile);

    let id = providers_local::registry::DEFAULT_WHISPER_ID;
    app.downloads.enqueue(DownloadKind::Asr, id).unwrap();
    assert!(app.downloads.is_pending(id));

    delete_asr_model_impl(&app, id).unwrap();
    assert!(!app.downloads.is_pending(id));
    assert!(app.downloads.status().is_empty());
}
