// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Registry of the AI tasks running right now — the summary generator and the
//! chapter extractor, which share one local-LLM worker.
//!
//! One task at a time: `begin` refuses while the list holds anything, and
//! returns a guard that clears its entry on drop. Every change emits
//! `ai-jobs:changed` carrying the whole list; `ai_jobs_list` reads it once at
//! app start.

use crate::error::{AppError, Result};
use serde::{Deserialize, Serialize};
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, Manager, Runtime};

/// Which AI task a job is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AiJobKind {
    Summary,
    Chapters,
}

/// One running AI task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiJob {
    pub session_id: String,
    pub kind: AiJobKind,
    pub started_at_unix_seconds: i64,
}

/// Tauri-managed state: the AI tasks in flight.
#[derive(Default)]
pub struct AiJobs(Mutex<Vec<AiJob>>);

impl AiJobs {
    /// Claims the AI worker for `session_id`/`kind`. Errors with
    /// `AppError::AiJobBusy` while any job is running. The returned guard
    /// releases the claim when it drops.
    pub fn begin<R: Runtime>(
        &self,
        app: &AppHandle<R>,
        session_id: impl Into<String>,
        kind: AiJobKind,
    ) -> Result<AiJobGuard<R>> {
        let session_id = session_id.into();
        {
            let mut jobs = self.0.lock().unwrap_or_else(|e| e.into_inner());
            if !jobs.is_empty() {
                return Err(AppError::AiJobBusy);
            }
            jobs.push(AiJob {
                session_id: session_id.clone(),
                kind,
                started_at_unix_seconds: crate::now_unix(),
            });
        }
        self.emit(app);
        Ok(AiJobGuard {
            app: app.clone(),
            session_id,
            kind,
        })
    }

    /// The jobs in flight.
    pub fn list(&self) -> Vec<AiJob> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn release<R: Runtime>(&self, app: &AppHandle<R>, session_id: &str, kind: AiJobKind) {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|j| j.session_id != session_id || j.kind != kind);
        self.emit(app);
    }

    fn emit<R: Runtime>(&self, app: &AppHandle<R>) {
        if let Err(e) = app.emit("ai-jobs:changed", self.list()) {
            log::warn!("emit ai-jobs:changed: {e}");
        }
    }
}

/// Holds one entry in the registry. Dropping it clears the entry and emits,
/// so every return path — value, error or unwind — frees the worker.
pub struct AiJobGuard<R: Runtime> {
    app: AppHandle<R>,
    session_id: String,
    kind: AiJobKind,
}

impl<R: Runtime> Drop for AiJobGuard<R> {
    fn drop(&mut self) {
        if let Some(jobs) = self.app.try_state::<AiJobs>() {
            jobs.release(&self.app, &self.session_id, self.kind);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app_with_registry() -> tauri::App<tauri::test::MockRuntime> {
        let app = tauri::test::mock_app();
        app.manage(AiJobs::default());
        app
    }

    #[test]
    fn a_second_job_is_refused_while_one_runs() {
        let app = app_with_registry();
        let jobs = app.state::<AiJobs>();
        let _first = jobs
            .begin(app.handle(), "daisy-1", AiJobKind::Chapters)
            .expect("the first job claims the worker");

        let second = jobs.begin(app.handle(), "daisy-2", AiJobKind::Summary);
        let err = second.err().expect("the second job is refused");
        assert_eq!(err.code(), "DAISY-E019");

        let same_session = jobs.begin(app.handle(), "daisy-1", AiJobKind::Chapters);
        assert!(
            same_session.is_err(),
            "the gate is global, so the same session is refused too"
        );
    }

    #[test]
    fn the_list_reflects_the_running_job() {
        let app = app_with_registry();
        let jobs = app.state::<AiJobs>();
        assert!(jobs.list().is_empty());

        let guard = jobs
            .begin(app.handle(), "daisy-1", AiJobKind::Summary)
            .unwrap();
        let running = jobs.list();
        assert_eq!(running.len(), 1);
        assert_eq!(running[0].session_id, "daisy-1");
        assert_eq!(running[0].kind, AiJobKind::Summary);
        drop(guard);
    }

    #[test]
    fn dropping_the_guard_frees_the_worker() {
        let app = app_with_registry();
        let jobs = app.state::<AiJobs>();
        let guard = jobs
            .begin(app.handle(), "daisy-1", AiJobKind::Chapters)
            .unwrap();
        drop(guard);

        assert!(jobs.list().is_empty());
        jobs.begin(app.handle(), "daisy-2", AiJobKind::Summary)
            .expect("the next job claims the freed worker");
    }

    #[test]
    fn the_kind_serializes_snake_case() {
        let job = AiJob {
            session_id: "daisy-1".into(),
            kind: AiJobKind::Chapters,
            started_at_unix_seconds: 1_700_000_000,
        };
        let json = serde_json::to_string(&job).unwrap();
        assert!(json.contains(r#""kind":"chapters""#), "{json}");
        assert!(json.contains(r#""started_at_unix_seconds":1700000000"#), "{json}");
    }
}
