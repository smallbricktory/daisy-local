// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Daisy's built-in summary model on this machine, and the one owner of its
//! lifecycle: whether this computer can run it, the worker that holds its
//! weights, the warm-up after an install, its selection as the AI provider,
//! stop and delete. The files on disk say whether it is installed or partly
//! downloaded; [`BuiltinModel`] holds the rest.
//!
//! Layout: `<profile>/models/llm/<id>/<shards>`; the worker binary sits next
//! to the app executable (Tauri sidecar) or at `DAISY_LLM_BIN`.
//!
//! Every inference reaches the worker through [`chat`], which refuses while
//! the vault is locked, on a machine that cannot run the model, while the
//! weights are missing, and while a delete is under way. [`stop`] ends the
//! worker and closes the chat any caller still holds.

use crate::commands::summary::SummaryProviderStatus;
use crate::error::{AppError, Result};
use crate::state::{AppState, ProviderId, VaultState};
use daisy_llm_protocol::Probe;
use providers_local::registry::{self, ModelRow};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};
use summarize::local::{LocalChat, LOCAL_CONTEXT_TOKENS, LOCAL_MODEL_ID};

/// How long `daisy-llm --probe` may take before it is killed.
const PROBE_TIMEOUT: Duration = Duration::from_secs(30);
/// How often the probe checks for exit and the deadline.
const POLL: Duration = Duration::from_millis(100);
/// Free VRAM a GPU needs for the weights.
const GPU_MIN_BYTES: u64 = 6_000_000_000;
/// The event every window hears when [`status`] may have changed.
pub const CHANGED_EVENT: &str = "builtin-model:changed";

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

/// The built-in model's state held in memory. Managed on [`AppState`].
#[derive(Default)]
pub struct BuiltinModel {
    /// What the probe found. A failed probe leaves it empty, so the next call probes again. Kept apart from `inner`.
    capability: Mutex<Option<Capability>>,
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    /// The one chat every call site shares, with the resident worker.
    chat: Option<Arc<LocalChat>>,
    /// A warm-up request is running.
    warming: bool,
    /// A delete is under way: nothing gets a chat and nothing is selected.
    deleting: bool,
    /// The last warm-up's error, until a warm-up succeeds or the model is
    /// deleted.
    warm_failed: Option<String>,
    /// The install selected the model.
    auto_selected: bool,
}

impl BuiltinModel {
    fn inner(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// One row for Settings → Providers, the wizard and the AI-required prompt.
#[derive(Debug, Clone, Serialize)]
pub struct BuiltinRow {
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
    /// The model is loading into its worker after an install.
    pub warming: bool,
    /// The model is installed but its worker would not start on this
    /// computer; the last warm-up's error.
    pub warm_failed: Option<String>,
}

/// What the UI reads about the built-in model and the AI provider in use.
#[derive(Clone, Serialize)]
pub struct BuiltinStatus {
    pub model: BuiltinRow,
    pub provider: SummaryProviderStatus,
}

/// Whether the local model can be offered at all: installed and runnable
/// here.
#[derive(Debug, Clone, Copy, Default)]
pub struct LocalReadiness {
    pub installed: bool,
    pub capability: Capability,
}

/// The call to action shown wherever the built-in model needs a download.
pub(crate) fn download_hint() -> String {
    format!("Download the built-in model in {}.", crate::settings::providers_hint())
}

fn not_downloaded() -> AppError {
    AppError::Config(format!("Built-in model isn't downloaded. {}", download_hint()))
}

fn unsupported() -> AppError {
    AppError::Config(format!(
        "This computer can't run the built-in model. {}",
        Capability::Unsupported.line()
    ))
}

/// The gate. A GPU with room for the weights is fast; anything else is unsupported. An integrated GPU does not count.
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

fn llm_dir(models_dir: &Path) -> PathBuf {
    models_dir.join("llm")
}

/// Where a download in flight puts its shards, until the verified set is
/// renamed into place.
pub fn staging_dir(models_dir: &Path) -> PathBuf {
    llm_dir(models_dir).join(format!("{}.staging", row().id))
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

fn model_path(models_dir: &Path) -> Option<PathBuf> {
    installed(models_dir)
        .then(|| row().engine_path(&llm_dir(models_dir)).expect("llm rows have a path"))
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
fn run_probe(worker: &Path) -> Result<Probe> {
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

fn probe_capability() -> Result<Capability> {
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

/// What this computer can do with the model. The probe runs on the first
/// call and its answer is kept; a failed probe reads as unsupported and runs
/// again on the next call.
pub fn capability(app: &AppState) -> Capability {
    let mut slot = app.builtin.capability.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(c) = *slot {
        return c;
    }
    match probe_capability() {
        Ok(c) => {
            *slot = Some(c);
            c
        }
        Err(_) => Capability::Unsupported,
    }
}

/// The shared chat, for any inference on the built-in model. Refused while
/// the vault is locked, on a machine that cannot run it, while it is not
/// installed, and while it is being deleted.
pub fn chat(app: &AppState, vs: &VaultState) -> Result<Arc<LocalChat>> {
    if !vs.is_unlocked() {
        return Err(AppError::VaultLocked);
    }
    if capability(app) == Capability::Unsupported {
        return Err(unsupported());
    }
    let worker = worker_path()?;
    let mut inner = app.builtin.inner();
    // Checked again under the lock: closing the vault clears the keys before
    // `stop` takes this lock, so a chat handed out here is one `stop` closes.
    if !vs.is_unlocked() {
        return Err(AppError::VaultLocked);
    }
    if inner.deleting {
        return Err(AppError::Config("The built-in model is being deleted.".into()));
    }
    let model = model_path(&app.profile.models_dir()).ok_or_else(not_downloaded)?;
    if let Some(c) = inner.chat.as_ref() {
        if c.paths() == (worker.as_path(), model.as_path()) {
            return Ok(c.clone());
        }
    }
    if let Some(old) = inner.chat.take() {
        old.close();
    }
    let c = Arc::new(LocalChat::new(worker, model, LOCAL_CONTEXT_TOKENS));
    inner.chat = Some(c.clone());
    Ok(c)
}

/// Ends the worker and closes the shared chat: a request in flight ends with
/// an error, and a caller still holding the chat gets no worker back. The
/// next [`chat`] builds a fresh one.
pub fn stop(app: &AppState) {
    let taken = app.builtin.inner().chat.take();
    if let Some(c) = taken {
        c.close();
    }
}

/// Makes the built-in model this machine's AI provider. Refused unless this
/// computer can run it and it is installed.
pub fn select(app: &AppState) -> Result<()> {
    if capability(app) == Capability::Unsupported {
        return Err(unsupported());
    }
    let inner = app.builtin.inner();
    if inner.deleting || !installed(&app.profile.models_dir()) {
        return Err(not_downloaded());
    }
    app.set_ai_provider(Some(ProviderId::Local))
        .map_err(|e| AppError::Io(format!("save settings: {e}")))
}

/// Queues the download. Refused on a computer that cannot run the model; a
/// model already installed or already queued queues nothing.
pub fn install(app: &AppState) -> Result<()> {
    if capability(app) == Capability::Unsupported {
        return Err(unsupported());
    }
    if installed(&app.profile.models_dir()) || app.downloads.is_pending(row().id) {
        return Ok(());
    }
    app.downloads.enqueue(crate::downloads::DownloadKind::Llm, row().id)
}

/// Resets `deleting` however a delete ends.
struct Deleting<'a>(&'a BuiltinModel);

impl Drop for Deleting<'_> {
    fn drop(&mut self) {
        self.0.inner().deleting = false;
    }
}

/// Removes the model: its queue entry, the download in flight (waited out),
/// the worker, the weights, anything staged, and a `local` provider choice
/// on this machine. Blocking — call it from `spawn_blocking`.
pub fn delete(app: &AppState) -> Result<()> {
    let id = row().id;
    app.builtin.inner().deleting = true;
    let _deleting = Deleting(&app.builtin);
    let cancelled = app.downloads.cancel(id);
    app.downloads.wait_until_ended(id);
    stop(app);
    let models_dir = app.profile.models_dir();
    let dir = llm_dir(&models_dir).join(id);
    if dir.is_dir() {
        std::fs::remove_dir_all(&dir).map_err(|e| AppError::Io(format!("delete model dir: {e}")))?;
    }
    let stage = staging_dir(&models_dir);
    if stage.is_dir() {
        std::fs::remove_dir_all(&stage)
            .map_err(|e| AppError::Io(format!("delete staged download: {e}")))?;
    }
    if app.ai_provider() == Some(ProviderId::Local) {
        app.set_ai_provider(None).map_err(|e| AppError::Io(format!("save settings: {e}")))?;
    }
    {
        let mut inner = app.builtin.inner();
        inner.warm_failed = None;
        inner.auto_selected = false;
    }
    cancelled
}

/// The row and the AI provider status, as the UI shows them.
pub fn status(app: &AppState, vs: &VaultState) -> BuiltinStatus {
    // The probe spawns a subprocess; it runs before any lock is taken.
    let capability = capability(app);
    let models_dir = app.profile.models_dir();
    let (warming, warm_failed) = {
        let inner = app.builtin.inner();
        (inner.warming, inner.warm_failed.clone())
    };
    let r = row();
    let installed = installed(&models_dir);
    let model = BuiltinRow {
        id: r.id,
        display: r.display,
        installed,
        bytes: r.bytes,
        license: r.license,
        license_url: r.license_url,
        capability,
        capability_line: capability.line(),
        partial_bytes: dir_bytes(&staging_dir(&models_dir)),
        warming,
        warm_failed,
    };
    let installing = app.downloads.is_pending(r.id);
    let choice = app.ai_provider();
    let keys = vs.keys.lock().unwrap();
    let provider = crate::commands::summary::provider_status(
        choice,
        keys.as_ref(),
        LocalReadiness { installed, capability },
        installing,
    );
    BuiltinStatus { model, provider }
}

/// Serializes status reads with their emits.
static EMITTING: Mutex<()> = Mutex::new(());

/// Tells every window the status changed. Reads it on its own thread: the
/// first read probes the hardware.
pub fn emit<R: tauri::Runtime>(handle: &tauri::AppHandle<R>) {
    let handle = handle.clone();
    std::thread::spawn(move || {
        use tauri::{Emitter, Manager};
        let _order = EMITTING.lock().unwrap_or_else(|e| e.into_inner());
        let (Some(app), Some(vs)) = (handle.try_state::<AppState>(), handle.try_state::<VaultState>()) else {
            return;
        };
        let s = status(&app, &vs);
        if let Err(e) = handle.emit(CHANGED_EVENT, s) {
            log::warn!("built-in model: status not emitted: {e}");
        }
    });
}

/// After an install: the model becomes this machine's AI provider when none
/// is chosen, then loads into its worker on a thread of its own.
pub fn on_installed<R: tauri::Runtime>(handle: &tauri::AppHandle<R>) {
    use tauri::Manager;
    let app = handle.state::<AppState>();
    let capable = capability(&app) == Capability::Fast;
    {
        let mut inner = app.builtin.inner();
        if inner.deleting {
            return;
        }
        inner.warm_failed = None;
        inner.auto_selected = false;
        if capable && app.ai_provider().is_none() {
            match app.set_ai_provider(Some(ProviderId::Local)) {
                Ok(()) => inner.auto_selected = true,
                Err(e) => log::warn!("built-in model: not selected after install: {e}"),
            }
        }
    }
    emit(handle);
    let handle = handle.clone();
    std::thread::spawn(move || warm(&handle));
}

/// After the person selects the model: a model whose last warm-up failed is
/// warmed again, on a thread of its own.
pub fn on_selected<R: tauri::Runtime>(handle: &tauri::AppHandle<R>) {
    use tauri::Manager;
    let app = handle.state::<AppState>();
    let failed = {
        let mut inner = app.builtin.inner();
        inner.auto_selected = false;
        inner.warm_failed.is_some()
    };
    if failed {
        let handle = handle.clone();
        std::thread::spawn(move || warm(&handle));
    }
}

/// Loads the weights into the resident worker every later call reuses, with
/// one short request. Skipped while the vault is locked. A warm-up that
/// fails on its own (not ended by a stop, a lock or a delete) is recorded
/// for the row, and a selection the install made is taken back.
fn warm<R: tauri::Runtime>(handle: &tauri::AppHandle<R>) {
    use tauri::Manager;
    let app = handle.state::<AppState>();
    let vs = handle.state::<VaultState>();
    let chat = match chat(&app, &vs) {
        Ok(c) => c,
        Err(e) => {
            log::info!("built-in model warm-up skipped: {e}");
            return;
        }
    };
    app.builtin.inner().warming = true;
    emit(handle);
    let started = Instant::now();
    let reply = chat.complete_text("", &[("user".into(), "Reply with the word ready.".into())], 1);
    let ms = started.elapsed().as_millis();
    {
        let mut inner = app.builtin.inner();
        inner.warming = false;
        let stopped = inner.deleting
            || !vs.is_unlocked()
            || !inner.chat.as_ref().is_some_and(|c| Arc::ptr_eq(c, &chat));
        match reply {
            Ok(_) => {
                log::info!("built-in model warm-up finished in {ms} ms");
                inner.warm_failed = None;
            }
            Err(e) if stopped => log::info!("built-in model warm-up stopped after {ms} ms: {e}"),
            Err(e) => {
                log::warn!("built-in model warm-up failed after {ms} ms: {e}");
                inner.warm_failed = Some(e.to_string());
                if std::mem::take(&mut inner.auto_selected) && app.ai_provider() == Some(ProviderId::Local) {
                    if let Err(e) = app.set_ai_provider(None) {
                        log::warn!("built-in model: selection not taken back: {e}");
                    }
                }
            }
        }
    }
    emit(handle);
}

/// The queue's job for the built-in model: downloads the shards into
/// `<profile>/models/llm/<id>/` and ends at installed. A cancel seen at any
/// point ends it as cancelled; the canceller removes what it left.
/// Blocking — the queue runs it on `spawn_blocking`.
pub fn run_download(
    handle: &tauri::AppHandle,
    id: &str,
    cancel: &Arc<AtomicBool>,
    on_phase: Arc<dyn Fn(crate::downloads::Phase) + Send + Sync>,
) -> Result<()> {
    use providers_local::{download_llm_model_opts, DownloadOpts};
    use tauri::Manager;

    let state = handle.state::<AppState>();
    let llm_parent = llm_dir(&state.profile.models_dir());
    let progress_phase = on_phase.clone();
    let progress_cb: Box<dyn FnMut(u64, Option<u64>) + Send + 'static> =
        Box::new(move |downloaded, total| {
            progress_phase(crate::downloads::Phase::Downloading {
                pct: crate::downloads::pct(downloaded, total),
            });
        });
    on_phase(crate::downloads::Phase::Downloading { pct: 0.0 });
    let opts = DownloadOpts {
        progress: Some(progress_cb),
        cancel: Some(cancel.clone()),
        ..Default::default()
    };
    download_llm_model_opts(id, &llm_parent, opts)
        .map_err(|e| AppError::Config(format!("download model: {e:#}")))?;
    if cancel.load(Ordering::SeqCst) {
        return Err(AppError::Config("download cancelled by user".into()));
    }
    on_installed(handle);
    Ok(())
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

    fn app() -> (AppState, tempfile::TempDir) {
        let tmp = tempfile::tempdir().unwrap();
        let profile = crate::profile::ProfileDir::at(tmp.path()).unwrap();
        let app = AppState::new(profile);
        *app.builtin.capability.lock().unwrap() = Some(Capability::Fast);
        (app, tmp)
    }

    fn install(app: &AppState) {
        let d = llm_dir(&app.profile.models_dir()).join(row().id);
        std::fs::create_dir_all(&d).unwrap();
        for (n, _, size) in row().files {
            std::fs::File::create(d.join(n)).unwrap().set_len(*size).unwrap();
        }
    }

    /// Delete waits for the download in flight to end before it removes anything.
    #[test]
    fn delete_waits_out_the_running_download() {
        let (app, _t) = app();
        app.downloads.enqueue(crate::downloads::DownloadKind::Llm, row().id).unwrap();
        let (entry, flag) = app.downloads.take_due().unwrap();
        let stage = staging_dir(&app.profile.models_dir());
        std::fs::create_dir_all(&stage).unwrap();
        std::fs::write(stage.join("shard.part"), b"x").unwrap();

        let (seen, ended, writing) = (flag.clone(), app.downloads.clone(), stage.clone());
        let writer = std::thread::spawn(move || {
            while !seen.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(10));
            }
            std::thread::sleep(Duration::from_millis(300));
            assert!(writing.exists(), "the staging dir went while the job still ran");
            ended.finish(&entry.id, Err("download cancelled by user".into()));
        });
        let started = Instant::now();
        delete(&app).unwrap();
        assert!(started.elapsed() >= Duration::from_millis(300), "delete returned before the job ended");
        writer.join().unwrap();
        assert!(!stage.exists());
        assert!(app.downloads.status().is_empty());
    }

    /// A download that finishes while a delete waits for it is neither
    /// selected nor left installed.
    #[test]
    fn an_install_that_lands_during_a_delete_is_removed_and_not_selected() {
        let (app, _t) = app();
        app.downloads.enqueue(crate::downloads::DownloadKind::Llm, row().id).unwrap();
        let (entry, _flag) = app.downloads.take_due().unwrap();
        let m = tauri::test::mock_app();
        use tauri::Manager;
        m.manage(app);
        m.manage(VaultState::default());
        let handle = m.handle().clone();
        let deleter = std::thread::spawn(move || delete(&handle.state::<AppState>()));
        let app = m.state::<AppState>();
        while !app.builtin.inner().deleting {
            std::thread::sleep(Duration::from_millis(5));
        }
        install(&app);
        on_installed(m.handle());
        app.downloads.finish(&entry.id, Ok(()));
        deleter.join().unwrap().unwrap();
        assert!(!installed(&app.profile.models_dir()));
        assert_eq!(app.ai_provider(), None);
    }

    #[test]
    fn worker_path_honors_the_env_override() {
        std::env::set_var("DAISY_LLM_BIN", "/tmp/whatever-llm");
        assert_eq!(worker_path().unwrap(), PathBuf::from("/tmp/whatever-llm"));
        std::env::remove_var("DAISY_LLM_BIN");
    }
}
