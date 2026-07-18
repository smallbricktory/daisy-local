//! Workflow CRUD over a plaintext `<profile>/workflows.json` file.
//! Workflows are "When <event>, if <conditions>, do <actions>" automation
//! rules; they reference integrations/prompts/tags by id.
use crate::error::{AppError, Result};
use crate::state::AppState;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TriggerEvent {
    Finalized,
    Deleted,
    Imported,
    FinalizeFailed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ConditionNode {
    All { children: Vec<ConditionNode> },
    Any { children: Vec<ConditionNode> },
    HasTag { tag_id: String },
    HasParticipant { contact_id: String },
    TitleContains { needle: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ActionStep {
    RunPrompt {
        /// The artifact is written under the session's analyses/ directory.
        prompt_id: String,
        /// Also POST the artifact markdown to this integration.
        send_to_integration: Option<String>,
        /// Also write a copy of the artifact into this directory.
        write_to_dir: Option<String>,
    },
    PushIntegration {
        integration_id: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Workflow {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub trigger: TriggerEvent,
    pub condition: ConditionNode,
    pub actions: Vec<ActionStep>,
    pub created_at_unix_seconds: i64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct WorkflowsFile {
    #[serde(default = "default_schema")]
    pub schema_version: u32,
    #[serde(default)]
    pub workflows: Vec<Workflow>,
}
fn default_schema() -> u32 {
    1
}

pub fn workflows_path(app: &AppState) -> PathBuf {
    app.profile.root().join("workflows.json")
}

pub fn load_workflows_file(app: &AppState) -> Result<WorkflowsFile> {
    let p = workflows_path(app);
    if !p.is_file() {
        return Ok(WorkflowsFile { schema_version: 1, workflows: vec![] });
    }
    let bytes = syncsafe::read(&p)?;
    serde_json::from_slice::<WorkflowsFile>(&bytes)
        .map_err(|e| AppError::Config(format!("parse workflows.json: {e}")))
}

pub fn save_workflows_file(app: &AppState, file: &WorkflowsFile) -> Result<()> {
    let p = workflows_path(app);
    let tmp = p.with_extension("json.tmp");
    let bytes = serde_json::to_vec_pretty(file)?;
    syncsafe::write(&tmp, &bytes)?;
    syncsafe::rename(&tmp, &p)?;
    Ok(())
}

pub fn list_workflows_impl(app: &AppState) -> Result<Vec<Workflow>> {
    Ok(load_workflows_file(app)?.workflows)
}

fn validate(w: &Workflow) -> Result<()> {
    if w.name.trim().is_empty() {
        return Err(AppError::Config("workflow needs a name".into()));
    }
    if w.actions.is_empty() {
        return Err(AppError::Config("workflow needs at least one action".into()));
    }
    let has_prompt = w.actions.iter().any(|a| matches!(a, ActionStep::RunPrompt { .. }));
    if has_prompt && w.trigger != TriggerEvent::Finalized {
        return Err(AppError::Config(
            "Run-prompt steps need a transcript — only available on the \"Recording finalized\" trigger".into(),
        ));
    }
    Ok(())
}

pub fn upsert_workflow_impl(app: &AppState, mut w: Workflow) -> Result<Workflow> {
    validate(&w)?;
    let mut file = load_workflows_file(app)?;
    if w.id.is_empty() {
        w.id = uuid::Uuid::new_v4().to_string();
        w.created_at_unix_seconds = crate::now_unix();
        file.workflows.push(w.clone());
    } else {
        let slot = file
            .workflows
            .iter_mut()
            .find(|x| x.id == w.id)
            .ok_or_else(|| AppError::Config(format!("no such workflow: {}", w.id)))?;
        w.created_at_unix_seconds = slot.created_at_unix_seconds;
        *slot = w.clone();
    }
    save_workflows_file(app, &file)?;
    Ok(w)
}

/// What condition evaluation (and metadata payloads) see. Captured at event
/// time; for Deleted it must be captured BEFORE the session dir is removed.
#[derive(Debug, Clone)]
pub struct SessionSnapshot {
    pub session_id: String,
    pub title: Option<String>,
    pub tag_ids: Vec<String>,
    pub contact_ids: std::collections::HashSet<String>,
}

pub fn eval_condition(node: &ConditionNode, s: &SessionSnapshot) -> bool {
    match node {
        // An empty All evaluates to true.
        ConditionNode::All { children } => children.iter().all(|c| eval_condition(c, s)),
        ConditionNode::Any { children } => children.iter().any(|c| eval_condition(c, s)),
        ConditionNode::HasTag { tag_id } => s.tag_ids.iter().any(|t| t == tag_id),
        ConditionNode::HasParticipant { contact_id } => s.contact_ids.contains(contact_id),
        ConditionNode::TitleContains { needle } => s
            .title
            .as_deref()
            .map(|t| t.to_lowercase().contains(&needle.to_lowercase()))
            .unwrap_or(false),
    }
}

pub fn snapshot_for_session(app: &AppState, session_id: &str) -> Result<SessionSnapshot> {
    let manifest = crate::commands::summary::load_manifest(app, session_id)
        .map_err(|e| AppError::Config(format!("snapshot {session_id}: {e}")))?;
    let contacts = crate::commands::contacts::load_contacts(app).unwrap_or_default();
    let contact_ids = crate::commands::contacts::session_contact_ids(&manifest, &contacts);
    Ok(SessionSnapshot {
        session_id: session_id.to_string(),
        title: manifest.title.clone(),
        tag_ids: manifest.tag_ids.clone(),
        contact_ids,
    })
}

pub fn delete_workflow_impl(app: &AppState, id: &str) -> Result<()> {
    let mut file = load_workflows_file(app)?;
    let before = file.workflows.len();
    file.workflows.retain(|w| w.id != id);
    if file.workflows.len() == before {
        return Err(AppError::Config(format!("no such workflow: {id}")));
    }
    save_workflows_file(app, &file)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> (crate::state::AppState, tempfile::TempDir) {
        let tmp = tempfile::tempdir().unwrap();
        let profile = crate::profile::ProfileDir::at(tmp.path()).unwrap();
        (crate::state::AppState::new(profile), tmp)
    }

    fn wf(trigger: TriggerEvent, actions: Vec<ActionStep>) -> Workflow {
        Workflow {
            id: String::new(), // empty = create
            name: "Design specs".into(),
            enabled: true,
            trigger,
            condition: ConditionNode::All { children: vec![] },
            actions,
            created_at_unix_seconds: 0,
        }
    }

    #[test]
    fn upsert_assigns_id_and_round_trips() {
        let (app, _t) = app();
        let saved = upsert_workflow_impl(&app, wf(TriggerEvent::Finalized, vec![
            ActionStep::RunPrompt { prompt_id: "builtin:pm".into(), send_to_integration: None, write_to_dir: None },
        ])).unwrap();
        assert!(!saved.id.is_empty());
        assert!(saved.created_at_unix_seconds > 0);
        let listed = list_workflows_impl(&app).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, "Design specs");
    }

    #[test]
    fn upsert_with_existing_id_updates_in_place() {
        let (app, _t) = app();
        let mut saved = upsert_workflow_impl(&app, wf(TriggerEvent::Finalized, vec![
            ActionStep::PushIntegration { integration_id: "i1".into() },
        ])).unwrap();
        saved.name = "Renamed".into();
        saved.enabled = false;
        let again = upsert_workflow_impl(&app, saved.clone()).unwrap();
        assert_eq!(again.id, saved.id);
        let listed = list_workflows_impl(&app).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, "Renamed");
        assert!(!listed[0].enabled);
    }

    #[test]
    fn upsert_unknown_id_errors() {
        let (app, _t) = app();
        let mut w = wf(TriggerEvent::Finalized, vec![
            ActionStep::PushIntegration { integration_id: "i1".into() },
        ]);
        w.id = "nope".into();
        assert!(upsert_workflow_impl(&app, w).is_err());
    }

    #[test]
    fn run_prompt_only_valid_on_finalized_trigger() {
        let (app, _t) = app();
        let bad = wf(TriggerEvent::Deleted, vec![
            ActionStep::RunPrompt { prompt_id: "builtin:pm".into(), send_to_integration: None, write_to_dir: None },
        ]);
        let err = upsert_workflow_impl(&app, bad).unwrap_err();
        assert!(err.to_string().contains("Run-prompt"), "{err}");
    }

    #[test]
    fn name_required_and_at_least_one_action() {
        let (app, _t) = app();
        let mut unnamed = wf(TriggerEvent::Finalized, vec![
            ActionStep::PushIntegration { integration_id: "i1".into() },
        ]);
        unnamed.name = "  ".into();
        assert!(upsert_workflow_impl(&app, unnamed).is_err());
        assert!(upsert_workflow_impl(&app, wf(TriggerEvent::Finalized, vec![])).is_err());
    }

    #[test]
    fn delete_removes() {
        let (app, _t) = app();
        let saved = upsert_workflow_impl(&app, wf(TriggerEvent::Finalized, vec![
            ActionStep::PushIntegration { integration_id: "i1".into() },
        ])).unwrap();
        delete_workflow_impl(&app, &saved.id).unwrap();
        assert!(list_workflows_impl(&app).unwrap().is_empty());
    }

    fn snap(tags: &[&str], contacts: &[&str], title: Option<&str>) -> SessionSnapshot {
        SessionSnapshot {
            session_id: "s1".into(),
            title: title.map(|s| s.to_string()),
            tag_ids: tags.iter().map(|s| s.to_string()).collect(),
            contact_ids: contacts.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn empty_all_always_fires_empty_any_never() {
        let s = snap(&[], &[], None);
        assert!(eval_condition(&ConditionNode::All { children: vec![] }, &s));
        assert!(!eval_condition(&ConditionNode::Any { children: vec![] }, &s));
    }

    #[test]
    fn leaves_match_snapshot() {
        let s = snap(&["t-client-a"], &["c-joe"], Some("Q3 Design Spec Review"));
        assert!(eval_condition(&ConditionNode::HasTag { tag_id: "t-client-a".into() }, &s));
        assert!(!eval_condition(&ConditionNode::HasTag { tag_id: "t-other".into() }, &s));
        assert!(eval_condition(&ConditionNode::HasParticipant { contact_id: "c-joe".into() }, &s));
        assert!(!eval_condition(&ConditionNode::HasParticipant { contact_id: "c-x".into() }, &s));
        // case-insensitive substring; None title never matches
        assert!(eval_condition(&ConditionNode::TitleContains { needle: "design spec".into() }, &s));
        assert!(!eval_condition(&ConditionNode::TitleContains { needle: "spec".into() }, &snap(&[], &[], None)));
    }

    #[test]
    fn nested_boolean_tree_evaluates() {
        // (tag=A AND (tag=B OR participant=joe))
        let tree = ConditionNode::All { children: vec![
            ConditionNode::HasTag { tag_id: "A".into() },
            ConditionNode::Any { children: vec![
                ConditionNode::HasTag { tag_id: "B".into() },
                ConditionNode::HasParticipant { contact_id: "joe".into() },
            ]},
        ]};
        assert!(eval_condition(&tree, &snap(&["A"], &["joe"], None)));
        assert!(eval_condition(&tree, &snap(&["A", "B"], &[], None)));
        assert!(!eval_condition(&tree, &snap(&["A"], &[], None)));
        assert!(!eval_condition(&tree, &snap(&["B"], &["joe"], None)));
    }

    #[test]
    fn snapshot_reads_manifest_tags_and_title() {
        let (app, _t) = app();
        // Real manifest via the import path.
        let sid = crate::commands::meeting::import_session_impl(
            &app,
            crate::commands::meeting::ImportSessionRequest {
                title: Some("Acme kickoff".into()),
                occurred_at: Some(1_700_000_000),
                transcript_md: Some("**Mira:** hello".into()),
                summary_md: None,
                notes_md: None,
                tag_ids: vec!["t-acme".into()],
            },
        )
        .unwrap();
        let s = snapshot_for_session(&app, &sid).unwrap();
        assert_eq!(s.title.as_deref(), Some("Acme kickoff"));
        assert_eq!(s.tag_ids, vec!["t-acme"]);
    }

    #[test]
    fn condition_serde_shape_is_internally_tagged() {
        let node = ConditionNode::Any { children: vec![
            ConditionNode::HasTag { tag_id: "t1".into() },
            ConditionNode::TitleContains { needle: "spec".into() },
        ]};
        let v = serde_json::to_value(&node).unwrap();
        assert_eq!(v["type"], "any");
        assert_eq!(v["children"][0]["type"], "has_tag");
        assert_eq!(v["children"][0]["tag_id"], "t1");
        let back: ConditionNode = serde_json::from_value(v).unwrap();
        assert!(matches!(back, ConditionNode::Any { .. }));
    }
}
