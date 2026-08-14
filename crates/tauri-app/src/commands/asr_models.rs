//! ASR model management over the engine-typed registry: list rows, switch the
//! active model, delete installed ones, and resolve the active engine+model.
//! The download itself lives in `main.rs` (`download_asr_model`).
//!
//! Layout: whisper models are `<profile>/models/ggml-<size>.bin` files;
//! parakeet models are `<profile>/models/parakeet/<id>/` dirs. Platform-floor
//! rows ship in the app bundle (resource dirs found via the model-dir env
//! vars); their bundled copies cannot be deleted. `resolve_asr_chain` is the
//! one resolution order every consumer derives from.

use crate::error::{AppError, Result};
use crate::state::AppState;
use providers_local::registry::{self, Engine, ModelRow};
use serde::Serialize;
use std::path::{Path, PathBuf};

/// Bundled resource dirs (the model-dir env vars set at startup), captured as
/// a value so resolution is testable without touching process env.
pub struct BundledDirs {
    pub whisper: Option<PathBuf>,
    pub parakeet: Option<PathBuf>,
}

impl BundledDirs {
    pub fn from_env() -> Self {
        Self {
            whisper: std::env::var_os("DAISY_WHISPER_MODEL_DIR").map(PathBuf::from),
            parakeet: std::env::var_os("DAISY_PARAKEET_MODEL_DIR").map(PathBuf::from),
        }
    }

    #[cfg(test)]
    fn none() -> Self {
        Self { whisper: None, parakeet: None }
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

/// The chain with platform, SpeechAnalyzer availability, and bundled dirs as
/// inputs, so the mac default and its fallbacks are unit-testable off-mac.
/// Availability is a runtime answer, never a compile-time platform gate.
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
/// file or parakeet dir; empty for the OS-managed SpeechAnalyzer.
pub fn resolve_active_asr(
    machine: &crate::settings::MachineSettings,
    models_dir: &Path,
) -> Option<(&'static ModelRow, PathBuf)> {
    resolve_asr_chain(machine, models_dir).into_iter().next()
}

/// "Installed" for a registry row. File-backed rows resolve on disk — the
/// profile copy first, else the bundled resource dir — requiring the full
/// pinned file set (a partially deleted dir is not installed; the Download
/// button repairs it). SpeechAnalyzer is OS-managed — installed = usable now
/// (capability AND system-locale assets), and its path is empty.
fn installed_for(
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
        .find(|b| row.installed_in(b))
        .and_then(|b| row.engine_path(&b))
}

/// Lookup bases for a file-backed row, in precedence order: the profile
/// models dir, then the bundled resource dir.
fn bases_for(row: &ModelRow, models_dir: &Path, bundled: &BundledDirs) -> [Option<PathBuf>; 2] {
    match row.engine {
        Engine::Whisper => [Some(models_dir.to_path_buf()), bundled.whisper.clone()],
        Engine::Parakeet => [Some(models_dir.join("parakeet")), bundled.parakeet.clone()],
        Engine::SpeechAnalyzer => [None, None],
    }
}

/// One registry row projected for the frontend model list.
#[derive(Debug, Serialize)]
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
        })
        .collect();
    Ok(out)
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
/// the ggml file, or the parakeet model dir. This is what Delete removes —
/// a bundled copy (floor row) is untouched, so deleting a floor row's
/// profile copy reverts it to the bundle.
fn profile_copy_dir(row: &ModelRow, models_dir: &Path) -> Option<PathBuf> {
    match row.engine {
        Engine::Whisper => {
            let p = models_dir.join(row.id);
            p.is_file().then_some(p)
        }
        Engine::Parakeet => {
            let d = models_dir.join("parakeet").join(row.id);
            d.is_dir().then_some(d)
        }
        Engine::SpeechAnalyzer => None,
    }
}

pub fn delete_asr_model_impl(app: &AppState, id: &str) -> Result<()> {
    let row = registry::row(id)
        .ok_or_else(|| AppError::Config(format!("unknown ASR model {id:?}")))?;
    if row.engine == Engine::SpeechAnalyzer {
        return Err(AppError::Config("OS-managed models can't be deleted".into()));
    }
    let models_dir = app.profile.models_dir();
    let Some(target) = profile_copy_dir(row, &models_dir) else {
        return Err(AppError::Config(if floor_here(row) {
            "only the bundled copy exists; nothing to delete".into()
        } else {
            format!("model {id} is not installed")
        }));
    };
    match row.engine {
        Engine::Whisper => syncsafe::remove_file(&target)
            .map_err(|e| AppError::Io(format!("delete model: {e}")))?,
        Engine::Parakeet => std::fs::remove_dir_all(&target)
            .map_err(|e| AppError::Io(format!("delete model dir: {e}")))?,
        Engine::SpeechAnalyzer => unreachable!(),
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

#[cfg(test)]
mod resolve_tests {
    use super::*;
    use providers_local::registry::Engine;

    const PARAKEET_ID: &str = "parakeet-tdt-0.6b-v2-int8";

    fn machine_with_active(id: Option<&str>) -> crate::settings::MachineSettings {
        let mut m = crate::settings::MachineSettings::default();
        m.asr.active = id.map(str::to_string);
        m
    }

    /// Full pinned file set — a bare dir is (correctly) not installed.
    fn install_parakeet_files(models_dir: &Path) {
        let row = registry::row(PARAKEET_ID).unwrap();
        let d = models_dir.join("parakeet").join(PARAKEET_ID);
        std::fs::create_dir_all(&d).unwrap();
        for (n, _) in row.files {
            std::fs::write(d.join(n), b"x").unwrap();
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
    fn active_parakeet_resolves_to_model_dir() {
        let dir = tempfile::tempdir().unwrap();
        install_parakeet_files(dir.path());
        let m = machine_with_active(Some(PARAKEET_ID));
        let (row, path) = chain_on(&m, dir.path(), "linux", false).into_iter().next().unwrap();
        assert_eq!(row.engine, Engine::Parakeet);
        assert_eq!(path, dir.path().join("parakeet").join(PARAKEET_ID));
    }

    #[test]
    fn nothing_set_resolves_platform_floor_when_present() {
        let dir = tempfile::tempdir().unwrap();
        install_parakeet_files(dir.path());
        let m = machine_with_active(None);
        let (row, _) = chain_on(&m, dir.path(), "linux", false).into_iter().next().unwrap();
        assert_eq!(row.engine, Engine::Parakeet, "floor row is the default");
        // Off-floor platform: the installed row still resolves via the
        // any-installed tail (record-only banner stays in lockstep with the
        // engines live dispatch would actually run).
        let (row, _) = chain_on(&m, dir.path(), "macos", false).into_iter().next().unwrap();
        assert_eq!(row.engine, Engine::Parakeet);
    }

    #[test]
    fn nothing_installed_resolves_empty_chain() {
        let dir = tempfile::tempdir().unwrap();
        let m = machine_with_active(None);
        assert!(chain_on(&m, dir.path(), "linux", false).is_empty());
        assert!(chain_on(&m, dir.path(), "macos", false).is_empty());
    }

    #[test]
    fn partial_parakeet_dir_is_not_installed() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path().join("parakeet").join(PARAKEET_ID);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("encoder.int8.onnx"), b"x").unwrap();
        let m = machine_with_active(None);
        assert!(
            chain_on(&m, dir.path(), "linux", false).is_empty(),
            "a partially deleted dir must not count as installed"
        );
    }

    #[test]
    fn bundled_dir_resolves_when_profile_is_empty() {
        let profile = tempfile::tempdir().unwrap();
        let bundle = tempfile::tempdir().unwrap();
        let row = registry::row(PARAKEET_ID).unwrap();
        let d = bundle.path().join(PARAKEET_ID);
        std::fs::create_dir_all(&d).unwrap();
        for (n, _) in row.files {
            std::fs::write(d.join(n), b"x").unwrap();
        }
        let bundled = BundledDirs {
            whisper: None,
            parakeet: Some(bundle.path().to_path_buf()),
        };
        let m = machine_with_active(None);
        let chain = resolve_asr_chain_on(&m, profile.path(), "linux", false, &bundled);
        let (r, p) = chain.first().expect("bundled floor resolves");
        assert_eq!(r.engine, Engine::Parakeet);
        assert_eq!(*p, bundle.path().join(PARAKEET_ID));
    }

    #[test]
    fn active_whisper_resolves_from_models_dir() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("ggml-small.en.bin"), b"x").unwrap();
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
        std::fs::write(dir.path().join("ggml-base.en.bin"), b"x").unwrap();
        let (row, _) = chain_on(&m, dir.path(), "macos", false).into_iter().next().unwrap();
        assert_eq!(row.engine, Engine::Whisper);
        // Unavailable and nothing installed: no engine.
        let empty = tempfile::tempdir().unwrap();
        assert!(chain_on(&m, empty.path(), "macos", false).is_empty());
    }

    #[test]
    fn explicit_sa_active_also_gates_on_availability() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("ggml-base.en.bin"), b"x").unwrap();
        let m = machine_with_active(Some("speechanalyzer"));
        let (row, _) = chain_on(&m, dir.path(), "macos", true).into_iter().next().unwrap();
        assert_eq!(row.engine, Engine::SpeechAnalyzer);
        let (row, _) = chain_on(&m, dir.path(), "macos", false).into_iter().next().unwrap();
        assert_eq!(row.engine, Engine::Whisper, "unavailable SA never resolves");
    }

    #[test]
    fn any_installed_row_resolves_when_default_is_absent() {
        // Downloaded-but-never-activated model (e.g. small.en on a mac
        // without SA assets): the chain must still resolve, or
        // asr_model_available says "record-only" while build_live_mode's
        // engine fallbacks transcribe anyway.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("ggml-small.en.bin"), b"x").unwrap();
        let m = machine_with_active(None);
        let (row, path) = chain_on(&m, dir.path(), "macos", false).into_iter().next().unwrap();
        assert_eq!(row.engine, Engine::Whisper);
        assert_eq!(path, dir.path().join("ggml-small.en.bin"));
    }

    #[test]
    fn chain_orders_active_floor_default_then_rest() {
        let dir = tempfile::tempdir().unwrap();
        install_parakeet_files(dir.path());
        std::fs::write(dir.path().join("ggml-base.en.bin"), b"x").unwrap();
        std::fs::write(dir.path().join("ggml-small.en.bin"), b"x").unwrap();
        let m = machine_with_active(Some("ggml-small.en.bin"));
        let ids: Vec<&str> =
            chain_on(&m, dir.path(), "linux", false).iter().map(|(r, _)| r.id).collect();
        assert_eq!(ids, ["ggml-small.en.bin", PARAKEET_ID, "ggml-base.en.bin"]);
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
    fn parakeet_fallback_resolves_when_active_is_whisper() {
        let dir = tempfile::tempdir().unwrap();
        install_parakeet_files(dir.path());
        let m = machine_with_active(Some("ggml-base.en.bin"));
        assert_eq!(
            chain_on(&m, dir.path(), "linux", false)
                .into_iter()
                .find(|(r, _)| r.engine == Engine::Parakeet)
                .map(|(_, p)| p),
            Some(dir.path().join("parakeet").join(PARAKEET_ID))
        );
    }

    #[test]
    fn whisper_fallback_prefers_active_then_default() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("ggml-base.en.bin"), b"x").unwrap();
        // Active is parakeet (not installed): the whisper fallback still
        // finds base.en.
        let m = machine_with_active(Some(PARAKEET_ID));
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
    use super::*;

    fn test_app() -> (AppState, tempfile::TempDir) {
        std::env::remove_var("DAISY_WHISPER_MODEL_DIR");
        std::env::remove_var("DAISY_PARAKEET_MODEL_DIR");
        let tmp = tempfile::tempdir().unwrap();
        let profile = crate::profile::ProfileDir::at(tmp.path()).unwrap();
        (AppState::new(profile), tmp)
    }

    fn install_parakeet(app: &AppState) -> &'static str {
        let id = "parakeet-tdt-0.6b-v2-int8";
        let row = registry::row(id).unwrap();
        let d = app.profile.models_dir().join("parakeet").join(id);
        std::fs::create_dir_all(&d).unwrap();
        for (n, _) in row.files {
            std::fs::write(d.join(n), b"x").unwrap();
        }
        id
    }

    fn install_whisper(app: &AppState, id: &str) {
        let dir = app.profile.models_dir();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(id), b"x").unwrap();
    }

    #[test]
    fn list_marks_floor_row_bundled_and_active_by_default() {
        let (app, _tmp) = test_app();
        let id = install_parakeet(&app);
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
        let id = install_parakeet(&app);
        // The profile copy (which shadows any bundled copy) is deletable.
        delete_asr_model_impl(&app, id).unwrap();
        assert!(!app.profile.models_dir().join("parakeet").join(id).exists());
        // Nothing left to delete: error rather than silent success.
        assert!(delete_asr_model_impl(&app, id).is_err());
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
    fn delete_active_non_floor_resets_active() {
        let (app, _tmp) = test_app();
        install_whisper(&app, "ggml-small.en.bin");
        set_active_asr_model_impl(&app, "ggml-small.en.bin").unwrap();
        delete_asr_model_impl(&app, "ggml-small.en.bin").unwrap();
        assert_eq!(app.machine_settings().asr.active, None);
        assert!(!app.profile.models_dir().join("ggml-small.en.bin").exists());
    }
}
