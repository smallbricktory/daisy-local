// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! The download queue: every model download is an entry in
//! `downloads.<MACHINE>.json` next to the machine settings, drained serially
//! by one worker. A download outlives the page that started it and the launch
//! that enqueued it.

use crate::error::{AppError, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// Attempts one launch gives an entry before it stops retrying.
const MAX_ATTEMPTS: u32 = 5;
/// Wait between a failed attempt and the next.
const RETRY_AFTER: Duration = Duration::from_secs(60);

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "kebab-case")]
pub enum DownloadKind {
    Llm,
    Asr,
    SaAssets,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct QueueEntry {
    pub kind: DownloadKind,
    pub id: String,
    #[serde(default)]
    pub attempts: u32,
    #[serde(default)]
    pub last_error: Option<String>,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Phase {
    Queued,
    Downloading { pct: f32 },
    /// An attempt failed and the next one is waiting out its delay.
    Retrying { attempt: u32, of: u32, error: String },
    Failed { error: String },
}

/// How a download that left the queue ended. `Failed` is an entry that spent
/// its attempts; it stays listed, failed, until a retry or a cancel.
#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Installed,
    Cancelled,
    Failed,
}

/// The `download:finished` event.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Finished {
    pub id: String,
    pub kind: DownloadKind,
    pub outcome: Outcome,
}

#[derive(Serialize, Clone, Debug)]
pub struct DownloadJob {
    pub kind: DownloadKind,
    pub id: String,
    /// The source reports no byte count; the UI shows an indeterminate bar.
    pub indeterminate: bool,
    pub phase: Phase,
}

/// The queue file next to `settings.<MACHINE>.json`.
pub fn queue_path(settings_path: &Path) -> PathBuf {
    let name = crate::hardware::sanitized_machine_name();
    settings_path.with_file_name(format!("downloads.{name}.json"))
}

/// Whether `id` matches a row in either model registry.
fn has_model_row(id: &str) -> bool {
    providers_local::registry::row(id).is_some() || providers_local::registry::llm_row(id).is_some()
}

/// Everything a stopped download leaves on disk for `id`.
fn staging_paths(kind: DownloadKind, id: &str, models_dir: &Path) -> Vec<PathBuf> {
    match kind {
        DownloadKind::Llm => vec![models_dir.join("llm").join(format!("{id}.staging"))],
        DownloadKind::Asr => {
            let nemotron = models_dir.join("nemotron");
            vec![
                nemotron.join(format!("{id}.staging")),
                models_dir.join(format!("{id}.part")),
            ]
        }
        DownloadKind::SaAssets => Vec::new(),
    }
}

/// The downloaded fraction as a phase percent: 0.0 while the total is
/// unknown or zero, clamped to 1.0.
pub(crate) fn pct(downloaded: u64, total: Option<u64>) -> f32 {
    match total {
        Some(t) if t > 0 => (downloaded as f32 / t as f32).min(1.0),
        _ => 0.0,
    }
}

/// What the UI shows for a phase: a percent at whole-percent resolution.
fn shown(p: &Phase) -> (u8, i32) {
    match p {
        Phase::Queued => (0, 0),
        Phase::Downloading { pct } => (1, (pct * 100.0).round() as i32),
        Phase::Failed { .. } => (3, 0),
        Phase::Retrying { attempt, .. } => (4, *attempt as i32),
    }
}

#[derive(Default)]
struct Inner {
    entries: Vec<QueueEntry>,
    /// The entry the worker is on, and the phase it reached.
    active: Option<(String, Phase)>,
    /// The cancel signal of the job in flight.
    cancel: Option<(String, Arc<AtomicBool>)>,
    /// What a cancelled job in flight staged, removed when the job ends.
    purge_on_end: Option<DownloadKind>,
    /// Earliest next attempt, per id.
    retry_at: HashMap<String, Instant>,
    /// Endings not yet announced; [`emit`] sends and clears them.
    finished: Vec<Finished>,
}

struct QueueState {
    path: PathBuf,
    models_dir: PathBuf,
    inner: Mutex<Inner>,
    wake: tokio::sync::Notify,
    /// Signalled each time a job ends.
    ended: std::sync::Condvar,
}

/// The queue held on [`crate::state::AppState`]. Cloning shares one queue.
#[derive(Clone)]
pub struct DownloadQueue(Arc<QueueState>);

impl DownloadQueue {
    pub fn new(path: PathBuf, models_dir: PathBuf) -> Self {
        Self(Arc::new(QueueState {
            path,
            models_dir,
            inner: Mutex::new(Inner::default()),
            wake: tokio::sync::Notify::new(),
            ended: std::sync::Condvar::new(),
        }))
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.0.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn persist(&self, entries: &[QueueEntry]) -> Result<()> {
        let bytes = serde_json::to_vec_pretty(entries)
            .map_err(|e| AppError::Io(format!("serialize download queue: {e}")))?;
        syncsafe::write_atomic(&self.0.path, bytes)
            .map_err(|e| AppError::Io(format!("save download queue: {e}")))
    }

    /// Reads the file into the queue. Attempts reset; an entry that reads as `Failed` keeps that phase until `retry` clears it. An entry whose id matches no model row is dropped and the file rewritten without it.
    pub fn load(&self) {
        let Ok(bytes) = syncsafe::read(&self.0.path) else { return };
        match serde_json::from_slice::<Vec<QueueEntry>>(&bytes) {
            Ok(v) => {
                let before = v.len();
                let kept: Vec<QueueEntry> = v
                    .into_iter()
                    .filter(|e| {
                        let known = has_model_row(&e.id);
                        if !known {
                            log::warn!(
                                "download queue: dropping {}, which matches no model row",
                                e.id
                            );
                        }
                        known
                    })
                    .map(|mut e| {
                        if e.attempts < MAX_ATTEMPTS {
                            e.attempts = 0;
                        }
                        e
                    })
                    .collect();
                if before != kept.len() {
                    if let Err(e) = self.persist(&kept) {
                        log::warn!("download queue not saved: {e}");
                    }
                }
                self.lock().entries = kept;
            }
            Err(e) => log::warn!(
                "{} is unreadable ({e}); starting with an empty download queue",
                self.0.path.display()
            ),
        }
    }

    /// Adds `id` when it is not already queued. An id that matches no model
    /// row is refused.
    pub fn enqueue(&self, kind: DownloadKind, id: &str) -> Result<()> {
        if !has_model_row(id) {
            return Err(AppError::Config(format!("unknown model {id:?}")));
        }
        let entries = {
            let mut inner = self.lock();
            if inner.entries.iter().any(|e| e.id == id) {
                return Ok(());
            }
            inner.entries.push(QueueEntry {
                kind,
                id: id.to_string(),
                attempts: 0,
                last_error: None,
            });
            inner.entries.clone()
        };
        let saved = self.persist(&entries);
        self.0.wake.notify_one();
        saved
    }

    /// Drops `id`: the entry, its retry schedule, the job in flight and what a stopped download left staged. A job in flight is told to stop, and what it staged is removed when it ends.
    pub fn cancel(&self, id: &str) -> Result<()> {
        let (purge_now, entries) = {
            let mut inner = self.lock();
            let kind = inner.entries.iter().find(|e| e.id == id).map(|e| e.kind);
            let running = match inner.cancel.as_ref() {
                Some((active, flag)) if active == id => {
                    flag.store(true, Ordering::SeqCst);
                    true
                }
                _ => false,
            };
            if running {
                inner.purge_on_end = inner.purge_on_end.or(kind);
            } else if let Some(kind) = kind {
                inner.finished.push(Finished { id: id.to_string(), kind, outcome: Outcome::Cancelled });
            }
            inner.entries.retain(|e| e.id != id);
            inner.retry_at.remove(id);
            (kind.filter(|_| !running), inner.entries.clone())
        };
        let saved = self.persist(&entries);
        if let Some(k) = purge_now {
            self.purge(k, id);
        }
        self.0.wake.notify_one();
        saved
    }

    fn purge(&self, kind: DownloadKind, id: &str) {
        for p in staging_paths(kind, id, &self.0.models_dir) {
            let _ = std::fs::remove_dir_all(&p);
            let _ = std::fs::remove_file(&p);
        }
    }

    /// Returns once `id` is not the job in flight. A cancelled job ends at
    /// its next cancel check.
    pub fn wait_until_ended(&self, id: &str) {
        let mut inner = self.lock();
        while inner.active.as_ref().is_some_and(|(active, _)| active == id) {
            inner = self.0.ended.wait(inner).unwrap_or_else(|e| e.into_inner());
        }
    }

    /// Puts a failed entry back in line with a fresh attempt budget.
    pub fn retry(&self, id: &str) -> Result<()> {
        let entries = {
            let mut inner = self.lock();
            if let Some(e) = inner.entries.iter_mut().find(|e| e.id == id) {
                e.attempts = 0;
                e.last_error = None;
            }
            inner.retry_at.remove(id);
            inner.entries.clone()
        };
        let saved = self.persist(&entries);
        self.0.wake.notify_one();
        saved
    }

    pub fn status(&self) -> Vec<DownloadJob> {
        let inner = self.lock();
        inner
            .entries
            .iter()
            .map(|e| DownloadJob {
                kind: e.kind,
                id: e.id.clone(),
                indeterminate: e.kind == DownloadKind::SaAssets,
                phase: phase_of(&inner, e),
            })
            .collect()
    }

    /// `id` is queued or downloading — not failed, not absent.
    pub fn is_pending(&self, id: &str) -> bool {
        let inner = self.lock();
        inner
            .entries
            .iter()
            .find(|e| e.id == id)
            .is_some_and(|e| !matches!(phase_of(&inner, e), Phase::Failed { .. }))
    }

    /// The head entry that may run now, marked active with its cancel signal armed under the same lock. `None` while every entry is waiting out a retry delay or has spent its attempts.
    pub(crate) fn take_due(&self) -> Option<(QueueEntry, Arc<AtomicBool>)> {
        let now = Instant::now();
        let mut inner = self.lock();
        if inner.active.is_some() {
            return None;
        }
        let due = {
            let retry_at = &inner.retry_at;
            inner
                .entries
                .iter()
                .find(|e| {
                    e.attempts < MAX_ATTEMPTS
                        && retry_at.get(&e.id).is_none_or(|t| *t <= now)
                })
                .cloned()
        };
        let entry = due?;
        inner.active = Some((entry.id.clone(), Phase::Downloading { pct: 0.0 }));
        let flag = Arc::new(AtomicBool::new(false));
        inner.cancel = Some((entry.id.clone(), flag.clone()));
        Some((entry, flag))
    }

    /// Records the phase of the job in flight. A no-op once `id` is no longer the active job. True when what the UI shows changed.
    pub(crate) fn set_active_phase(&self, id: &str, phase: Phase) -> bool {
        let mut inner = self.lock();
        match inner.active.as_ref() {
            Some((active, p)) if active == id => {
                let same = shown(p) == shown(&phase);
                inner.active = Some((id.to_string(), phase));
                !same
            }
            _ => false,
        }
    }

    /// Ends the active job. Success drops the entry; so does a cancelled job regardless of its result, and its error is logged. An uncancelled failure counts an attempt and schedules the next one, except when the bytes did not match their pinned sha256: that spends the whole budget at once and lands on `Failed`, which only [`Self::retry`] leaves.
    pub(crate) fn finish(&self, id: &str, result: std::result::Result<(), String>) {
        let (entries, purge) = {
            let mut inner = self.lock();
            let cancelled = matches!(
                inner.cancel.as_ref(),
                Some((active, flag)) if active == id && flag.load(Ordering::SeqCst)
            );
            inner.active = None;
            inner.cancel = None;
            let purge = inner.purge_on_end.take();
            let kind = inner.entries.iter().find(|e| e.id == id).map(|e| e.kind).or(purge);
            let outcome = if cancelled {
                Some(Outcome::Cancelled)
            } else if result.is_ok() {
                Some(Outcome::Installed)
            } else {
                None
            };
            if cancelled || result.is_ok() {
                if let Err(error) = &result {
                    log::warn!("download queue: {id} was cancelled, ended with: {error}");
                }
                inner.entries.retain(|e| e.id != id);
                inner.retry_at.remove(id);
            } else if let Err(error) = result {
                let permanent = providers_local::is_integrity_failure(&error);
                let mut schedule = false;
                if let Some(e) = inner.entries.iter_mut().find(|e| e.id == id) {
                    e.attempts = if permanent { MAX_ATTEMPTS } else { e.attempts + 1 };
                    e.last_error = Some(error);
                    schedule = e.attempts < MAX_ATTEMPTS;
                }
                if schedule {
                    inner.retry_at.insert(id.to_string(), Instant::now() + RETRY_AFTER);
                }
            }
            let spent = inner.entries.iter().any(|e| e.id == id && e.attempts >= MAX_ATTEMPTS);
            if let (Some(kind), Some(outcome)) =
                (kind, outcome.or(spent.then_some(Outcome::Failed)))
            {
                inner.finished.push(Finished { id: id.to_string(), kind, outcome });
            }
            (inner.entries.clone(), purge)
        };
        if let Some(k) = purge {
            self.purge(k, id);
        }
        self.0.ended.notify_all();
        if let Err(e) = self.persist(&entries) {
            log::warn!("download queue not saved: {e}");
        }
    }

    /// Endings recorded since the last call, oldest first.
    pub fn take_finished(&self) -> Vec<Finished> {
        std::mem::take(&mut self.lock().finished)
    }

    /// Makes `id` due now. Tests drive the attempt cap with it.
    #[cfg(test)]
    pub(crate) fn clear_retry_delay(&self, id: &str) {
        self.lock().retry_at.remove(id);
    }

    /// How long the worker may sleep before an entry it can run comes due.
    /// `None` when no entry will ever come due on its own, so the worker
    /// parks until it is woken.
    pub(crate) fn next_wait(&self) -> Option<Duration> {
        let now = Instant::now();
        let inner = self.lock();
        inner
            .entries
            .iter()
            .filter(|e| e.attempts < MAX_ATTEMPTS)
            .map(|e| {
                inner
                    .retry_at
                    .get(&e.id)
                    .map_or(Duration::ZERO, |t| t.saturating_duration_since(now))
            })
            .min()
    }

    /// Parks until something is enqueued, cancelled or retried, or until the
    /// earliest waiting entry's retry delay runs out.
    pub(crate) async fn wait_for_work(&self) {
        match self.next_wait() {
            Some(d) => {
                let _ = tokio::time::timeout(d, self.0.wake.notified()).await;
            }
            None => self.0.wake.notified().await,
        }
    }
}

/// Sends the whole queue to every window, then each ending since the last
/// send as `download:finished`.
pub fn emit(app: &tauri::AppHandle, queue: &DownloadQueue) {
    use tauri::Emitter;
    let _ = app.emit("download-queue:changed", queue.status());
    for f in queue.take_finished() {
        let _ = app.emit("download:finished", f);
    }
}

/// Runs one entry to completion on the calling thread.
fn run_job(
    app: &tauri::AppHandle,
    queue: &DownloadQueue,
    entry: &QueueEntry,
    cancel: &Arc<AtomicBool>,
) -> std::result::Result<(), String> {
    let id = entry.id.clone();
    let on_phase: Arc<dyn Fn(Phase) + Send + Sync> = {
        let (app, queue, id) = (app.clone(), queue.clone(), id.clone());
        Arc::new(move |phase: Phase| {
            if queue.set_active_phase(&id, phase) {
                emit(&app, &queue);
            }
        })
    };
    let outcome = match entry.kind {
        DownloadKind::Llm => crate::builtin_model::run_download(app, &entry.id, cancel, on_phase),
        DownloadKind::Asr | DownloadKind::SaAssets => {
            crate::commands::asr_models::run_asr_download(app, &entry.id, cancel, on_phase)
        }
    };
    outcome.map_err(|e| e.to_string())
}

/// Loads the persisted queue and starts the one worker that drains it in FIFO order. Called from setup once the profile resolves.
pub fn start(app: tauri::AppHandle) {
    use tauri::Manager;
    static STARTED: AtomicBool = AtomicBool::new(false);
    if STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    let queue = app.state::<crate::state::AppState>().downloads.clone();
    queue.load();
    emit(&app, &queue);
    tauri::async_runtime::spawn(async move {
        loop {
            let Some((entry, cancel)) = queue.take_due() else {
                queue.wait_for_work().await;
                continue;
            };
            emit(&app, &queue);
            let (a, q, e) = (app.clone(), queue.clone(), entry.clone());
            let joined =
                tauri::async_runtime::spawn_blocking(move || run_job(&a, &q, &e, &cancel)).await;
            let result = match joined {
                Ok(r) => r,
                Err(err) => Err(format!("download task join: {err}")),
            };
            match &result {
                Ok(()) => log::info!("download queue: {} finished", entry.id),
                Err(e) => log::warn!("download queue: {} failed: {e}", entry.id),
            }
            queue.finish(&entry.id, result);
            emit(&app, &queue);
            match entry.kind {
                DownloadKind::Llm => crate::builtin_model::emit(&app),
                DownloadKind::Asr | DownloadKind::SaAssets => {
                    crate::commands::asr_models::emit(&app)
                }
            }
        }
    });
}

fn phase_of(inner: &Inner, e: &QueueEntry) -> Phase {
    if let Some((active, phase)) = inner.active.as_ref() {
        if active == &e.id {
            return phase.clone();
        }
    }
    match e.last_error.as_ref() {
        Some(error) if e.attempts >= MAX_ATTEMPTS => Phase::Failed { error: error.clone() },
        Some(error) if e.attempts > 0 => Phase::Retrying {
            attempt: e.attempts + 1,
            of: MAX_ATTEMPTS,
            error: error.clone(),
        },
        _ => Phase::Queued,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn queue() -> (DownloadQueue, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let q = DownloadQueue::new(
            queue_path(&dir.path().join("settings.json")),
            dir.path().join("models"),
        );
        (q, dir)
    }

    #[test]
    fn the_queue_file_sits_beside_the_machine_settings() {
        let p = queue_path(std::path::Path::new("/profile/settings.json"));
        let name = p.file_name().unwrap().to_str().unwrap().to_string();
        assert!(name.starts_with("downloads."), "{name}");
        assert!(name.ends_with(".json"), "{name}");
        assert_eq!(p.parent().unwrap(), std::path::Path::new("/profile"));
    }

    #[test]
    fn entries_round_trip_through_the_file() {
        let (q, dir) = queue();
        q.enqueue(DownloadKind::Llm, "gemma-4-e4b-it-q4").unwrap();
        q.enqueue(DownloadKind::Asr, providers_local::registry::NEMOTRON_ID).unwrap();
        let fresh = DownloadQueue::new(
            queue_path(&dir.path().join("settings.json")),
            dir.path().join("models"),
        );
        fresh.load();
        let ids: Vec<String> = fresh.status().into_iter().map(|j| j.id).collect();
        assert_eq!(ids, vec!["gemma-4-e4b-it-q4", providers_local::registry::NEMOTRON_ID]);
    }

    #[test]
    fn load_resets_the_attempt_budget() {
        let (q, dir) = queue();
        let path = queue_path(&dir.path().join("settings.json"));
        std::fs::write(
            &path,
            r#"[{"kind":"llm","id":"gemma-4-e4b-it-q4","attempts":3,"last_error":"timed out"}]"#,
        )
        .unwrap();
        q.load();
        let inner = q.lock();
        let e = inner.entries.iter().find(|e| e.id == "gemma-4-e4b-it-q4").unwrap();
        assert_eq!(e.attempts, 0);
        assert_eq!(e.last_error.as_deref(), Some("timed out"));
    }

    #[test]
    fn an_entry_that_spent_its_whole_budget_reads_failed_after_a_restart() {
        let (q, dir) = queue();
        let path = queue_path(&dir.path().join("settings.json"));
        std::fs::write(
            &path,
            r#"[{"kind":"llm","id":"gemma-4-e4b-it-q4","attempts":5,"last_error":"404"}]"#,
        )
        .unwrap();
        q.load();
        let jobs = q.status();
        assert_eq!(jobs[0].phase, Phase::Failed { error: "404".into() });
        assert!(q.take_due().is_none(), "an exhausted entry is not due");

        q.retry("gemma-4-e4b-it-q4").unwrap();
        assert_eq!(q.status()[0].phase, Phase::Queued);
        assert!(q.take_due().is_some(), "retry gives the entry a fresh budget");
    }

    #[test]
    fn load_drops_an_entry_no_model_row_can_show() {
        let (q, dir) = queue();
        let path = queue_path(&dir.path().join("settings.json"));
        std::fs::write(
            &path,
            r#"[{"kind":"llm","id":"retired-model","attempts":0,"last_error":null},
                {"kind":"llm","id":"gemma-4-e4b-it-q4","attempts":0,"last_error":null}]"#,
        )
        .unwrap();
        q.load();
        let ids: Vec<String> = q.status().into_iter().map(|j| j.id).collect();
        assert_eq!(ids, vec!["gemma-4-e4b-it-q4"]);
        assert!(q.take_due().is_some_and(|(e, _)| e.id == "gemma-4-e4b-it-q4"));

        let on_disk: Vec<QueueEntry> =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(on_disk.len(), 1, "the dropped entry must not come back next launch");
    }

    #[test]
    fn an_idle_queue_parks_until_it_is_woken() {
        let (q, _d) = queue();
        assert_eq!(q.next_wait(), None, "an empty queue has nothing to wake for");

        q.enqueue(DownloadKind::Llm, "gemma-4-e4b-it-q4").unwrap();
        assert_eq!(q.next_wait(), Some(Duration::ZERO), "a due entry runs now");

        assert!(q.take_due().is_some());
        q.finish("gemma-4-e4b-it-q4", Err("timed out".into()));
        let wait = q.next_wait().expect("a waiting entry sets a wake time");
        assert!(wait > RETRY_AFTER / 2 && wait <= RETRY_AFTER, "{wait:?}");

        for _ in 1..MAX_ATTEMPTS {
            q.clear_retry_delay("gemma-4-e4b-it-q4");
            assert!(q.take_due().is_some());
            q.finish("gemma-4-e4b-it-q4", Err("timed out".into()));
        }
        assert_eq!(q.next_wait(), None, "a spent entry never wakes the worker again");
    }

    #[test]
    fn enqueue_is_idempotent() {
        let (q, _d) = queue();
        q.enqueue(DownloadKind::Llm, "gemma-4-e4b-it-q4").unwrap();
        q.enqueue(DownloadKind::Llm, "gemma-4-e4b-it-q4").unwrap();
        assert_eq!(q.status().len(), 1);
    }

    #[test]
    fn a_queued_entry_is_pending_and_an_unqueued_one_is_not() {
        let (q, _d) = queue();
        q.enqueue(DownloadKind::Llm, "gemma-4-e4b-it-q4").unwrap();
        assert!(q.is_pending("gemma-4-e4b-it-q4"));
        assert!(!q.is_pending(providers_local::registry::NEMOTRON_ID));
    }

    #[test]
    fn a_first_failure_reads_retrying_and_stays_pending() {
        let (q, _d) = queue();
        q.enqueue(DownloadKind::Llm, "gemma-4-e4b-it-q4").unwrap();
        q.finish("gemma-4-e4b-it-q4", Err("no route to host".into()));
        assert!(q.is_pending("gemma-4-e4b-it-q4"), "attempts remain; still pending");
        assert_eq!(
            q.status()[0].phase,
            Phase::Retrying {
                attempt: 2,
                of: MAX_ATTEMPTS,
                error: "no route to host".into()
            }
        );
    }

    #[test]
    fn a_bogus_id_is_refused_at_enqueue() {
        let (q, _d) = queue();
        for id in ["../../../etc", "..", "gemma-4-e4b-it-q4/../../evil", "", "nonesuch"] {
            assert!(q.enqueue(DownloadKind::Llm, id).is_err(), "accepted {id:?}");
            assert!(q.enqueue(DownloadKind::Asr, id).is_err(), "accepted {id:?}");
            assert!(q.enqueue(DownloadKind::SaAssets, id).is_err(), "accepted {id:?}");
        }
        assert!(q.status().is_empty());
    }

    #[test]
    fn every_kind_the_ui_can_start_has_an_id_enqueue_accepts() {
        let (q, _d) = queue();
        q.enqueue(DownloadKind::Llm, "gemma-4-e4b-it-q4").unwrap();
        q.enqueue(DownloadKind::Asr, providers_local::registry::NEMOTRON_ID).unwrap();
        q.enqueue(DownloadKind::SaAssets, "speechanalyzer").unwrap();
        assert_eq!(q.status().len(), 3);
    }

    #[test]
    fn a_pin_mismatch_fails_at_once_instead_of_retrying() {
        let (q, _d) = queue();
        q.enqueue(DownloadKind::Llm, "gemma-4-e4b-it-q4").unwrap();
        let error = "download model: sha256 mismatch for gemma-4-e4b-it-q4: got aa, want bb";
        q.finish("gemma-4-e4b-it-q4", Err(error.into()));
        assert!(!q.is_pending("gemma-4-e4b-it-q4"));
        assert_eq!(q.status()[0].phase, Phase::Failed { error: error.into() });
        assert!(q.take_due().is_none());
    }

    #[test]
    fn a_pin_mismatch_the_user_retries_gets_a_fresh_budget() {
        let (q, _d) = queue();
        q.enqueue(DownloadKind::Llm, "gemma-4-e4b-it-q4").unwrap();
        q.finish("gemma-4-e4b-it-q4", Err("sha256 mismatch for x: got aa, want bb".into()));
        q.retry("gemma-4-e4b-it-q4").unwrap();
        assert!(q.is_pending("gemma-4-e4b-it-q4"));
        assert_eq!(q.status()[0].phase, Phase::Queued);
    }

    #[test]
    fn each_wait_between_attempts_names_the_attempt_to_come() {
        let (q, _d) = queue();
        q.enqueue(DownloadKind::Llm, "gemma-4-e4b-it-q4").unwrap();
        for attempt in 2..=MAX_ATTEMPTS {
            assert!(q.take_due().is_some());
            q.finish("gemma-4-e4b-it-q4", Err("timed out".into()));
            assert_eq!(
                q.status()[0].phase,
                Phase::Retrying { attempt, of: MAX_ATTEMPTS, error: "timed out".into() }
            );
            q.clear_retry_delay("gemma-4-e4b-it-q4");
        }
    }

    #[test]
    fn only_the_attempt_that_spends_the_budget_reads_failed() {
        let (q, _d) = queue();
        q.enqueue(DownloadKind::Llm, "gemma-4-e4b-it-q4").unwrap();
        for _ in 0..4 {
            assert!(q.take_due().is_some());
            q.finish("gemma-4-e4b-it-q4", Err("timed out".into()));
            q.clear_retry_delay("gemma-4-e4b-it-q4");
            assert!(q.is_pending("gemma-4-e4b-it-q4"));
            assert!(matches!(q.status()[0].phase, Phase::Retrying { .. }));
        }
        assert!(q.take_due().is_some());
        q.finish("gemma-4-e4b-it-q4", Err("no route to host".into()));
        assert!(!q.is_pending("gemma-4-e4b-it-q4"));
        assert_eq!(
            q.status()[0].phase,
            Phase::Failed { error: "no route to host".into() }
        );
    }

    #[test]
    fn a_failed_entry_waits_out_the_retry_delay() {
        let (q, _d) = queue();
        q.enqueue(DownloadKind::Llm, "gemma-4-e4b-it-q4").unwrap();
        assert!(q.take_due().is_some());
        q.finish("gemma-4-e4b-it-q4", Err("timed out".into()));
        assert!(q.take_due().is_none(), "the retry delay holds the entry back");
        q.clear_retry_delay("gemma-4-e4b-it-q4");
        assert!(q.take_due().is_some());
    }

    #[test]
    fn five_failures_stop_the_entry_until_retry_resets_it() {
        let (q, _d) = queue();
        q.enqueue(DownloadKind::Llm, "gemma-4-e4b-it-q4").unwrap();
        for _ in 0..5 {
            assert!(q.take_due().is_some());
            q.finish("gemma-4-e4b-it-q4", Err("timed out".into()));
            q.clear_retry_delay("gemma-4-e4b-it-q4");
        }
        assert!(q.take_due().is_none(), "the cap holds the entry back");
        q.retry("gemma-4-e4b-it-q4").unwrap();
        assert!(q.take_due().is_some());
    }

    #[test]
    fn each_ending_is_announced_once_with_its_outcome() {
        let (q, _d) = queue();
        let ended = |q: &DownloadQueue| -> Vec<(String, Outcome)> {
            q.take_finished().into_iter().map(|f| (f.id, f.outcome)).collect()
        };
        let llm = "gemma-4-e4b-it-q4";
        let asr = providers_local::registry::NEMOTRON_ID;

        q.enqueue(DownloadKind::Llm, llm).unwrap();
        q.take_due().unwrap();
        q.finish(llm, Ok(()));
        assert_eq!(ended(&q), vec![(llm.to_string(), Outcome::Installed)]);
        assert!(ended(&q).is_empty(), "an ending is announced once");

        q.enqueue(DownloadKind::Asr, asr).unwrap();
        q.cancel(asr).unwrap();
        assert_eq!(ended(&q), vec![(asr.to_string(), Outcome::Cancelled)]);

        q.enqueue(DownloadKind::Llm, llm).unwrap();
        q.take_due().unwrap();
        q.cancel(llm).unwrap();
        assert!(ended(&q).is_empty(), "the running job ends when the worker says so");
        q.finish(llm, Err("download cancelled by user".into()));
        assert_eq!(ended(&q), vec![(llm.to_string(), Outcome::Cancelled)]);

        q.enqueue(DownloadKind::Llm, llm).unwrap();
        for attempt in 1..=MAX_ATTEMPTS {
            q.take_due().unwrap();
            q.finish(llm, Err("timed out".into()));
            q.clear_retry_delay(llm);
            let expected = if attempt == MAX_ATTEMPTS {
                vec![(llm.to_string(), Outcome::Failed)]
            } else {
                vec![]
            };
            assert_eq!(ended(&q), expected, "attempt {attempt}");
        }
        q.cancel(llm).unwrap();
        assert_eq!(ended(&q), vec![(llm.to_string(), Outcome::Cancelled)]);
    }

    #[test]
    fn cancel_removes_the_entry_and_its_staging_dir() {
        let (q, dir) = queue();
        let stage = dir.path().join("models").join("llm").join("gemma-4-e4b-it-q4.staging");
        std::fs::create_dir_all(&stage).unwrap();
        std::fs::write(stage.join("shard.gguf.part"), b"x").unwrap();
        q.enqueue(DownloadKind::Llm, "gemma-4-e4b-it-q4").unwrap();
        q.cancel("gemma-4-e4b-it-q4").unwrap();
        assert!(q.status().is_empty());
        assert!(!stage.exists(), "the staging dir must be gone");
    }

    #[test]
    fn cancelling_the_running_job_leaves_its_staging_dir_until_the_job_ends() {
        let (q, dir) = queue();
        let stage = dir.path().join("models").join("llm").join("gemma-4-e4b-it-q4.staging");
        q.enqueue(DownloadKind::Llm, "gemma-4-e4b-it-q4").unwrap();
        let (entry, flag) = q.take_due().unwrap();
        std::fs::create_dir_all(&stage).unwrap();
        std::fs::write(stage.join("shard.gguf.part"), b"x").unwrap();
        q.cancel(&entry.id).unwrap();
        assert!(flag.load(Ordering::SeqCst));
        assert!(stage.exists(), "the writer still owns the staging dir");
        q.finish(&entry.id, Err("download cancelled by user".into()));
        assert!(!stage.exists(), "the job's end removes what it staged");
    }

    #[test]
    fn a_late_phase_report_after_finish_does_not_stall_the_queue() {
        let (q, _d) = queue();
        q.enqueue(DownloadKind::Llm, "gemma-4-e4b-it-q4").unwrap();
        q.enqueue(DownloadKind::Asr, providers_local::registry::NEMOTRON_ID).unwrap();
        let (entry, _flag) = q.take_due().unwrap();
        q.finish(&entry.id, Ok(()));
        let changed = q.set_active_phase(&entry.id, Phase::Downloading { pct: 0.5 });
        assert!(!changed, "a phase report for a finished job reports no change");
        assert!(q.take_due().is_some(), "the next entry must still be takeable");
    }

    #[test]
    fn cancel_removes_a_whisper_entrys_leftover_part_file() {
        let (q, dir) = queue();
        let part = dir.path().join("models").join("ggml-base.en.bin.part");
        std::fs::create_dir_all(part.parent().unwrap()).unwrap();
        std::fs::write(&part, b"x").unwrap();
        q.enqueue(DownloadKind::Asr, "ggml-base.en.bin").unwrap();
        q.cancel("ggml-base.en.bin").unwrap();
        assert!(!part.exists(), "the whisper .part file must be gone");
    }

    #[test]
    fn a_job_serialises_the_shape_the_frontend_reads() {
        let job = DownloadJob {
            kind: DownloadKind::SaAssets,
            id: "speechanalyzer".into(),
            indeterminate: true,
            phase: Phase::Downloading { pct: 0.5 },
        };
        let v = serde_json::to_value(&job).unwrap();
        assert_eq!(v["kind"], "sa-assets");
        assert_eq!(v["indeterminate"], true);
        assert_eq!(v["phase"]["state"], "downloading");
        assert_eq!(v["phase"]["pct"], 0.5);
        let failed = serde_json::to_value(Phase::Failed { error: "nope".into() }).unwrap();
        assert_eq!(failed["state"], "failed");
        assert_eq!(failed["error"], "nope");
        let retrying =
            serde_json::to_value(Phase::Retrying { attempt: 2, of: 5, error: "nope".into() })
                .unwrap();
        assert_eq!(retrying["state"], "retrying");
        assert_eq!(retrying["attempt"], 2);
        assert_eq!(retrying["of"], 5);
        assert_eq!(retrying["error"], "nope");
    }

    #[test]
    fn cancel_after_take_due_still_sets_the_flag() {
        let (q, _d) = queue();
        q.enqueue(DownloadKind::Llm, "gemma-4-e4b-it-q4").unwrap();
        let (entry, flag) = q.take_due().unwrap();
        assert!(!flag.load(Ordering::SeqCst));
        q.cancel(&entry.id).unwrap();
        assert!(flag.load(Ordering::SeqCst), "take_due must arm the flag before any job runs");
    }

    #[test]
    fn cancel_while_downloading_drops_the_entry_now() {
        let (q, _d) = queue();
        q.enqueue(DownloadKind::Llm, "gemma-4-e4b-it-q4").unwrap();
        let (entry, flag) = q.take_due().unwrap();
        q.set_active_phase(&entry.id, Phase::Downloading { pct: 0.5 });
        q.cancel(&entry.id).unwrap();
        assert!(flag.load(Ordering::SeqCst), "the runner must see the cancel");
        assert!(q.status().is_empty());
    }

    #[test]
    fn byte_progress_only_reports_when_the_whole_percent_moves() {
        let (q, _d) = queue();
        q.enqueue(DownloadKind::Llm, "gemma-4-e4b-it-q4").unwrap();
        let (entry, _flag) = q.take_due().unwrap();
        assert!(q.set_active_phase(&entry.id, Phase::Downloading { pct: 0.42 }));
        assert!(!q.set_active_phase(&entry.id, Phase::Downloading { pct: 0.4201 }));
        assert!(q.set_active_phase(&entry.id, Phase::Downloading { pct: 0.43 }));
    }

    #[test]
    fn pct_is_zero_without_a_total_and_clamped_to_one() {
        assert_eq!(pct(10, None), 0.0);
        assert_eq!(pct(10, Some(0)), 0.0);
        assert_eq!(pct(42, Some(100)), 0.42);
        assert_eq!(pct(150, Some(100)), 1.0);
    }

    /// A queue whose file cannot be written: the parent of the queue path is
    /// a regular file, so every `persist` fails.
    fn unwritable_queue() -> (DownloadQueue, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let blocked = dir.path().join("blocked");
        std::fs::write(&blocked, b"a file, not a directory").unwrap();
        let q = DownloadQueue::new(blocked.join("downloads.json"), dir.path().join("models"));
        (q, dir)
    }

    /// Consumes one wake permit; false when none is waiting.
    async fn woken(q: &DownloadQueue) -> bool {
        tokio::time::timeout(Duration::from_millis(250), q.0.wake.notified())
            .await
            .is_ok()
    }

    #[tokio::test]
    async fn a_queue_whose_file_cannot_be_written_still_wakes_the_worker() {
        let (q, _d) = unwritable_queue();

        assert!(q.enqueue(DownloadKind::Llm, "gemma-4-e4b-it-q4").is_err());
        assert_eq!(q.status().len(), 1, "the entry is in memory whatever the file did");
        assert!(woken(&q).await, "enqueue wakes the worker even when the save failed");

        assert!(q.retry("gemma-4-e4b-it-q4").is_err());
        assert!(woken(&q).await, "retry wakes the worker even when the save failed");

        assert!(q.cancel("gemma-4-e4b-it-q4").is_err());
        assert!(woken(&q).await, "cancel wakes the worker even when the save failed");
        assert!(q.status().is_empty(), "the cancelled entry is gone from memory");
    }

    #[test]
    fn byte_progress_reaches_status_through_set_active_phase() {
        let (q, _d) = queue();
        q.enqueue(DownloadKind::Llm, "gemma-4-e4b-it-q4").unwrap();
        let (entry, _flag) = q.take_due().unwrap();
        q.set_active_phase(&entry.id, Phase::Downloading { pct: pct(42, Some(100)) });
        assert_eq!(q.status()[0].phase, Phase::Downloading { pct: 0.42 });
    }
}
