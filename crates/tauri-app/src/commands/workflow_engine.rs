//! Workflow trigger dispatch + durable serial run queue.
//!
//! dispatch() appends matching runs to <profile>/workflow_queue.json; a
//! single worker drains the queue serially.
use crate::error::{AppError, Result};
use crate::state::AppState;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use super::workflows::{ActionStep, SessionSnapshot, TriggerEvent, eval_condition, list_workflows_impl};

pub const MAX_ATTEMPTS: u32 = 2;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueuedRun {
    pub run_id: String,
    pub workflow_id: String,
    pub workflow_name: String,
    pub session_id: String,
    pub session_title: Option<String>,
    /// Tags captured at dispatch time.
    pub tag_ids: Vec<String>,
    pub trigger: TriggerEvent,
    pub actions: Vec<ActionStep>,
    pub attempts: u32,
    pub created_at_unix_seconds: i64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct QueueFile {
    #[serde(default = "default_schema")]
    pub schema_version: u32,
    #[serde(default)]
    pub runs: Vec<QueuedRun>,
}
fn default_schema() -> u32 {
    1
}

fn queue_path(app: &AppState) -> PathBuf {
    app.profile.root().join("workflow_queue.json")
}

pub fn load_queue(app: &AppState) -> Result<QueueFile> {
    let p = queue_path(app);
    if !p.is_file() {
        return Ok(QueueFile { schema_version: 1, runs: vec![] });
    }
    let bytes = syncsafe::read(&p)?;
    serde_json::from_slice(&bytes)
        .map_err(|e| AppError::Config(format!("parse workflow_queue.json: {e}")))
}

pub fn save_queue(app: &AppState, q: &QueueFile) -> Result<()> {
    let p = queue_path(app);
    let tmp = p.with_extension("json.tmp");
    syncsafe::write(&tmp, serde_json::to_vec_pretty(q)?)?;
    syncsafe::rename(&tmp, &p)?;
    Ok(())
}

/// Evaluate all enabled workflows for `trigger` against the snapshot; enqueue
/// matches. Returns how many runs were enqueued.
pub fn dispatch(app: &AppState, trigger: TriggerEvent, snap: &SessionSnapshot) -> Result<usize> {
    let matching: Vec<_> = list_workflows_impl(app)?
        .into_iter()
        .filter(|w| w.enabled && w.trigger == trigger && eval_condition(&w.condition, snap))
        .collect();
    if matching.is_empty() {
        return Ok(0);
    }
    let mut q = load_queue(app)?;
    let n = matching.len();
    for w in matching {
        q.runs.push(QueuedRun {
            run_id: uuid::Uuid::new_v4().to_string(),
            workflow_id: w.id,
            workflow_name: w.name,
            session_id: snap.session_id.clone(),
            session_title: snap.title.clone(),
            tag_ids: snap.tag_ids.clone(),
            trigger,
            actions: w.actions,
            attempts: 0,
            created_at_unix_seconds: crate::now_unix(),
        });
    }
    save_queue(app, &q)?;
    Ok(n)
}

/// First pending run, with attempts bumped and persisted. The run stays in
/// the file while executing; remove_run() removes it after completion.
pub fn take_next_run(app: &AppState) -> Option<QueuedRun> {
    let mut q = load_queue(app).ok()?;
    let run = q.runs.first_mut()?;
    run.attempts += 1;
    let out = run.clone();
    save_queue(app, &q).ok()?;
    Some(out)
}

pub fn remove_run(app: &AppState, run_id: &str) -> Result<()> {
    let mut q = load_queue(app)?;
    q.runs.retain(|r| r.run_id != run_id);
    save_queue(app, &q)
}

/// Startup pass: any run whose attempts reached MAX_ATTEMPTS gets a "gave-up"
/// history entry and is dropped. Runs under the cap stay queued.
pub fn recover_startup(app: &AppState) -> Result<()> {
    let q = load_queue(app)?;
    let (dead, alive): (Vec<_>, Vec<_>) = q.runs.into_iter().partition(|r| r.attempts >= MAX_ATTEMPTS);
    for r in &dead {
        let _ = super::workflow_history::append_run(app, &super::workflow_history::WorkflowRunRecord {
            run_id: r.run_id.clone(),
            at_unix_seconds: crate::now_unix(),
            workflow_id: r.workflow_id.clone(),
            workflow_name: r.workflow_name.clone(),
            session_id: r.session_id.clone(),
            session_title: r.session_title.clone(),
            trigger: r.trigger,
            status: "gave-up".into(),
            steps: vec![],
        });
    }
    if !dead.is_empty() {
        save_queue(app, &QueueFile { schema_version: 1, runs: alive })?;
    }
    Ok(())
}

// ---- step execution -----------------------------------------------------------

use crate::state::VaultState;

fn short_err(e: &dyn std::fmt::Display) -> String {
    let s = e.to_string().replace('\n', " ");
    let one: String = s.chars().take(200).collect();
    format!("error: {one}")
}

/// Event-details-only body for triggers where session content is unavailable
/// (deleted / not yet transcribed / failed).
fn metadata_payload(run: &QueuedRun) -> serde_json::Value {
    serde_json::json!({
        "source": "daisy",
        "event": run.trigger,
        "meeting": {
            "session_id": run.session_id,
            "title": run.session_title,
            "tag_ids": run.tag_ids,
        },
        "at_unix_seconds": crate::now_unix(),
    })
}

fn integration_name(vs: &VaultState, id: &str) -> String {
    super::integrations::get_integration(vs, id)
        .map(|i| i.name)
        .unwrap_or_else(|_| id.to_string())
}

fn prompt_name(app: &AppState, id: &str) -> String {
    super::prompts::load_prompts(app)
        .ok()
        .and_then(|ps| ps.into_iter().find(|p| p.id == id))
        .map(|p| p.name)
        .unwrap_or_else(|| id.to_string())
}

fn do_step(app: &AppState, vs: &VaultState, run: &QueuedRun, step: &ActionStep) -> Result<()> {
    match step {
        ActionStep::PushIntegration { integration_id } => {
            if run.trigger == TriggerEvent::Finalized {
                // Full payload from session files; `false` skips the
                // manual-send history entry.
                super::integrations::integration_push_impl(app, vs, &run.session_id, integration_id, false)
            } else {
                let integration = super::integrations::get_integration(vs, integration_id)?;
                let crate::state::IntegrationKind::Webhook { url, auth } = &integration.kind;
                super::integrations::push_webhook(url, auth, &metadata_payload(run))
            }
        }
        ActionStep::RunPrompt { prompt_id, send_to_integration, write_to_dir } => {
            let result = super::analysis::run_analysis_impl(app, vs, super::analysis::RunAnalysisRequest {
                session_id: run.session_id.clone(),
                prompt_id: Some(prompt_id.clone()),
                directive_md: None,
            })?;
            if let Some(iid) = send_to_integration {
                let integration = super::integrations::get_integration(vs, iid)?;
                let crate::state::IntegrationKind::Webhook { url, auth } = &integration.kind;
                let payload = serde_json::json!({
                    "source": "daisy",
                    "meeting": { "session_id": run.session_id, "title": run.session_title, "tag_ids": run.tag_ids },
                    "analysis": {
                        "prompt_id": result.prompt_id,
                        "prompt_name": result.prompt_name,
                        "markdown": result.markdown,
                    },
                });
                super::integrations::push_webhook(url, auth, &payload)?;
            }
            if let Some(dir) = write_to_dir {
                let date = chrono::DateTime::from_timestamp(crate::now_unix(), 0)
                    .map(|d| d.format("%Y-%m-%d").to_string())
                    .unwrap_or_else(|| "0000-00-00".into());
                let title = run.session_title.clone().unwrap_or_else(|| run.session_id.clone());
                let fname = format!(
                    "{date}-{}-{}.md",
                    super::analysis::sanitize_artifact_id(&title),
                    super::analysis::sanitize_artifact_id(prompt_id),
                );
                syncsafe::create_dir_all(dir)?;
                syncsafe::write(std::path::Path::new(dir).join(fname), result.markdown.as_bytes())?;
            }
            Ok(())
        }
    }
}

fn label_for(app: &AppState, vs: &VaultState, step: &ActionStep) -> String {
    match step {
        ActionStep::RunPrompt { prompt_id, .. } => format!("Run prompt: {}", prompt_name(app, prompt_id)),
        ActionStep::PushIntegration { integration_id } => {
            format!("Send to {}", integration_name(vs, integration_id))
        }
    }
}

/// Execute all steps of a run, reporting progress via `on_step(index, label,
/// "running")` before each step. Never errors — every failure is captured in
/// the per-step status; a failed step does not stop later steps.
pub fn execute_run_with_progress(
    app: &AppState,
    vs: &VaultState,
    run: &QueuedRun,
    on_step: &dyn Fn(usize, String, &str),
) -> super::workflow_history::WorkflowRunRecord {
    let mut steps = Vec::with_capacity(run.actions.len());
    for (i, step) in run.actions.iter().enumerate() {
        let label = label_for(app, vs, step);
        on_step(i, label.clone(), "running");
        let started = std::time::Instant::now();
        let status = match do_step(app, vs, run, step) {
            Ok(()) => "ok".to_string(),
            Err(e) => short_err(&e),
        };
        steps.push(super::workflow_history::StepRecord {
            label,
            status,
            duration_ms: started.elapsed().as_millis() as u64,
        });
    }
    let ok = steps.iter().filter(|s| s.status == "ok").count();
    let status = if ok == steps.len() { "ok" } else if ok > 0 { "partial" } else { "error" };
    super::workflow_history::WorkflowRunRecord {
        run_id: run.run_id.clone(),
        at_unix_seconds: crate::now_unix(),
        workflow_id: run.workflow_id.clone(),
        workflow_name: run.workflow_name.clone(),
        session_id: run.session_id.clone(),
        session_title: run.session_title.clone(),
        trigger: run.trigger,
        status: status.into(),
        steps,
    }
}

pub fn execute_run(
    app: &AppState,
    vs: &VaultState,
    run: &QueuedRun,
) -> super::workflow_history::WorkflowRunRecord {
    execute_run_with_progress(app, vs, run, &|_, _, _| {})
}

// ---- worker --------------------------------------------------------------------

/// Managed handle; wakes the worker after enqueueing.
pub struct WorkflowEngineHandle(pub std::sync::Arc<tokio::sync::Notify>);

#[derive(Debug, Clone, Serialize)]
pub struct WorkflowRunEvent {
    pub run_id: String,
    pub workflow_name: String,
    pub session_id: String,
    pub step_index: usize,
    pub step_count: usize,
    pub step_label: String,
    /// "running" while stepping; terminal = the run's history status.
    pub status: String,
}

/// Serial queue worker. Wakes on notify or every 30 s; drains one run at a
/// time on a blocking thread. Never more than one run in flight.
pub fn spawn_worker(app: tauri::AppHandle) -> std::sync::Arc<tokio::sync::Notify> {
    use tauri::{Emitter, Manager};
    let notify = std::sync::Arc::new(tokio::sync::Notify::new());
    let n = notify.clone();
    tauri::async_runtime::spawn(async move {
        {
            let state = app.state::<AppState>();
            if let Err(e) = recover_startup(&state) {
                log::warn!("workflow recover_startup: {e}");
            }
        }
        loop {
            tokio::select! {
                _ = n.notified() => {}
                _ = tokio::time::sleep(std::time::Duration::from_secs(30)) => {}
            }
            loop {
                if !app.state::<VaultState>().is_unlocked() {
                    break; // the 30 s tick re-checks after unlock
                }
                let run = take_next_run(&app.state::<AppState>());
                let Some(run) = run else { break };
                let h = app.clone();
                let joined = tauri::async_runtime::spawn_blocking(move || {
                    let state = h.state::<AppState>();
                    let vault = h.state::<VaultState>();
                    let total = run.actions.len();
                    let ev = |step_index: usize, step_label: String, status: &str| {
                        let _ = h.emit("workflow:run", WorkflowRunEvent {
                            run_id: run.run_id.clone(),
                            workflow_name: run.workflow_name.clone(),
                            session_id: run.session_id.clone(),
                            step_index,
                            step_count: total,
                            step_label,
                            status: status.into(),
                        });
                    };
                    let rec = execute_run_with_progress(&state, &vault, &run, &ev);
                    let _ = super::workflow_history::append_run(&state, &rec);
                    let _ = remove_run(&state, &run.run_id);
                    ev(total, String::new(), &rec.status);
                })
                .await;
                if let Err(e) = joined {
                    log::warn!("workflow run task panicked/cancelled: {e}");
                }
            }
        }
    });
    notify
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::workflows::*;

    fn app() -> (crate::state::AppState, tempfile::TempDir) {
        let tmp = tempfile::tempdir().unwrap();
        let profile = crate::profile::ProfileDir::at(tmp.path()).unwrap();
        (crate::state::AppState::new(profile), tmp)
    }

    fn seed_workflow(
        app: &crate::state::AppState,
        trigger: TriggerEvent,
        condition: ConditionNode,
        enabled: bool,
    ) -> Workflow {
        upsert_workflow_impl(app, Workflow {
            id: String::new(),
            name: "W".into(),
            enabled,
            trigger,
            condition,
            actions: vec![ActionStep::PushIntegration { integration_id: "i1".into() }],
            created_at_unix_seconds: 0,
        })
        .unwrap()
    }

    fn snap() -> SessionSnapshot {
        SessionSnapshot {
            session_id: "s1".into(),
            title: Some("Acme kickoff".into()),
            tag_ids: vec!["t-acme".into()],
            contact_ids: Default::default(),
        }
    }

    #[test]
    fn dispatch_enqueues_only_matching_enabled_workflows() {
        let (app, _t) = app();
        seed_workflow(&app, TriggerEvent::Finalized, ConditionNode::All { children: vec![] }, true);
        seed_workflow(&app, TriggerEvent::Deleted, ConditionNode::All { children: vec![] }, true); // wrong trigger
        seed_workflow(&app, TriggerEvent::Finalized, ConditionNode::All { children: vec![] }, false); // disabled
        seed_workflow(&app, TriggerEvent::Finalized,
            ConditionNode::HasTag { tag_id: "t-other".into() }, true); // condition miss
        let n = dispatch(&app, TriggerEvent::Finalized, &snap()).unwrap();
        assert_eq!(n, 1);
        let q = load_queue(&app).unwrap();
        assert_eq!(q.runs.len(), 1);
        assert_eq!(q.runs[0].session_id, "s1");
        assert_eq!(q.runs[0].tag_ids, vec!["t-acme"]);
        assert_eq!(q.runs[0].attempts, 0);
    }

    #[test]
    fn take_next_bumps_attempts_and_keeps_in_file_remove_drops() {
        let (app, _t) = app();
        seed_workflow(&app, TriggerEvent::Finalized, ConditionNode::All { children: vec![] }, true);
        dispatch(&app, TriggerEvent::Finalized, &snap()).unwrap();
        let run = take_next_run(&app).unwrap();
        assert_eq!(run.attempts, 1);
        // Still persisted: the run survives until remove_run.
        assert_eq!(load_queue(&app).unwrap().runs.len(), 1);
        assert_eq!(load_queue(&app).unwrap().runs[0].attempts, 1);
        remove_run(&app, &run.run_id).unwrap();
        assert!(load_queue(&app).unwrap().runs.is_empty());
        assert!(take_next_run(&app).is_none());
    }

    /// Unlocked vault with one webhook integration pointing at `url`.
    fn vault_with_webhook(url: &str) -> crate::state::VaultState {
        use crate::state::{Integration, IntegrationKind, PayloadSelection, WebhookAuth};
        let vs = crate::state::VaultState::default();
        *vs.keys.lock().unwrap() = Some(Default::default());
        *vs.passphrase.lock().unwrap() = Some(zeroize::Zeroizing::new("correct horse battery staple".into()));
        vs.keys.lock().unwrap().as_mut().unwrap().integrations.push(Integration {
            id: "i1".into(),
            name: "Ops hook".into(),
            enabled: true,
            kind: IntegrationKind::Webhook { url: url.into(), auth: WebhookAuth::None },
            payloads: PayloadSelection { summary: true, notes: false, transcript: false },
        });
        vs
    }

    fn queued(trigger: TriggerEvent, actions: Vec<ActionStep>) -> QueuedRun {
        QueuedRun {
            run_id: "r1".into(),
            workflow_id: "w1".into(),
            workflow_name: "Notify ops".into(),
            session_id: "s1".into(),
            session_title: Some("Acme kickoff".into()),
            tag_ids: vec!["t-acme".into()],
            trigger,
            actions,
            attempts: 1,
            created_at_unix_seconds: 0,
        }
    }

    #[test]
    fn deleted_trigger_pushes_metadata_payload() {
        let (app, _t) = app();
        let mut server = mockito::Server::new();
        let m = server.mock("POST", "/")
            .match_body(mockito::Matcher::PartialJson(serde_json::json!({
                "source": "daisy",
                "event": "deleted",
                "meeting": { "session_id": "s1", "title": "Acme kickoff", "tag_ids": ["t-acme"] },
            })))
            .with_status(200)
            .create();
        let vs = vault_with_webhook(&server.url());
        // No session dir on disk — deleted. Must still succeed.
        let rec = execute_run(&app, &vs, &queued(TriggerEvent::Deleted, vec![
            ActionStep::PushIntegration { integration_id: "i1".into() },
        ]));
        m.assert();
        assert_eq!(rec.status, "ok", "{:?}", rec.steps);
        assert_eq!(rec.steps.len(), 1);
        assert_eq!(rec.steps[0].status, "ok");
    }

    #[test]
    fn failed_step_recorded_and_later_steps_still_run() {
        let (app, _t) = app();
        let mut server = mockito::Server::new();
        let m = server.mock("POST", "/").with_status(200).expect(1).create();
        let vs = vault_with_webhook(&server.url());
        let rec = execute_run(&app, &vs, &queued(TriggerEvent::Deleted, vec![
            ActionStep::PushIntegration { integration_id: "missing".into() }, // fails
            ActionStep::PushIntegration { integration_id: "i1".into() },      // still runs
        ]));
        m.assert();
        assert_eq!(rec.status, "partial");
        assert!(rec.steps[0].status.starts_with("error:"), "{}", rec.steps[0].status);
        assert_eq!(rec.steps[1].status, "ok");
    }

    #[test]
    fn all_steps_failing_is_error_status() {
        let (app, _t) = app();
        let vs = vault_with_webhook("http://127.0.0.1:1"); // nothing listening
        let rec = execute_run(&app, &vs, &queued(TriggerEvent::Deleted, vec![
            ActionStep::PushIntegration { integration_id: "missing".into() },
        ]));
        assert_eq!(rec.status, "error");
    }

    #[test]
    fn run_prompt_writes_artifact_and_optional_file_copy() {
        // Mock OpenAI-compatible provider, session with transcript, RunPrompt
        // with write_to_dir.
        let (app, _t) = app();
        let dir = app.profile.session_path("s1");
        syncsafe::create_dir_all(&dir).unwrap();
        syncsafe::write(dir.join("transcript.md"), "**Me**: ship the pilot").unwrap();
        let out_dir = tempfile::tempdir().unwrap();

        let mut server = mockito::Server::new();
        let content = serde_json::json!({
            "exec_summary": "Pilot shipping.",
            "sections": [{"heading": "Decisions", "bullets": true, "content": ["ship it"]}],
            "action_items": []
        }).to_string();
        server.mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_body(serde_json::json!({"choices":[{"message":{"content": content}}]}).to_string())
            .create();

        let vs = vault_with_webhook("http://127.0.0.1:1");
        // Point lm_studio at the mock server.
        {
            use crate::state::{ProviderConfig, ProviderId};
            let sp = app.profile.settings_path();
            let mut settings = crate::settings::Settings::load_or_default(&sp);
            settings.default_summary_provider = Some(ProviderId::LmStudio);
            settings.save(&sp).unwrap();
            vs.keys.lock().unwrap().as_mut().unwrap().providers.insert(
                ProviderId::LmStudio,
                ProviderConfig { api_key: None, model: None, base_url: Some(server.url()) },
            );
        }

        let rec = execute_run(&app, &vs, &queued(TriggerEvent::Finalized, vec![
            ActionStep::RunPrompt {
                prompt_id: "builtin:pm".into(),
                send_to_integration: None,
                write_to_dir: Some(out_dir.path().to_string_lossy().into_owned()),
            },
        ]));
        assert_eq!(rec.status, "ok", "{:?}", rec.steps);
        assert!(dir.join("analyses").join("builtin-pm.md").is_file(), "artifact stored with session");
        let copies: Vec<_> = std::fs::read_dir(out_dir.path()).unwrap().flatten().collect();
        assert_eq!(copies.len(), 1, "one exported copy");
        assert!(copies[0].file_name().to_string_lossy().ends_with(".md"));
    }

    #[test]
    fn recover_startup_gives_up_after_attempt_cap() {
        let (app, _t) = app();
        seed_workflow(&app, TriggerEvent::Finalized, ConditionNode::All { children: vec![] }, true);
        dispatch(&app, TriggerEvent::Finalized, &snap()).unwrap();
        // Simulate two prior crashed attempts.
        let mut q = load_queue(&app).unwrap();
        q.runs[0].attempts = 2;
        save_queue(&app, &q).unwrap();
        recover_startup(&app).unwrap();
        assert!(load_queue(&app).unwrap().runs.is_empty(), "capped run dropped from queue");
        let hist = crate::commands::workflow_history::read_runs(&app, 10, 0).unwrap();
        assert_eq!(hist.len(), 1);
        assert_eq!(hist[0].status, "gave-up");
    }
}
