// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! The built-in model's list / delete / default-provider behavior over a temp
//! profile, with the fake worker standing in for daisy-llm.
//!
//! `cargo build -p summarize --bin fake-llm-worker` runs first — `cargo test
//! -p tauri-app` after `cargo build --workspace` already has it; CI's linux
//! job builds the workspace before testing.
use tauri_app_core::commands::llm_models::{delete_llm_model_impl, list_llm_models_impl, row, set_default_provider_if_unset, staging_dir, warm_and_select, Capability};
use tauri_app_core::commands::llm::resolve_optional;
use tauri_app_core::commands::summary::summary_provider_status_impl_from_state;
use tauri_app_core::profile::ProfileDir;
use tauri_app_core::settings::Settings;
use tauri_app_core::state::{AppState, DecryptedKeys, ProviderId, VaultState};

fn app() -> (AppState, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let profile = ProfileDir::at(dir.path().join("daisy")).unwrap();
    (AppState::new(profile), dir)
}

/// The install guard and `DAISY_LLM_BIN` are process-wide, so these tests take
/// the model id one at a time.
fn one_at_a_time() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

fn install(app: &AppState) {
    let d = app.profile.models_dir().join("llm").join(row().id);
    std::fs::create_dir_all(&d).unwrap();
    for (n, _) in row().files {
        std::fs::write(d.join(n), b"x").unwrap();
    }
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

/// An unlocked vault with the built-in model selected, and the probe held
/// back long enough to see who waits on it.
fn slow_probe_app(delay_ms: &str) -> (std::sync::Arc<AppState>, std::sync::Arc<VaultState>, tempfile::TempDir) {
    std::env::set_var("DAISY_LLM_BIN", fake_worker());
    std::env::set_var("FAKE_LLM_PROBE_DELAY_MS", delay_ms);
    let (app, d) = app();
    app.set_ai_provider(Some(ProviderId::Local)).unwrap();
    let vs = std::sync::Arc::new(VaultState::default());
    *vs.keys.lock().unwrap() = Some(DecryptedKeys::default());
    (std::sync::Arc::new(app), vs, d)
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

/// Resolving the built-in model probes the hardware, which spawns a
/// subprocess. The vault must not be held across it.
#[test]
fn resolving_the_built_in_model_does_not_hold_the_vault_across_the_probe() {
    let _serial = one_at_a_time();
    let (app, vs, _d) = slow_probe_app("2000");
    let waited = vault_wait_during(app, vs, |a, v| {
        let _ = resolve_optional(a, v, None, None, "summary");
    });
    assert!(
        waited < std::time::Duration::from_secs(1),
        "another vault user waited {waited:?} behind the probe"
    );
}

/// Same for the provider-status call the AI banners poll.
#[test]
fn provider_status_does_not_hold_the_vault_across_the_probe() {
    let _serial = one_at_a_time();
    let (app, vs, _d) = slow_probe_app("2000");
    let waited = vault_wait_during(app, vs, |a, v| {
        let _ = summary_provider_status_impl_from_state(a, v);
    });
    assert!(
        waited < std::time::Duration::from_secs(1),
        "another vault user waited {waited:?} behind the probe"
    );
}

/// A locked vault stops every provider. The built-in model needs no vault
/// entry, but locked means no AI runs at all.
#[test]
fn a_locked_vault_stops_the_built_in_model_too() {
    let _serial = one_at_a_time();
    std::env::set_var("DAISY_LLM_BIN", fake_worker());
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
    std::env::set_var("DAISY_LLM_BIN", fake_worker());
    let (app, _d) = app();
    install(&app);
    app.set_ai_provider(Some(ProviderId::Local)).unwrap();
    let vs = VaultState::default();
    *vs.keys.lock().unwrap() = Some(DecryptedKeys::default());
    let r = resolve_optional(&app, &vs, None, None, "summary")
        .expect("an unlocked vault resolves the built-in model")
        .expect("the built-in model is this machine's provider");
    assert_eq!(r.provider, ProviderId::Local);
}

#[test]
fn list_reports_the_one_row_with_install_state_and_the_probe_capability() {
    let _serial = one_at_a_time();
    // The fake worker's default probe is an 8 GiB GPU → fast. Each AppState
    // probes once and keeps the answer, so a test that wants a different one
    // sets DAISY_LLM_BIN before it builds its AppState.
    std::env::set_var("DAISY_LLM_BIN", fake_worker());
    let (app, _d) = app();
    let rows = list_llm_models_impl(&app);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, "gemma-4-e4b-it-q4");
    assert!(!rows[0].installed);
    assert_eq!(rows[0].capability, Capability::Fast);
    assert_eq!(rows[0].capability_line, "Fast on this machine.");
    install(&app);
    assert!(list_llm_models_impl(&app)[0].installed);
}

#[test]
fn delete_removes_the_dir_and_clears_a_local_default() {
    let _serial = one_at_a_time();
    std::env::set_var("DAISY_LLM_BIN", fake_worker());
    let (app, _d) = app();
    install(&app);
    let sp = app.profile.settings_path();
    let mut s = Settings::load_or_default(&sp);
    s.default_summary_provider = Some(ProviderId::Local);
    s.save(&sp).unwrap();

    delete_llm_model_impl(&app, "gemma-4-e4b-it-q4").unwrap();
    assert!(!app.profile.models_dir().join("llm").join("gemma-4-e4b-it-q4").exists());
    assert_eq!(app.ai_provider(), None);
    assert_eq!(Settings::load_or_default(&sp).default_summary_provider, Some(ProviderId::Local), "the profile is left to the other machines");
    delete_llm_model_impl(&app, "gemma-4-e4b-it-q4").unwrap();
    assert!(!app.profile.models_dir().join("llm").join("gemma-4-e4b-it-q4").exists());
}

#[test]
fn delete_stops_a_resident_worker() {
    let _serial = one_at_a_time();
    let worker = fake_worker();
    std::env::set_var("DAISY_LLM_BIN", &worker);
    let (app, _d) = app();
    install(&app);
    let model = app.profile.models_dir().join("llm").join(row().id).join(row().files[0].0);
    let chat = app.local_chat(std::path::Path::new(&worker), &model);
    chat.complete_text("", &[("user".into(), "free".into())], 16).unwrap();
    assert!(chat.has_worker());

    delete_llm_model_impl(&app, "gemma-4-e4b-it-q4").unwrap();
    assert!(!chat.has_worker(), "delete kills the worker holding the weights");
    assert!(!app.profile.models_dir().join("llm").join(row().id).exists());
}

/// Delete during a request on the built-in model ends that request and
/// returns without waiting for it.
#[test]
fn delete_ends_a_request_in_flight() {
    let _serial = one_at_a_time();
    let worker = fake_worker();
    std::env::set_var("DAISY_LLM_BIN", &worker);
    let (app, _d) = app();
    install(&app);
    let model = app.profile.models_dir().join("llm").join(row().id).join(row().files[0].0);
    let chat = app.local_chat(std::path::Path::new(&worker), &model);
    chat.complete_text("", &[("user".into(), "free".into())], 16).unwrap();
    let caller = chat.clone();
    let request = std::thread::spawn(move || caller.complete_text("", &[("user".into(), "hang".into())], 16));
    std::thread::sleep(std::time::Duration::from_millis(500));

    let started = std::time::Instant::now();
    delete_llm_model_impl(&app, row().id).unwrap();
    assert!(request.join().unwrap().is_err(), "the request in flight ends with an error");
    assert!(started.elapsed() < std::time::Duration::from_secs(10), "delete waited for the request: {:?}", started.elapsed());
    assert!(!app.profile.models_dir().join("llm").join(row().id).exists());
}

#[test]
fn delete_leaves_another_default_alone() {
    let _serial = one_at_a_time();
    let (app, _d) = app();
    install(&app);
    let sp = app.profile.settings_path();
    let mut s = Settings::load_or_default(&sp);
    s.default_summary_provider = Some(ProviderId::Anthropic);
    s.save(&sp).unwrap();
    delete_llm_model_impl(&app, "gemma-4-e4b-it-q4").unwrap();
    assert_eq!(app.ai_provider(), Some(ProviderId::Anthropic));
}

#[test]
fn default_is_set_only_when_nothing_else_is() {
    let _serial = one_at_a_time();
    let (app, _d) = app();
    set_default_provider_if_unset(&app).unwrap();
    assert_eq!(app.ai_provider(), Some(ProviderId::Local));
    app.set_ai_provider(Some(ProviderId::Openai)).unwrap();
    set_default_provider_if_unset(&app).unwrap();
    assert_eq!(app.ai_provider(), Some(ProviderId::Openai));
    assert_eq!(Settings::load_or_default(&app.profile.settings_path()).default_summary_provider, None);
}

/// A cancelled download leaves `<id>.staging` behind with no model dir. The
/// row reports those bytes and delete reclaims them.
#[test]
fn a_partial_download_is_listed_and_deletable() {
    let _serial = one_at_a_time();
    std::env::set_var("DAISY_LLM_BIN", fake_worker());
    let (app, _d) = app();
    let llm = app.profile.models_dir().join("llm");
    let stage = staging_dir(&llm, row().id);
    std::fs::create_dir_all(&stage).unwrap();
    std::fs::write(stage.join(row().files[0].0), vec![0u8; 2048]).unwrap();

    let rows = list_llm_models_impl(&app);
    assert!(!rows[0].installed);
    assert_eq!(rows[0].partial_bytes, 2048);

    delete_llm_model_impl(&app, row().id).unwrap();
    assert!(!stage.exists());
    assert_eq!(list_llm_models_impl(&app)[0].partial_bytes, 0);
}

/// An installed model plus a leftover staging dir: delete removes both.
#[test]
fn delete_removes_the_model_dir_and_the_staging_dir_together() {
    let _serial = one_at_a_time();
    let (app, _d) = app();
    install(&app);
    let llm = app.profile.models_dir().join("llm");
    let stage = staging_dir(&llm, row().id);
    std::fs::create_dir_all(&stage).unwrap();
    std::fs::write(stage.join("leftover.gguf"), b"x").unwrap();
    delete_llm_model_impl(&app, row().id).unwrap();
    assert!(!llm.join(row().id).exists());
    assert!(!stage.exists());
}

/// Cancel during the warm-up kills the worker and returns without waiting for
/// the request it sent. The installed model still becomes the provider.
#[test]
fn a_cancelled_warm_up_stops_the_worker_and_selects_the_model() {
    let _serial = one_at_a_time();
    let worker = fake_worker();
    let (app, _d) = app();
    // The fake worker hangs on every request when its model path says so.
    let model = std::path::Path::new("hang.gguf");
    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = cancel.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(300));
        flag.store(true, std::sync::atomic::Ordering::SeqCst);
    });
    let started = std::time::Instant::now();
    warm_and_select(&app, std::path::Path::new(&worker), model, &cancel).unwrap();
    assert!(started.elapsed() < std::time::Duration::from_secs(30), "it returned promptly");
    assert_eq!(app.ai_provider(), Some(ProviderId::Local));
    assert!(
        !app.local_chat(std::path::Path::new(&worker), model).has_worker(),
        "the cancelled warm-up left the worker running"
    );
}

/// Cancel set before the warm-up has spawned its worker still ends the
/// warm-up, instead of leaving it to run out the request timeout.
#[test]
fn a_warm_up_cancelled_before_the_worker_starts_returns() {
    let _serial = one_at_a_time();
    let worker = fake_worker();
    let (app, _d) = app();
    let app = std::sync::Arc::new(app);
    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let (tx, rx) = std::sync::mpsc::channel();
    let (warming, bin, flag) = (app.clone(), worker.clone(), cancel.clone());
    std::thread::spawn(move || {
        let r = warm_and_select(&warming, std::path::Path::new(&bin), std::path::Path::new("hang.gguf"), &flag);
        let _ = tx.send(r);
    });
    rx.recv_timeout(std::time::Duration::from_secs(20))
        .expect("the warm-up never returned: cancel set before the worker spawned reached nothing")
        .unwrap();
    assert_eq!(app.ai_provider(), Some(ProviderId::Local));
    assert!(
        !app.local_chat(std::path::Path::new(&worker), std::path::Path::new("hang.gguf")).has_worker(),
        "the cancelled warm-up left the worker running"
    );
}

/// Delete during the warm-up is refused by name, so the weights stay on disk
/// while the worker is loading them.
#[test]
fn delete_is_refused_while_the_warm_up_holds_the_model() {
    let _serial = one_at_a_time();
    let worker = fake_worker();
    std::env::set_var("DAISY_LLM_BIN", &worker);
    let (app, _d) = app();
    install(&app);
    let app = std::sync::Arc::new(app);
    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (tx, rx) = std::sync::mpsc::channel();
    let (warming, bin, flag) = (app.clone(), worker.clone(), cancel.clone());
    let warm_thread = std::thread::spawn(move || {
        let r = warm_and_select(&warming, std::path::Path::new(&bin), std::path::Path::new("hang.gguf"), &flag);
        let _ = tx.send(r);
    });
    std::thread::sleep(std::time::Duration::from_millis(500));

    let e = delete_llm_model_impl(&app, row().id).unwrap_err();
    assert!(e.to_string().contains("still installing"), "{e}");
    assert!(
        app.profile.models_dir().join("llm").join(row().id).is_dir(),
        "delete removed the weights the warm-up is loading"
    );

    cancel.store(true, std::sync::atomic::Ordering::SeqCst);
    rx.recv_timeout(std::time::Duration::from_secs(20)).unwrap().unwrap();
    warm_thread.join().unwrap();
    delete_llm_model_impl(&app, row().id).unwrap();
}

/// The warm-up spawns the worker every later call reuses, instead of a second
/// process loading the same weights.
#[test]
fn the_warm_up_leaves_one_resident_worker_behind() {
    let _serial = one_at_a_time();
    let worker = fake_worker();
    let (app, _d) = app();
    let model = std::path::Path::new("model.gguf");
    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    warm_and_select(&app, std::path::Path::new(&worker), model, &cancel).unwrap();
    let chat = app.local_chat(std::path::Path::new(&worker), model);
    assert!(chat.has_worker(), "the warm-up's worker stays resident");
    // The fake worker's "free" reply carries its pid: the warm-up's process is
    // the one the next call reaches.
    let pid = chat.complete_text("", &[("user".into(), "free".into())], 16).unwrap();
    assert_eq!(pid, chat.complete_text("", &[("user".into(), "free".into())], 16).unwrap());
}
