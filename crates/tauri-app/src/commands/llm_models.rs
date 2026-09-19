// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! The built-in summary model on this machine: capability gate, worker and
//! weight paths, install state, list / download / delete.
//!
//! Layout: `<profile>/models/llm/<id>/<shards>`; the worker binary sits next
//! to the app executable (Tauri sidecar) or at `DAISY_LLM_BIN`.

use crate::error::{AppError, Result};
use crate::state::{AppState, ProviderId};
use daisy_llm_protocol::Probe;
use providers_local::registry::{self, ModelRow};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use summarize::local::LOCAL_MODEL_ID;

/// How long `daisy-llm --probe` may take before it is killed.
const PROBE_TIMEOUT: Duration = Duration::from_secs(30);
/// How often the probe and the warm-up check for exit, cancellation and the
/// deadline.
const POLL: Duration = Duration::from_millis(100);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    Fast,
    #[default]
    Unsupported,
}

impl Capability {
    pub fn line(self) -> &'static str {
        match self {
            Capability::Fast => "Fast on this machine.",
            Capability::Unsupported => "Needs a graphics card with 6 GB of memory or more.",
        }
    }
}

/// The call to action shown wherever the built-in model needs a download.
pub(crate) fn download_hint() -> String {
    format!("Download the built-in model in {}.", crate::settings::providers_hint())
}

#[derive(Debug, Clone, Copy, Default)]
pub struct LocalStatus {
    pub installed: bool,
    pub capability: Capability,
}

#[derive(Debug, Clone)]
pub struct LocalPaths {
    pub worker: PathBuf,
    pub model: PathBuf,
}

const GPU_MIN_BYTES: u64 = 6_000_000_000;

/// The gate. A GPU with room for the weights → fast; anything else →
/// unsupported. An integrated GPU does not count: decoding on one is slower
/// than the summary is worth, and its driver drops the device under load.
pub fn gate(p: &Probe) -> Capability {
    if p.devices.iter().any(|d| d.kind == "gpu" && d.memory_bytes >= GPU_MIN_BYTES) {
        Capability::Fast
    } else {
        Capability::Unsupported
    }
}

pub fn row() -> &'static ModelRow {
    registry::llm_row(LOCAL_MODEL_ID).expect("registry has the built-in row")
}

pub fn llm_dir(models_dir: &Path) -> PathBuf {
    models_dir.join("llm")
}

/// Where a download in flight puts its shards, until the verified set is
/// renamed into place.
pub fn staging_dir(llm_dir: &Path, id: &str) -> PathBuf {
    llm_dir.join(format!("{id}.staging"))
}

/// Bytes sitting in a directory, one level deep. Zero when it is absent.
fn dir_bytes(dir: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else { return 0 };
    entries
        .filter_map(|e| e.ok())
        .filter_map(|e| e.metadata().ok())
        .filter(|m| m.is_file())
        .map(|m| m.len())
        .sum()
}

pub fn installed(models_dir: &Path) -> bool {
    row().installed_in(&llm_dir(models_dir))
}

pub fn model_path(models_dir: &Path) -> Option<PathBuf> {
    installed(models_dir).then(|| row().engine_path(&llm_dir(models_dir)).expect("llm rows have a path"))
}

/// The sidecar next to the app binary, or `DAISY_LLM_BIN`.
pub fn worker_path() -> Result<PathBuf> {
    if let Some(p) = std::env::var_os("DAISY_LLM_BIN") {
        return Ok(PathBuf::from(p));
    }
    let exe = std::env::current_exe().map_err(|e| AppError::Config(format!("current exe: {e}")))?;
    let dir = exe.parent().ok_or_else(|| AppError::Config("app binary has no parent dir".into()))?;
    let p = dir.join(if cfg!(windows) { "daisy-llm.exe" } else { "daisy-llm" });
    if p.is_file() {
        Ok(p)
    } else {
        Err(AppError::Config(format!("built-in model worker missing at {}", p.display())))
    }
}

/// Runs `daisy-llm --probe` and parses its last non-empty output line. The
/// child is killed when it outlives [`PROBE_TIMEOUT`].
pub fn run_probe(worker: &Path) -> Result<Probe> {
    use std::io::Read;
    let mut child = daisy_llm_protocol::no_window(std::process::Command::new(worker))
        .arg("--probe")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| AppError::Config(format!("probe: {e}")))?;
    let mut out = child.stdout.take().expect("piped stdout");
    let mut err = child.stderr.take().expect("piped stderr");
    let drain = |p: &mut dyn Read| {
        let mut s = Vec::new();
        let _ = p.read_to_end(&mut s);
        String::from_utf8_lossy(&s).into_owned()
    };
    let out_thread = std::thread::spawn(move || drain(&mut out));
    let err_thread = std::thread::spawn(move || drain(&mut err));
    let deadline = Instant::now() + PROBE_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) => {}
            Err(e) => return Err(AppError::Config(format!("probe: {e}"))),
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(AppError::Config(format!(
                "probe gave up after {} s",
                PROBE_TIMEOUT.as_secs()
            )));
        }
        std::thread::sleep(POLL);
    };
    let stdout = out_thread.join().unwrap_or_default();
    let stderr = err_thread.join().unwrap_or_default();
    if !status.success() {
        return Err(AppError::Config(format!("probe exited {status}: {stderr}")));
    }
    let line = stdout
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .ok_or_else(|| AppError::Config("probe printed nothing".into()))?;
    serde_json::from_str(line).map_err(|e| AppError::Config(format!("probe output: {e}")))
}

/// Runs the probe and gates what it reports. [`AppState::llm_capability`]
/// keeps the answer; an error there reads as unsupported and is probed again
/// on the next call.
pub(crate) fn probe_capability() -> Result<Capability> {
    let started = Instant::now();
    log::info!("built-in model probe: started");
    match worker_path().and_then(|w| run_probe(&w)) {
        Ok(p) => {
            let c = gate(&p);
            log::info!(
                "built-in model probe: {:?} → {c:?} in {} ms",
                p.devices.iter().map(|d| format!("{} {} {} {}B", d.kind, d.name, d.backend, d.memory_bytes)).collect::<Vec<_>>(),
                started.elapsed().as_millis()
            );
            Ok(c)
        }
        Err(e) => {
            log::warn!("built-in model probe failed after {} ms: {e}", started.elapsed().as_millis());
            Err(e)
        }
    }
}

pub fn local_status(app: &AppState) -> LocalStatus {
    LocalStatus { installed: installed(&app.profile.models_dir()), capability: app.llm_capability() }
}

/// Both paths a call needs, or the reason it cannot run.
pub fn local_paths(app: &AppState) -> Result<LocalPaths> {
    if app.llm_capability() == Capability::Unsupported {
        return Err(AppError::Config(format!("This computer can't run the built-in model. {}", Capability::Unsupported.line())));
    }
    let model = model_path(&app.profile.models_dir()).ok_or_else(|| {
        let hint = download_hint();
        AppError::Config(format!("Built-in model isn't downloaded. {hint}"))
    })?;
    Ok(LocalPaths { worker: worker_path()?, model })
}

#[derive(Debug, Clone, Serialize)]
pub struct LlmModelInfo {
    pub id: &'static str,
    pub display: &'static str,
    pub installed: bool,
    pub bytes: u64,
    pub license: &'static str,
    pub license_url: &'static str,
    pub capability: Capability,
    pub capability_line: &'static str,
    /// Bytes a stopped download left staged on disk. The row offers to
    /// reclaim them.
    pub partial_bytes: u64,
}

pub fn list_llm_models_impl(app: &AppState) -> Vec<LlmModelInfo> {
    let models_dir = app.profile.models_dir();
    let dir = llm_dir(&models_dir);
    let c = app.llm_capability();
    registry::LLM_REGISTRY
        .iter()
        .map(|r| LlmModelInfo {
            id: r.id,
            display: r.display,
            installed: r.installed_in(&dir),
            bytes: r.bytes,
            license: r.license,
            license_url: r.license_url,
            capability: c,
            capability_line: c.line(),
            partial_bytes: dir_bytes(&staging_dir(&dir, r.id)),
        })
        .collect()
}

/// Drops the queue entry first, then removes the model dir and anything a
/// stopped download left staged, and clears a `local` default provider.
/// Nothing left to remove — a queued-but-never-downloaded id, or one the
/// cancel above removed — returns success. A download still writing the
/// same id past the cancel holds the install guard, and delete names it
/// instead of waiting.
pub fn delete_llm_model_impl(app: &AppState, id: &str) -> Result<()> {
    let r = registry::llm_row(id).ok_or_else(|| AppError::Config(format!("unknown model {id:?}")))?;
    app.downloads.cancel(id)?;
    let base = llm_dir(&app.profile.models_dir());
    let dir = base.join(r.id);
    let stage = staging_dir(&base, r.id);
    if !dir.is_dir() && !stage.is_dir() {
        return Ok(());
    }
    let _guard = providers_local::delete_guard(r.id).ok_or_else(|| {
        AppError::Config(format!(
            "{} is still installing. Cancel that first, then delete it.",
            r.display
        ))
    })?;
    app.release_local_chat();
    if dir.is_dir() {
        std::fs::remove_dir_all(&dir).map_err(|e| AppError::Io(format!("delete model dir: {e}")))?;
    }
    if stage.is_dir() {
        std::fs::remove_dir_all(&stage)
            .map_err(|e| AppError::Io(format!("delete staged download: {e}")))?;
    }
    if app.ai_provider() == Some(ProviderId::Local) {
        app.set_ai_provider(None).map_err(|e| AppError::Io(format!("save settings: {e}")))?;
    }
    Ok(())
}

/// After a download: the built-in model becomes this machine's provider when
/// none is set.
pub fn set_default_provider_if_unset(app: &AppState) -> Result<()> {
    if app.ai_provider().is_none() {
        app.set_ai_provider(Some(ProviderId::Local))
            .map_err(|e| AppError::Io(format!("save settings: {e}")))?;
    }
    Ok(())
}

/// Loads the weights into the resident worker every later call reuses, by
/// putting one short request through it. The model id is held against delete
/// for the length of the warm-up. Setting `cancel` kills that worker and
/// returns; the weights stay installed and the next call spawns a fresh
/// worker.
fn warm(app: &AppState, worker: &Path, model: &Path, cancel: &Arc<AtomicBool>) -> Result<()> {
    let _installing = providers_local::install_guard(row().id);
    let chat = app.local_chat(worker, model);
    let done = Arc::new(AtomicBool::new(false));
    let watcher = std::thread::spawn({
        let (chat, done, cancel) = (chat.clone(), done.clone(), cancel.clone());
        move || {
            while !done.load(Ordering::SeqCst) {
                if cancel.load(Ordering::SeqCst) {
                    chat.release();
                }
                std::thread::sleep(POLL);
            }
        }
    });
    let started = Instant::now();
    let reply = chat.complete_text("", &[("user".into(), "Reply with the word ready.".into())], 1);
    done.store(true, Ordering::SeqCst);
    let _ = watcher.join();
    let ms = started.elapsed().as_millis();
    if cancel.load(Ordering::SeqCst) {
        log::info!("built-in model warm-up cancelled after {ms} ms");
        return Ok(());
    }
    reply.map_err(|e| AppError::Config(format!("warm-up: {e}")))?;
    log::info!("built-in model warm-up finished in {ms} ms");
    Ok(())
}

/// Warms the worker on a freshly installed model, then makes the built-in
/// model the provider when none is set.
pub fn warm_and_select(app: &AppState, worker: &Path, model: &Path, cancel: &Arc<AtomicBool>) -> Result<()> {
    warm(app, worker, model, cancel)?;
    set_default_provider_if_unset(app)
}

/// Downloads the shards into `<profile>/models/llm/<id>/`, loads them into the
/// resident worker and makes the built-in model the provider when none is
/// set. `on_phase` carries byte progress and phase transitions to the queue.
/// Blocking — call it from `spawn_blocking`.
pub fn run_llm_download(
    app: &tauri::AppHandle,
    id: &str,
    cancel: &Arc<AtomicBool>,
    on_phase: Arc<dyn Fn(crate::downloads::Phase) + Send + Sync>,
) -> Result<()> {
    use providers_local::{download_llm_model_opts, DownloadOpts};
    use tauri::Manager;

    let state = app.state::<AppState>();
    let llm_parent = llm_dir(&state.profile.models_dir());

    let progress_phase = on_phase.clone();
    let progress_cb: Box<dyn FnMut(u64, Option<u64>) + Send + 'static> =
        Box::new(move |downloaded, total| {
            progress_phase(crate::downloads::Phase::Downloading {
                pct: crate::downloads::pct(downloaded, total),
            });
        });

    on_phase(crate::downloads::Phase::Downloading { pct: 0.0 });
    state.release_local_chat();
    let opts = DownloadOpts {
        progress: Some(progress_cb),
        cancel: Some(cancel.clone()),
        ..Default::default()
    };
    let model = download_llm_model_opts(id, &llm_parent, opts)
        .map_err(|e| AppError::Config(format!("download model: {e:#}")))?;
    on_phase(crate::downloads::Phase::Warming);
    let worker = worker_path()?;
    warm_and_select(&state, &worker, &model, cancel)
}

#[cfg(test)]
mod gate_tests {
    use super::*;
    use daisy_llm_protocol::Device;

    fn probe(devices: &[(&str, u64)], ram: u64) -> Probe {
        Probe {
            devices: devices.iter().map(|(k, m)| Device { name: k.to_string(), backend: "x".into(), kind: k.to_string(), memory_bytes: *m }).collect(),
            ram_bytes: ram,
        }
    }

    #[test]
    fn gate_table() {
        let gib = 1u64 << 30;
        assert_eq!(gate(&probe(&[("gpu", 8 * gib), ("cpu", 0)], 32 * gib)), Capability::Fast);
        assert_eq!(gate(&probe(&[("gpu", 6 * gib)], 8 * gib)), Capability::Fast);
        assert_eq!(gate(&probe(&[("gpu", 4 * gib)], 32 * gib)), Capability::Unsupported);
        assert_eq!(gate(&probe(&[("igpu", 16 * gib)], 32 * gib)), Capability::Unsupported);
        assert_eq!(gate(&probe(&[("cpu", 0)], 16 * gib)), Capability::Unsupported);
        assert_eq!(gate(&probe(&[("igpu", 8 * gib), ("cpu", 0)], 8 * gib)), Capability::Unsupported);
        assert_eq!(gate(&probe(&[], 0)), Capability::Unsupported);
    }

    #[test]
    fn worker_path_honors_the_env_override() {
        std::env::set_var("DAISY_LLM_BIN", "/tmp/whatever-llm");
        assert_eq!(worker_path().unwrap(), PathBuf::from("/tmp/whatever-llm"));
        std::env::remove_var("DAISY_LLM_BIN");
    }
}
