// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! ASR model management over the engine-typed registry: list rows, switch the
//! active model, delete installed ones, download and resolve the active
//! engine+model.
//!
//! Layout: whisper models are `<profile>/models/ggml-<size>.bin` files;
//! nemotron models are `<profile>/models/nemotron/<id>/` dirs. Platform-floor
//! rows ship in the app bundle (resource dirs found via the model-dir env
//! vars); their bundled copies cannot be deleted. `resolve_asr_chain` is the
//! one resolution order every consumer derives from.

use crate::error::{AppError, Result};
use crate::state::AppState;
use providers_local::registry::{self, Engine, ModelRow};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

/// Bundled resource dirs (the model-dir env vars set at startup), captured as
/// a value so resolution is testable without touching process env.
pub struct BundledDirs {
    pub whisper: Option<PathBuf>,
    pub nemotron: Option<PathBuf>,
}

impl BundledDirs {
    pub fn from_env() -> Self {
        Self {
            whisper: std::env::var_os("DAISY_WHISPER_MODEL_DIR").map(PathBuf::from),
            nemotron: std::env::var_os("DAISY_NEMOTRON_MODEL_DIR").map(PathBuf::from),
        }
    }

    #[cfg(test)]
    fn none() -> Self {
        Self { whisper: None, nemotron: None }
    }
}

/// Ordered candidates for "what will transcribe", installed rows only:
/// the explicit `asr.active` row → this platform's bundled floor row → the
/// default whisper model → every other installed row in registry order,
/// deduped. The head is what dispatch runs; engine-specific consumers take
/// their first row of that engine. Every consumer (record-only banner, live
/// dispatch, batch dispatch, model list) derives from this one chain.
pub fn resolve_asr_chain(
    machine: &crate::settings::MachineSettings,
    models_dir: &Path,
) -> Vec<(&'static ModelRow, PathBuf)> {
    resolve_asr_chain_on(
        machine,
        models_dir,
        std::env::consts::OS,
        providers_local::macos::sa_ready(),
        &BundledDirs::from_env(),
    )
}

/// The chain with platform, SpeechAnalyzer availability, and bundled dirs as inputs.
fn resolve_asr_chain_on(
    machine: &crate::settings::MachineSettings,
    models_dir: &Path,
    os: &str,
    sa_ready: bool,
    bundled: &BundledDirs,
) -> Vec<(&'static ModelRow, PathBuf)> {
    let mut out: Vec<(&'static ModelRow, PathBuf)> = Vec::new();
    let push = |row: &'static ModelRow, out: &mut Vec<(&'static ModelRow, PathBuf)>| {
        if out.iter().any(|(r, _)| r.id == row.id) {
            return;
        }
        if let Some(p) = installed_for(row, models_dir, sa_ready, bundled) {
            out.push((row, p));
        }
    };
    if let Some(row) = machine.asr.active.as_deref().and_then(registry::row) {
        push(row, &mut out);
    }
    if let Some(row) = registry::REGISTRY.iter().find(|r| r.floor_on.contains(&os)) {
        push(row, &mut out);
    }
    if let Some(row) = registry::row(registry::DEFAULT_WHISPER_ID) {
        push(row, &mut out);
    }
    for row in registry::REGISTRY {
        push(row, &mut out);
    }
    out
}

/// The row live/batch dispatch runs: head of the chain. The path is the ggml
/// file or nemotron dir; empty for the OS-managed SpeechAnalyzer.
pub fn resolve_active_asr(
    machine: &crate::settings::MachineSettings,
    models_dir: &Path,
) -> Option<(&'static ModelRow, PathBuf)> {
    resolve_asr_chain(machine, models_dir).into_iter().next()
}

/// "Installed" for a registry row. File-backed rows resolve on disk, the profile copy first, else the bundled resource dir, and need the full pinned file set.
/// SpeechAnalyzer is OS-managed: installed means usable now (capability AND system-locale assets), and its path is empty.
pub(crate) fn installed_for(
    row: &ModelRow,
    models_dir: &Path,
    sa_ready: bool,
    bundled: &BundledDirs,
) -> Option<PathBuf> {
    if row.engine == Engine::SpeechAnalyzer {
        return sa_ready.then(PathBuf::new);
    }
    bases_for(row, models_dir, bundled)
        .into_iter()
        .flatten()
        .find(|b| match row.presence_in(b) {
            providers_local::registry::Presence::Installed => true,
            providers_local::registry::Presence::Missing => false,
            providers_local::registry::Presence::Unreadable { path, error } => {
                log::warn!("model {}: cannot read {}: {error}", row.id, path.display());
                false
            }
        })
        .and_then(|b| row.engine_path(&b))
}

/// Lookup bases for a file-backed row, in precedence order: the profile
/// models dir, then the bundled resource dir.
fn bases_for(row: &ModelRow, models_dir: &Path, bundled: &BundledDirs) -> [Option<PathBuf>; 2] {
    match row.engine {
        Engine::Whisper => [Some(models_dir.to_path_buf()), bundled.whisper.clone()],
        Engine::Nemotron => [Some(models_dir.join("nemotron")), bundled.nemotron.clone()],
        Engine::SpeechAnalyzer => [None, None],
        Engine::Llm => unreachable!("LLM rows live in LLM_REGISTRY"),
    }
}

/// One registry row projected for the frontend model list.
#[derive(Debug, Clone, Serialize)]
pub struct AsrModelInfo {
    pub id: String,
    pub engine: Engine,
    pub display: String,
    pub installed: bool,
    pub active: bool,
    /// Ships in this platform's bundle; the bundled copy cannot be deleted.
    pub bundled_floor: bool,
    /// A profile-side copy exists (what Delete removes; for a floor row this
    /// shadows the bundled copy).
    pub profile_copy: bool,
    pub multilingual: bool,
    /// Approximate install size.
    pub bytes: u64,
    pub license: String,
    pub license_url: String,
}

/// This platform bundles the row (undeletable, preselected).
fn floor_here(row: &ModelRow) -> bool {
    row.floor_on.contains(&std::env::consts::OS)
}

pub fn list_asr_models_impl(app: &AppState) -> Result<Vec<AsrModelInfo>> {
    let machine = app.machine_settings();
    let models_dir = app.profile.models_dir();
    let sa = providers_local::macos::sa_ready();
    let bundled = BundledDirs::from_env();
    let active_id = resolve_active_asr(&machine, &models_dir).map(|(row, _)| row.id);
    let out = registry::REGISTRY
        .iter()
        // The SpeechAnalyzer row exists only where the OS can transcribe
        // (macOS 26+ on a supported machine); assets may still be missing —
        // that is the not-installed state whose Download runs the OS fetch.
        .filter(|r| {
            r.engine != Engine::SpeechAnalyzer || providers_local::macos::sa_available()
        })
        .map(|r| AsrModelInfo {
            id: r.id.to_string(),
            engine: r.engine,
            display: r.display.to_string(),
            installed: installed_for(r, &models_dir, sa, &bundled).is_some(),
            active: active_id == Some(r.id),
            bundled_floor: floor_here(r),
            profile_copy: profile_copy_dir(r, &models_dir).is_some(),
            multilingual: r.multilingual,
            bytes: r.bytes,
            license: r.license.to_string(),
            license_url: r.license_url.to_string(),
        })
        .collect();
    Ok(out)
}

/// The event every window follows the model rows by.
pub const MODELS_CHANGED: &str = "models:changed";

/// Serializes reads with their emits.
static EMITTING: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Tells every window what the model rows say now: after an install, a
/// delete, or a change of the active model. Reads them on its own thread.
pub fn emit<R: tauri::Runtime>(handle: &tauri::AppHandle<R>) {
    let handle = handle.clone();
    std::thread::spawn(move || {
        use tauri::{Emitter, Manager};
        let _order = EMITTING.lock().unwrap_or_else(|e| e.into_inner());
        let Some(app) = handle.try_state::<AppState>() else { return };
        match list_asr_models_impl(&app) {
            Ok(rows) => {
                if let Err(e) = handle.emit(MODELS_CHANGED, rows) {
                    log::warn!("asr models: change not emitted: {e}");
                }
            }
            Err(e) => log::warn!("asr models: rows not read: {e}"),
        }
    });
}

pub fn set_active_asr_model_impl(app: &AppState, id: &str) -> Result<()> {
    let row = registry::row(id)
        .ok_or_else(|| AppError::Config(format!("unknown ASR model {id:?}")))?;
    let sa = providers_local::macos::sa_ready();
    if installed_for(row, &app.profile.models_dir(), sa, &BundledDirs::from_env()).is_none() {
        return Err(AppError::Config(format!("model {id} is not installed")));
    }
    app.update_machine_settings(|m| m.asr.active = Some(id.to_string()))
        .map_err(|e| AppError::Io(e.to_string()))?;
    Ok(())
}

/// The profile-side copy of a file-backed row, if any of its files exist:
/// the ggml file, or the nemotron model dir. This is what Delete removes —
/// a bundled copy (floor row) is untouched, so deleting a floor row's
/// profile copy reverts it to the bundle.
fn profile_copy_dir(row: &ModelRow, models_dir: &Path) -> Option<PathBuf> {
    match row.engine {
        Engine::Whisper => {
            let p = models_dir.join(row.id);
            p.is_file().then_some(p)
        }
        Engine::Nemotron => {
            let d = models_dir.join("nemotron").join(row.id);
            d.is_dir().then_some(d)
        }
        Engine::SpeechAnalyzer => None,
        Engine::Llm => None,
    }
}

/// Drops the queue entry first, then removes this profile's copy of a whisper or nemotron model. A SpeechAnalyzer row is refused. Nothing left to remove (a queued-but-never-downloaded id, or one the cancel removed) returns success; a floor row with only the bundled copy refuses. A download still writing the same id holds the install guard, and delete names it.
pub fn delete_asr_model_impl(app: &AppState, id: &str) -> Result<()> {
    let row = registry::row(id)
        .ok_or_else(|| AppError::Config(format!("unknown ASR model {id:?}")))?;
    if row.engine == Engine::SpeechAnalyzer {
        return Err(AppError::Config("OS-managed models can't be deleted".into()));
    }
    app.downloads.cancel(id)?;
    let models_dir = app.profile.models_dir();
    let Some(target) = profile_copy_dir(row, &models_dir) else {
        return if floor_here(row) {
            Err(AppError::Config("only the bundled copy exists; nothing to delete".into()))
        } else {
            Ok(())
        };
    };
    let _guard = providers_local::delete_guard(row.id).ok_or_else(|| {
        AppError::Config(format!(
            "{} is still installing. Cancel that first, then delete it.",
            row.display
        ))
    })?;
    match row.engine {
        Engine::Whisper => syncsafe::remove_file(&target)
            .map_err(|e| AppError::Io(format!("delete model: {e}")))?,
        Engine::Nemotron => std::fs::remove_dir_all(&target)
            .map_err(|e| AppError::Io(format!("delete model dir: {e}")))?,
        Engine::SpeechAnalyzer => unreachable!(),
        Engine::Llm => unreachable!("LLM rows live in LLM_REGISTRY"),
    }
    // Deleting the active model's last copy falls back to the resolution
    // default (floor row, else the default whisper model). A floor row that
    // still resolves from the bundle stays active.
    if app.machine_settings().asr.active.as_deref() == Some(id)
        && installed_for(row, &models_dir, providers_local::macos::sa_ready(), &BundledDirs::from_env())
            .is_none()
    {
        app.update_machine_settings(|m| m.asr.active = None)
            .map_err(|e| AppError::Io(e.to_string()))?;
    }
    Ok(())
}

/// Downloads one registry row into `<profile>/models/`. `on_phase` carries
/// byte progress and phase transitions to the queue. The SpeechAnalyzer row
/// is an OS asset fetch with no byte count; whisper and nemotron hold the
/// install guard internally, for their whole run. Blocking — call it from
/// `spawn_blocking`.
pub fn run_asr_download(
    app: &tauri::AppHandle,
    id: &str,
    cancel: &Arc<AtomicBool>,
    on_phase: Arc<dyn Fn(crate::downloads::Phase) + Send + Sync>,
) -> Result<()> {
    use providers_local::registry::Engine;
    use providers_local::{download_ggml_model_opts, download_nemotron_model_opts, DownloadOpts};
    use tauri::Manager;

    let dest_dir = app.state::<AppState>().profile.models_dir();
    let row = providers_local::registry::row(id)
        .ok_or_else(|| AppError::Config(format!("unknown ASR model {id:?}")))?;

    let progress_phase = on_phase.clone();
    let progress_cb: Box<dyn FnMut(u64, Option<u64>) + Send + 'static> =
        Box::new(move |downloaded, total| {
            progress_phase(crate::downloads::Phase::Downloading {
                pct: crate::downloads::pct(downloaded, total),
            });
        });

    let opts = DownloadOpts {
        progress: Some(progress_cb),
        cancel: Some(cancel.clone()),
        ..Default::default()
    };

    on_phase(crate::downloads::Phase::Downloading { pct: 0.0 });
    let done = match row.engine {
        Engine::Whisper => {
            let size = row
                .whisper_size()
                .ok_or_else(|| anyhow::anyhow!("whisper row id shape: {}", row.id))
                .map_err(|e| AppError::Config(e.to_string()))?;
            download_ggml_model_opts(size, &dest_dir, opts).map(|_| ())
        }
        Engine::Nemotron => {
            download_nemotron_model_opts(&dest_dir.join("nemotron"), opts).map(|_| ())
        }
        Engine::SpeechAnalyzer => providers_local::macos::sa_assets_install_blocking("")
            .map_err(|e| anyhow::anyhow!("OS speech assets: {e:#}")),
        Engine::Llm => unreachable!("LLM rows live in LLM_REGISTRY"),
    };
    done.map_err(|e| AppError::Config(format!("download model: {e:#}")))
}

#[cfg(test)]
pub(crate) mod resolve_tests {
    use super::*;
    use providers_local::registry::Engine;

    const NEMOTRON_ID: &str = providers_local::registry::NEMOTRON_ID;

    /// A sparse placeholder at an exact length: the size pin is what makes a
    /// file count as installed.
    pub(super) fn sized_file(path: &Path, len: u64) {
        std::fs::File::create(path).unwrap().set_len(len).unwrap();
    }

    pub(crate) fn whisper_file(path: PathBuf) {
        let name = path.file_name().unwrap().to_str().unwrap();
        sized_file(&path, registry::row(name).unwrap().files[0].2);
    }

    fn machine_with_active(id: Option<&str>) -> crate::settings::MachineSettings {
        let mut m = crate::settings::MachineSettings::default();
        m.asr.active = id.map(str::to_string);
        m
    }

    /// Full pinned file set — a bare dir is (correctly) not installed.
    fn install_nemotron_files(models_dir: &Path) {
        let row = registry::row(NEMOTRON_ID).unwrap();
        let d = models_dir.join("nemotron").join(NEMOTRON_ID);
        std::fs::create_dir_all(&d).unwrap();
        for (n, _, size) in row.files {
            sized_file(&d.join(n), *size);
        }
    }

    fn chain_on(
        m: &crate::settings::MachineSettings,
        models_dir: &Path,
        os: &str,
        sa_ready: bool,
    ) -> Vec<(&'static ModelRow, PathBuf)> {
        resolve_asr_chain_on(m, models_dir, os, sa_ready, &BundledDirs::none())
    }

    #[test]
    fn active_nemotron_resolves_to_model_dir() {
        let dir = tempfile::tempdir().unwrap();
        install_nemotron_files(dir.path());
        let m = machine_with_active(Some(NEMOTRON_ID));
        let (row, path) = chain_on(&m, dir.path(), "linux", false).into_iter().next().unwrap();
        assert_eq!(row.engine, Engine::Nemotron);
        assert_eq!(path, dir.path().join("nemotron").join(NEMOTRON_ID));
    }

    #[test]
    fn nothing_set_resolves_platform_floor_when_present() {
        let dir = tempfile::tempdir().unwrap();
        install_nemotron_files(dir.path());
        let m = machine_with_active(None);
        let (row, _) = chain_on(&m, dir.path(), "linux", false).into_iter().next().unwrap();
        assert_eq!(row.engine, Engine::Nemotron, "floor row is the default");
        // Off-floor platform: the installed row still resolves via the any-installed tail.
        let (row, _) = chain_on(&m, dir.path(), "macos", false).into_iter().next().unwrap();
        assert_eq!(row.engine, Engine::Nemotron);
    }

    #[test]
    fn nothing_installed_resolves_empty_chain() {
        let dir = tempfile::tempdir().unwrap();
        let m = machine_with_active(None);
        assert!(chain_on(&m, dir.path(), "linux", false).is_empty());
        assert!(chain_on(&m, dir.path(), "macos", false).is_empty());
    }

    #[test]
    fn partial_nemotron_dir_is_not_installed() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path().join("nemotron").join(NEMOTRON_ID);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("encoder.int8.onnx"), b"x").unwrap();
        let m = machine_with_active(None);
        assert!(
            chain_on(&m, dir.path(), "linux", false).is_empty(),
            "a partially deleted dir must not count as installed"
        );
    }

    #[test]
    fn bundled_dir_with_a_zero_byte_file_is_not_installed() {
        let profile = tempfile::tempdir().unwrap();
        let bundle = tempfile::tempdir().unwrap();
        let row = registry::row(NEMOTRON_ID).unwrap();
        let d = bundle.path().join(NEMOTRON_ID);
        std::fs::create_dir_all(&d).unwrap();
        for (n, _, size) in row.files {
            sized_file(&d.join(n), if *n == "encoder.int8.onnx.data" { 0 } else { *size });
        }
        let bundled = BundledDirs { whisper: None, nemotron: Some(bundle.path().to_path_buf()) };
        assert!(installed_for(row, profile.path(), false, &bundled).is_none());
        let m = machine_with_active(None);
        assert!(resolve_asr_chain_on(&m, profile.path(), "linux", false, &bundled).is_empty());

        install_nemotron_files(profile.path());
        let got = installed_for(row, &profile.path().join("x"), false, &bundled);
        assert!(got.is_none(), "profile dir is models_dir/nemotron, not this path");
        let (_, p) = resolve_asr_chain_on(&m, profile.path(), "linux", false, &bundled)
            .into_iter()
            .next()
            .expect("a repaired profile copy resolves");
        assert!(p.starts_with(profile.path()));
    }

    #[test]
    fn bundled_dir_resolves_when_profile_is_empty() {
        let profile = tempfile::tempdir().unwrap();
        let bundle = tempfile::tempdir().unwrap();
        let row = registry::row(NEMOTRON_ID).unwrap();
        let d = bundle.path().join(NEMOTRON_ID);
        std::fs::create_dir_all(&d).unwrap();
        for (n, _, size) in row.files {
            sized_file(&d.join(n), *size);
        }
        let bundled = BundledDirs {
            whisper: None,
            nemotron: Some(bundle.path().to_path_buf()),
        };
        let m = machine_with_active(None);
        let chain = resolve_asr_chain_on(&m, profile.path(), "linux", false, &bundled);
        let (r, p) = chain.first().expect("bundled floor resolves");
        assert_eq!(r.engine, Engine::Nemotron);
        assert_eq!(*p, bundle.path().join(NEMOTRON_ID));
    }

    #[test]
    fn active_whisper_resolves_from_models_dir() {
        let dir = tempfile::tempdir().unwrap();
        whisper_file(dir.path().join("ggml-small.en.bin"));
        let m = machine_with_active(Some("ggml-small.en.bin"));
        let (row, path) = chain_on(&m, dir.path(), "linux", false).into_iter().next().unwrap();
        assert_eq!(row.engine, Engine::Whisper);
        assert_eq!(path, dir.path().join("ggml-small.en.bin"));
    }

    #[test]
    fn sa_platform_default_gates_on_availability() {
        let dir = tempfile::tempdir().unwrap();
        let m = machine_with_active(None);
        // Available on the mac platform: SA is the platform default.
        let (row, _) = chain_on(&m, dir.path(), "macos", true).into_iter().next().unwrap();
        assert_eq!(row.engine, Engine::SpeechAnalyzer);
        // Unavailable: falls through to the installed default whisper row.
        whisper_file(dir.path().join("ggml-base.en.bin"));
        let (row, _) = chain_on(&m, dir.path(), "macos", false).into_iter().next().unwrap();
        assert_eq!(row.engine, Engine::Whisper);
        // Unavailable and nothing installed: no engine.
        let empty = tempfile::tempdir().unwrap();
        assert!(chain_on(&m, empty.path(), "macos", false).is_empty());
    }

    #[test]
    fn explicit_sa_active_also_gates_on_availability() {
        let dir = tempfile::tempdir().unwrap();
        whisper_file(dir.path().join("ggml-base.en.bin"));
        let m = machine_with_active(Some("speechanalyzer"));
        let (row, _) = chain_on(&m, dir.path(), "macos", true).into_iter().next().unwrap();
        assert_eq!(row.engine, Engine::SpeechAnalyzer);
        let (row, _) = chain_on(&m, dir.path(), "macos", false).into_iter().next().unwrap();
        assert_eq!(row.engine, Engine::Whisper, "unavailable SA never resolves");
    }

    #[test]
    fn any_installed_row_resolves_when_default_is_absent() {
        // Downloaded-but-never-activated model: the chain still resolves.
        let dir = tempfile::tempdir().unwrap();
        whisper_file(dir.path().join("ggml-small.en.bin"));
        let m = machine_with_active(None);
        let (row, path) = chain_on(&m, dir.path(), "macos", false).into_iter().next().unwrap();
        assert_eq!(row.engine, Engine::Whisper);
        assert_eq!(path, dir.path().join("ggml-small.en.bin"));
    }

    #[test]
    fn chain_orders_active_floor_default_then_rest() {
        let dir = tempfile::tempdir().unwrap();
        install_nemotron_files(dir.path());
        whisper_file(dir.path().join("ggml-base.en.bin"));
        whisper_file(dir.path().join("ggml-small.en.bin"));
        let m = machine_with_active(Some("ggml-small.en.bin"));
        let ids: Vec<&str> =
            chain_on(&m, dir.path(), "linux", false).iter().map(|(r, _)| r.id).collect();
        assert_eq!(ids, ["ggml-small.en.bin", NEMOTRON_ID, "ggml-base.en.bin"]);
        // Engine-specific consumers take their first row of that engine.
        assert_eq!(
            resolve_asr_chain_on(&m, dir.path(), "linux", false, &BundledDirs::none())
                .into_iter()
                .find(|(r, _)| r.engine == Engine::Whisper)
                .map(|(r, _)| r.id),
            Some("ggml-small.en.bin"),
            "active whisper row wins over the default"
        );
    }

    #[test]
    fn nemotron_fallback_resolves_when_active_is_whisper() {
        let dir = tempfile::tempdir().unwrap();
        install_nemotron_files(dir.path());
        let m = machine_with_active(Some("ggml-base.en.bin"));
        assert_eq!(
            chain_on(&m, dir.path(), "linux", false)
                .into_iter()
                .find(|(r, _)| r.engine == Engine::Nemotron)
                .map(|(_, p)| p),
            Some(dir.path().join("nemotron").join(NEMOTRON_ID))
        );
    }

    #[test]
    fn whisper_fallback_prefers_active_then_default() {
        let dir = tempfile::tempdir().unwrap();
        whisper_file(dir.path().join("ggml-base.en.bin"));
        // Active is nemotron (not installed): the whisper fallback still
        // finds base.en.
        let m = machine_with_active(Some(NEMOTRON_ID));
        assert_eq!(
            chain_on(&m, dir.path(), "linux", false)
                .into_iter()
                .find(|(r, _)| r.engine == Engine::Whisper)
                .map(|(_, p)| p),
            Some(dir.path().join("ggml-base.en.bin"))
        );
    }
}

#[cfg(test)]
mod manager_tests {
    use super::resolve_tests::{sized_file, whisper_file};
    use super::*;

    fn test_app() -> (AppState, tempfile::TempDir) {
        std::env::remove_var("DAISY_WHISPER_MODEL_DIR");
        std::env::remove_var("DAISY_NEMOTRON_MODEL_DIR");
        let tmp = tempfile::tempdir().unwrap();
        let profile = crate::profile::ProfileDir::at(tmp.path()).unwrap();
        (AppState::new(profile), tmp)
    }

    fn install_nemotron(app: &AppState) -> &'static str {
        let id = providers_local::registry::NEMOTRON_ID;
        let row = registry::row(id).unwrap();
        let d = app.profile.models_dir().join("nemotron").join(id);
        std::fs::create_dir_all(&d).unwrap();
        for (n, _, size) in row.files {
            sized_file(&d.join(n), *size);
        }
        id
    }

    fn install_whisper(app: &AppState, id: &str) {
        let dir = app.profile.models_dir();
        std::fs::create_dir_all(&dir).unwrap();
        whisper_file(dir.join(id));
    }

    #[test]
    fn list_marks_floor_row_bundled_and_active_by_default() {
        let (app, _tmp) = test_app();
        let id = install_nemotron(&app);
        let rows = list_asr_models_impl(&app).unwrap();
        let pk = rows.iter().find(|r| r.id == id).unwrap();
        if cfg!(any(target_os = "windows", target_os = "linux")) {
            assert!(pk.bundled_floor);
            assert!(pk.active, "floor row is the default active model");
        }
        assert!(pk.installed);
        assert!(rows.iter().filter(|r| r.active).count() <= 1);
    }

    #[test]
    fn set_active_persists_and_list_reflects_it() {
        let (app, _tmp) = test_app();
        install_whisper(&app, "ggml-small.en.bin");
        set_active_asr_model_impl(&app, "ggml-small.en.bin").unwrap();
        assert_eq!(
            app.machine_settings().asr.active.as_deref(),
            Some("ggml-small.en.bin")
        );
        let rows = list_asr_models_impl(&app).unwrap();
        assert!(rows.iter().find(|r| r.id == "ggml-small.en.bin").unwrap().active);
    }

    #[test]
    fn set_active_rejects_uninstalled() {
        let (app, _tmp) = test_app();
        assert!(set_active_asr_model_impl(&app, "ggml-small.en.bin").is_err());
        assert!(set_active_asr_model_impl(&app, "no-such-model").is_err());
    }

    #[test]
    fn delete_removes_profile_copy_even_for_floor_rows() {
        let (app, _tmp) = test_app();
        let id = install_nemotron(&app);
        // The profile copy (which shadows any bundled copy) is deletable.
        delete_asr_model_impl(&app, id).unwrap();
        assert!(!app.profile.models_dir().join("nemotron").join(id).exists());
        // A second delete: the floor platforms refuse (the bundled copy
        // stays); elsewhere nothing is left and it is a successful no-op.
        let again = delete_asr_model_impl(&app, id);
        if cfg!(any(target_os = "windows", target_os = "linux")) {
            assert!(again.is_err());
        } else {
            again.unwrap();
        }
    }

    #[test]
    fn delete_active_models_last_copy_resets_active() {
        let (app, _tmp) = test_app();
        install_whisper(&app, "ggml-small.en.bin");
        set_active_asr_model_impl(&app, "ggml-small.en.bin").unwrap();
        delete_asr_model_impl(&app, "ggml-small.en.bin").unwrap();
        assert!(!app.profile.models_dir().join("ggml-small.en.bin").exists());
        assert_eq!(app.machine_settings().asr.active, None);
    }

    #[test]
    fn list_reports_profile_copy() {
        let (app, _tmp) = test_app();
        install_whisper(&app, "ggml-small.en.bin");
        let rows = list_asr_models_impl(&app).unwrap();
        assert!(rows.iter().find(|r| r.id == "ggml-small.en.bin").unwrap().profile_copy);
        assert!(!rows.iter().find(|r| r.id == "ggml-base.en.bin").unwrap().profile_copy);
    }

    #[test]
    fn delete_is_refused_while_a_download_holds_the_model() {
        let (app, _tmp) = test_app();
        install_whisper(&app, "ggml-base.en.bin");
        let _downloading = providers_local::install_guard("ggml-base.en.bin");
        let e = delete_asr_model_impl(&app, "ggml-base.en.bin").unwrap_err();
        assert!(e.to_string().contains("still installing"), "{e}");
        assert!(
            app.profile.models_dir().join("ggml-base.en.bin").is_file(),
            "delete removed the file the download is writing"
        );
    }

    #[test]
    fn delete_active_non_floor_resets_active() {
        let (app, _tmp) = test_app();
        install_whisper(&app, "ggml-small.en.bin");
        set_active_asr_model_impl(&app, "ggml-small.en.bin").unwrap();
        delete_asr_model_impl(&app, "ggml-small.en.bin").unwrap();
        assert_eq!(app.machine_settings().asr.active, None);
        assert!(!app.profile.models_dir().join("ggml-small.en.bin").exists());
    }
}
