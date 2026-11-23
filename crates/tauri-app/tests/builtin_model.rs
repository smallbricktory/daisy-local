// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! The built-in model's owner over a temp profile, with the fake worker
//! standing in for daisy-llm: status, selection, install, the shared chat,
//! stop, warm-up and delete.
//!
//! `cargo build -p summarize --bin fake-llm-worker` runs first.
use tauri::Manager as _;
use tauri_app_core::builtin_model::{self, row, staging_dir, Capability};
use tauri_app_core::commands::llm::resolve_optional;
use tauri_app_core::profile::ProfileDir;
use tauri_app_core::settings::Settings;
use tauri_app_core::state::{AppState, DecryptedKeys, ProviderId, VaultState};

fn app() -> (AppState, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let profile = ProfileDir::at(dir.path().join("daisy")).unwrap();
    (AppState::new(profile), dir)
}

fn unlocked() -> VaultState {
    let vs = VaultState::default();
    *vs.keys.lock().unwrap() = Some(DecryptedKeys::default());
    vs
}

/// `DAISY_LLM_BIN` and the fake probe's answer are process-wide, so these
/// tests take turns.
fn one_at_a_time() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

fn install(app: &AppState) {
    let d = app.profile.models_dir().join("llm").join(row().id);
    std::fs::create_dir_all(&d).unwrap();
    for (n, _, size) in row().files {
        std::fs::File::create(d.join(n)).unwrap().set_len(*size).unwrap();
    }
}

fn model_dir(app: &AppState) -> std::path::PathBuf {
    app.profile.models_dir().join("llm").join(row().id)
}

/// `cargo test` builds every bin target of the workspace member under test's
/// dependencies into the same profile dir as this test binary.
fn fake_worker() -> String {
    let mut p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.pop(); p.pop(); // workspace root
    let exe = if cfg!(windows) { "fake-llm-worker.exe" } else { "fake-llm-worker" };
    let profile = if cfg!(debug_assertions) { "debug" } else { "release" };
    let target = std::env::var("CARGO_TARGET_DIR").map(std::path::PathBuf::from).unwrap_or_else(|_| p.join("target"));
    let path = target.join(profile).join(exe);
    assert!(path.is_file(), "build it first: cargo build -p summarize --bin fake-llm-worker ({})", path.display());
    path.to_string_lossy().into_owned()
}

/// The fake worker on a machine it reports as fast.
fn fast_machine() {
    std::env::remove_var("FAKE_LLM_PROBE");
    std::env::set_var("DAISY_LLM_BIN", fake_worker());
}

/// The fake worker on a machine with no GPU.
fn machine_without_a_gpu() {
    std::env::set_var("FAKE_LLM_PROBE", r#"{"devices":[{"name":"CPU","backend":"cpu","kind":"cpu","memory_bytes":0}],"ram_bytes":17179869184}"#);
    std::env::set_var("DAISY_LLM_BIN", fake_worker());
}

fn free(chat: &summarize::local::LocalChat) -> summarize::Result<String> {
    chat.complete_text("", &[("user".into(), "free".into())], 16)
}

/// An unlocked vault with the built-in model selected, and the probe held
/// back long enough to see who waits on it.
fn slow_probe_app(delay_ms: &str) -> (std::sync::Arc<AppState>, std::sync::Arc<VaultState>, tempfile::TempDir) {
    fast_machine();
    std::env::set_var("FAKE_LLM_PROBE_DELAY_MS", delay_ms);
    let (app, d) = app();
    app.set_ai_provider(Some(ProviderId::Local)).unwrap();
    (std::sync::Arc::new(app), std::sync::Arc::new(unlocked()), d)
}

/// How long a second vault user waits while `f` runs the probe on another
/// thread.
fn vault_wait_during(
    app: std::sync::Arc<AppState>,
    vs: std::sync::Arc<VaultState>,
    f: impl FnOnce(&AppState, &VaultState) + Send + 'static,
) -> std::time::Duration {
    let (a, v) = (app.clone(), vs.clone());
    let probing = std::thread::spawn(move || f(&a, &v));
    std::thread::sleep(std::time::Duration::from_millis(300));
    let started = std::time::Instant::now();
    let waited = {
        let _keys = vs.keys.lock().unwrap();
        started.elapsed()
    };
    probing.join().unwrap();
    std::env::remove_var("FAKE_LLM_PROBE_DELAY_MS");
    waited
}

// ---- status ----------------------------------------------------------------

/// Resolving the built-in model probes the hardware, which spawns a
/// subprocess. The vault must not be held across it.
#[test]
fn resolving_the_built_in_model_does_not_hold_the_vault_across_the_probe() {
    let _serial = one_at_a_time();
    let (app, vs, _d) = slow_probe_app("2000");
    let waited = vault_wait_during(app, vs, |a, v| {
        let _ = resolve_optional(a, v, None, None, "summary");
    });
    assert!(waited < std::time::Duration::from_secs(1), "another vault user waited {waited:?} behind the probe");
}

/// Same for the status the AI gates read.
#[test]
fn status_does_not_hold_the_vault_across_the_probe() {
    let _serial = one_at_a_time();
    let (app, vs, _d) = slow_probe_app("2000");
    let waited = vault_wait_during(app, vs, |a, v| {
        let _ = builtin_model::status(a, v);
    });
    assert!(waited < std::time::Duration::from_secs(1), "another vault user waited {waited:?} behind the probe");
}

#[test]
fn status_reports_the_row_with_install_state_and_the_probe_capability() {
    let _serial = one_at_a_time();
    fast_machine();
    let (app, _d) = app();
    let vs = unlocked();
    let s = builtin_model::status(&app, &vs);
    assert_eq!(s.model.id, "gemma-4-e4b-it-q4");
    assert!(!s.model.installed);
    assert!(!s.model.warming);
    assert_eq!(s.model.capability, Capability::Fast);
    assert_eq!(s.model.capability_line, "Fast on this machine.");
    assert_eq!(s.provider.state, "None");
    install(&app);
    assert!(builtin_model::status(&app, &vs).model.installed);
}

/// A cancelled download leaves `<id>.staging` behind with no model dir. The
/// row reports those bytes and delete reclaims them.
#[test]
fn a_partial_download_is_listed_and_deletable() {
    let _serial = one_at_a_time();
    fast_machine();
    let (app, _d) = app();
    let vs = unlocked();
    let stage = staging_dir(&app.profile.models_dir());
    std::fs::create_dir_all(&stage).unwrap();
    std::fs::write(stage.join(row().files[0].0), vec![0u8; 2048]).unwrap();

    let s = builtin_model::status(&app, &vs);
    assert!(!s.model.installed);
    assert_eq!(s.model.partial_bytes, 2048);

    builtin_model::delete(&app).unwrap();
    assert!(!stage.exists());
    assert_eq!(builtin_model::status(&app, &vs).model.partial_bytes, 0);
}

// ---- the shared chat -------------------------------------------------------

/// A locked vault stops every provider. The built-in model needs no vault
/// entry, but locked means no AI runs at all.
#[test]
fn a_locked_vault_stops_the_built_in_model_too() {
    let _serial = one_at_a_time();
    fast_machine();
    let (app, _d) = app();
    install(&app);
    app.set_ai_provider(Some(ProviderId::Local)).unwrap();
    let vs = VaultState::default();
    let e = resolve_optional(&app, &vs, None, None, "summary")
        .err()
        .expect("a locked vault must stop the built-in model");
    assert!(matches!(e, tauri_app_core::error::AppError::VaultLocked), "{e:?}");
}

/// Unlocked, the built-in model resolves with no vault entry of its own.
#[test]
fn the_built_in_model_resolves_without_a_vault_entry() {
    let _serial = one_at_a_time();
    fast_machine();
    let (app, _d) = app();
    install(&app);
    app.set_ai_provider(Some(ProviderId::Local)).unwrap();
    let r = resolve_optional(&app, &unlocked(), None, None, "summary")
        .expect("an unlocked vault resolves the built-in model")
        .expect("the built-in model is this machine's provider");
    assert_eq!(r.provider, ProviderId::Local);
}

#[test]
fn every_caller_shares_one_chat() {
    let _serial = one_at_a_time();
    fast_machine();
    let (app, _d) = app();
    install(&app);
    let vs = unlocked();
    let a = builtin_model::chat(&app, &vs).unwrap();
    let b = builtin_model::chat(&app, &vs).unwrap();
    assert!(std::sync::Arc::ptr_eq(&a, &b));
    assert_eq!(free(&a).unwrap(), free(&b).unwrap(), "one worker answers both");
}

#[test]
fn no_chat_on_a_machine_that_cannot_run_the_model_or_without_the_weights() {
    let _serial = one_at_a_time();
    fast_machine();
    let (bare, _d) = app();
    let vs = unlocked();
    let e = builtin_model::chat(&bare, &vs).err().expect("nothing is installed");
    assert!(e.to_string().contains("isn't downloaded"), "{e}");

    machine_without_a_gpu();
    let (no_gpu, _d2) = app();
    install(&no_gpu);
    let e = builtin_model::chat(&no_gpu, &vs).err().expect("this machine cannot run it");
    assert!(e.to_string().contains("can't run the built-in model"), "{e}");
    std::env::remove_var("FAKE_LLM_PROBE");
}

/// A caller still holding the chat when the model is stopped gets no worker back.
#[test]
fn a_held_chat_does_not_respawn_after_the_model_is_stopped() {
    let _serial = one_at_a_time();
    fast_machine();
    let (app, _d) = app();
    install(&app);
    let vs = unlocked();
    let held = builtin_model::chat(&app, &vs).unwrap();
    free(&held).unwrap();
    builtin_model::stop(&app);

    let fresh = builtin_model::chat(&app, &vs).unwrap();
    let a = free(&fresh).unwrap();
    let b = free(&held);
    assert!(b.is_err(), "the held chat spawned another worker: {a} beside {b:?}");
    assert!(!held.has_worker());
}

/// Stopping the model ends a request in flight, and returns without waiting
/// for it.
#[test]
fn stop_ends_a_request_in_flight() {
    let _serial = one_at_a_time();
    fast_machine();
    let (app, _d) = app();
    install(&app);
    let chat = builtin_model::chat(&app, &unlocked()).unwrap();
    free(&chat).unwrap();
    let caller = chat.clone();
    let request = std::thread::spawn(move || caller.complete_text("", &[("user".into(), "hang".into())], 16));
    std::thread::sleep(std::time::Duration::from_millis(500));
    let started = std::time::Instant::now();
    builtin_model::stop(&app);
    assert!(request.join().unwrap().is_err(), "the request in flight ends with an error");
    assert!(started.elapsed() < std::time::Duration::from_secs(10), "stop waited for the request: {:?}", started.elapsed());
    assert!(!chat.has_worker());
}

// ---- selection and install -------------------------------------------------

#[test]
fn select_takes_an_installed_model_on_a_machine_that_runs_it() {
    let _serial = one_at_a_time();
    fast_machine();
    let (app, _d) = app();
    let e = builtin_model::select(&app).unwrap_err();
    assert!(e.to_string().contains("isn't downloaded"), "{e}");
    assert_eq!(app.ai_provider(), None, "an uninstalled model is not selected");
    install(&app);
    builtin_model::select(&app).unwrap();
    assert_eq!(app.ai_provider(), Some(ProviderId::Local));
}

#[test]
fn select_and_install_refuse_a_machine_that_cannot_run_the_model() {
    let _serial = one_at_a_time();
    machine_without_a_gpu();
    let (app, _d) = app();
    install(&app);
    assert!(builtin_model::select(&app).is_err());
    assert_eq!(app.ai_provider(), None);
    std::fs::remove_dir_all(model_dir(&app)).unwrap();
    assert!(builtin_model::install(&app).is_err());
    assert!(app.downloads.status().is_empty(), "nothing is queued");
    std::env::remove_var("FAKE_LLM_PROBE");
}

#[test]
fn install_queues_the_download_once_and_nothing_for_an_installed_model() {
    let _serial = one_at_a_time();
    fast_machine();
    let (app, _d) = app();
    builtin_model::install(&app).unwrap();
    builtin_model::install(&app).unwrap();
    assert_eq!(app.downloads.status().len(), 1);
    builtin_model::delete(&app).unwrap();
    install(&app);
    builtin_model::install(&app).unwrap();
    assert!(app.downloads.status().is_empty());
}

// ---- delete ----------------------------------------------------------------

#[test]
fn delete_removes_the_dir_and_clears_a_local_choice() {
    let _serial = one_at_a_time();
    fast_machine();
    let (app, _d) = app();
    install(&app);
    let sp = app.profile.settings_path();
    let mut s = Settings::load_or_default(&sp);
    s.default_summary_provider = Some(ProviderId::Local);
    s.save(&sp).unwrap();

    builtin_model::delete(&app).unwrap();
    assert!(!model_dir(&app).exists());
    assert_eq!(app.ai_provider(), None);
    assert_eq!(Settings::load_or_default(&sp).default_summary_provider, Some(ProviderId::Local), "the profile is left to the other machines");
    builtin_model::delete(&app).unwrap();
    assert!(!model_dir(&app).exists());
}

#[test]
fn delete_leaves_another_choice_alone() {
    let _serial = one_at_a_time();
    fast_machine();
    let (app, _d) = app();
    install(&app);
    app.set_ai_provider(Some(ProviderId::Anthropic)).unwrap();
    builtin_model::delete(&app).unwrap();
    assert_eq!(app.ai_provider(), Some(ProviderId::Anthropic));
}

#[test]
fn delete_ends_the_worker_before_the_weights_go() {
    let _serial = one_at_a_time();
    fast_machine();
    let (app, _d) = app();
    install(&app);
    let chat = builtin_model::chat(&app, &unlocked()).unwrap();
    free(&chat).unwrap();
    assert!(chat.has_worker());

    builtin_model::delete(&app).unwrap();
    assert!(!chat.has_worker(), "delete ends the worker holding the weights");
    assert!(free(&chat).is_err(), "and nothing brings one back onto removed weights");
    assert!(!model_dir(&app).exists());
}

/// An installed model plus a leftover staging dir: delete removes both.
#[test]
fn delete_removes_the_model_dir_and_the_staging_dir_together() {
    let _serial = one_at_a_time();
    fast_machine();
    let (app, _d) = app();
    install(&app);
    let stage = staging_dir(&app.profile.models_dir());
    std::fs::create_dir_all(&stage).unwrap();
    std::fs::write(stage.join("leftover.gguf"), b"x").unwrap();
    builtin_model::delete(&app).unwrap();
    assert!(!model_dir(&app).exists());
    assert!(!stage.exists());
}

// ---- after an install ------------------------------------------------------

/// A running app over `app`, with the vault open or not.
fn mock(app: AppState, open: bool) -> tauri::App<tauri::test::MockRuntime> {
    let m = tauri::test::mock_app();
    m.manage(app);
    m.manage(if open { unlocked() } else { VaultState::default() });
    m
}

fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while !done() {
        assert!(std::time::Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// The first install becomes the provider and leaves one resident worker
/// that every later call reuses.
#[test]
fn an_install_selects_the_model_and_warms_one_worker() {
    let _serial = one_at_a_time();
    fast_machine();
    let (app, _d) = app();
    install(&app);
    let m = mock(app, true);
    builtin_model::on_installed(m.handle());
    let (app, vs) = (m.state::<AppState>(), m.state::<VaultState>());
    assert_eq!(app.ai_provider(), Some(ProviderId::Local));
    wait_for("the warm-up", || {
        let s = builtin_model::status(&app, &vs);
        !s.model.warming && builtin_model::chat(&app, &vs).unwrap().has_worker()
    });
    let chat = builtin_model::chat(&app, &vs).unwrap();
    assert_eq!(free(&chat).unwrap(), free(&chat).unwrap(), "the warm-up's worker answers");
}

#[test]
fn an_install_leaves_another_choice_alone() {
    let _serial = one_at_a_time();
    fast_machine();
    let (app, _d) = app();
    install(&app);
    app.set_ai_provider(Some(ProviderId::Openai)).unwrap();
    let m = mock(app, true);
    builtin_model::on_installed(m.handle());
    assert_eq!(m.state::<AppState>().ai_provider(), Some(ProviderId::Openai));
}

/// A worker that will not start on this computer: the install's own
/// selection is taken back, and the row says why.
#[test]
fn a_failed_warm_up_unselects_what_the_install_selected_and_shows_the_failure() {
    let _serial = one_at_a_time();
    fast_machine();
    std::env::set_var("FAKE_LLM_FAIL", "1");
    let (app, _d) = app();
    install(&app);
    let m = mock(app, true);
    builtin_model::on_installed(m.handle());
    let (app, vs) = (m.state::<AppState>(), m.state::<VaultState>());
    wait_for("the warm-up to fail", || builtin_model::status(&app, &vs).model.warm_failed.is_some());
    std::env::remove_var("FAKE_LLM_FAIL");
    let s = builtin_model::status(&app, &vs);
    assert!(s.model.installed, "the weights stay for the person to delete");
    assert!(!s.model.warming);
    assert_eq!(app.ai_provider(), None, "nothing is left selected that cannot run");
    assert_eq!(s.provider.state, "None");

    // Picking it again by hand warms it again; a start that works clears
    // the failure and keeps the choice.
    builtin_model::select(&app).unwrap();
    builtin_model::on_selected(m.handle());
    wait_for("the second warm-up", || builtin_model::status(&app, &vs).model.warm_failed.is_none());
    assert_eq!(app.ai_provider(), Some(ProviderId::Local));
}

/// A choice the person made by hand stays when the warm-up fails.
#[test]
fn a_manual_selection_survives_a_failed_warm_up() {
    let _serial = one_at_a_time();
    fast_machine();
    let (app, _d) = app();
    install(&app);
    builtin_model::select(&app).unwrap();
    std::env::set_var("FAKE_LLM_FAIL", "1");
    let m = mock(app, true);
    builtin_model::on_installed(m.handle());
    let (app, vs) = (m.state::<AppState>(), m.state::<VaultState>());
    wait_for("the warm-up to fail", || builtin_model::status(&app, &vs).model.warm_failed.is_some());
    std::env::remove_var("FAKE_LLM_FAIL");
    assert_eq!(app.ai_provider(), Some(ProviderId::Local));
}

/// A warm-up ended by a lock is not a failure of the model.
#[test]
fn a_warm_up_stopped_by_a_lock_records_no_failure() {
    let _serial = one_at_a_time();
    fast_machine();
    std::env::set_var("FAKE_LLM_HANG", "1");
    let (app, _d) = app();
    install(&app);
    let m = mock(app, true);
    let (app, vs) = (m.state::<AppState>(), m.state::<VaultState>());
    builtin_model::on_installed(m.handle());
    wait_for("the warm-up to start", || builtin_model::status(&app, &vs).model.warming);
    *vs.keys.lock().unwrap() = None;
    builtin_model::stop(&app);
    wait_for("the warm-up to end", || !builtin_model::status(&app, &vs).model.warming);
    std::env::remove_var("FAKE_LLM_HANG");
    assert!(builtin_model::status(&app, &vs).model.warm_failed.is_none());
    assert_eq!(app.ai_provider(), Some(ProviderId::Local));
}

/// An install that lands while the vault is locked selects the model but
/// loads nothing.
#[test]
fn an_install_under_a_locked_vault_starts_no_worker() {
    let _serial = one_at_a_time();
    fast_machine();
    let (app, _d) = app();
    install(&app);
    let m = mock(app, false);
    builtin_model::on_installed(m.handle());
    std::thread::sleep(std::time::Duration::from_millis(500));
    let app = m.state::<AppState>();
    assert!(!builtin_model::status(&app, &m.state::<VaultState>()).model.warming);
    let vs = unlocked();
    assert!(!builtin_model::chat(&app, &vs).unwrap().has_worker(), "no worker was started while locked");
}

/// Delete during the warm-up ends it: the worker is gone, the weights are
/// removed after it, and the model is not selected.
#[test]
fn delete_during_the_warm_up_ends_it_and_selects_nothing() {
    let _serial = one_at_a_time();
    fast_machine();
    std::env::set_var("FAKE_LLM_HANG", "1");
    let (app, _d) = app();
    install(&app);
    let m = mock(app, true);
    let (app, vs) = (m.state::<AppState>(), m.state::<VaultState>());
    builtin_model::on_installed(m.handle());
    wait_for("the warm-up to start", || builtin_model::status(&app, &vs).model.warming);
    let started = std::time::Instant::now();
    builtin_model::delete(&app).unwrap();
    assert!(started.elapsed() < std::time::Duration::from_secs(10), "delete waited on the warm-up: {:?}", started.elapsed());
    assert!(!model_dir(&app).exists());
    assert_eq!(app.ai_provider(), None);
    wait_for("the warm-up to end", || !builtin_model::status(&app, &vs).model.warming);
    std::env::remove_var("FAKE_LLM_HANG");
}
