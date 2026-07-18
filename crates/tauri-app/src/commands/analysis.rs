//! Run a prompt (stored or ad-hoc) over a session's transcript via the shared
//! sectioned summary engine, persisting the output as a per-session artifact:
//!   sessions/<sid>/analyses/<sanitized-prompt-id>.{md,json}
use crate::error::{AppError, Result};
use crate::settings::Settings;
use crate::state::{AppState, VaultState};
use serde::{Deserialize, Serialize};
use summarize::prompts::Envelope;
use summarize::{AttendeeRef, SummaryInput};

pub const ADHOC_ID: &str = "adhoc";

/// Windows-safe artifact basename for a prompt id (`builtin:pm` → `builtin-pm`).
pub fn sanitize_artifact_id(id: &str) -> String {
    id.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '-'
            }
        })
        .collect()
}

#[derive(Debug, Deserialize)]
pub struct RunAnalysisRequest {
    pub session_id: String,
    /// Stored prompt to run…
    pub prompt_id: Option<String>,
    /// …or an ad-hoc one-time directive (used when prompt_id is None).
    pub directive_md: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalysisResult {
    pub prompt_id: String,
    pub prompt_name: String,
    pub markdown: String,
    pub generated_at_unix_seconds: i64,
}

pub fn run_analysis_impl(
    app: &AppState,
    vs: &VaultState,
    req: RunAnalysisRequest,
) -> Result<AnalysisResult> {
    if !vs.is_unlocked() {
        return Err(AppError::VaultLocked);
    }
    let session_dir = app.profile.session_path(&req.session_id);
    let transcript_md = syncsafe::read_to_string(session_dir.join("transcript.md")).map_err(|_| {
        AppError::Config(format!(
            "session {}: no transcript.md (run transcribe first)",
            req.session_id
        ))
    })?;

    // Stored prompt or ad-hoc directive. Ad-hoc must be non-empty. An explicit
    // prompt_id that no longer resolves is an error.
    let (pid, pname, directive) = match (&req.prompt_id, &req.directive_md) {
        (Some(id), _) => {
            let p = crate::commands::prompts::load_prompts(app)?
                .into_iter()
                .find(|p| &p.id == id)
                .ok_or_else(|| {
                    AppError::Config(format!("unknown prompt: {id} (was it deleted?)"))
                })?;
            (p.id, p.name, p.directive_md)
        }
        (None, Some(d)) if !d.trim().is_empty() => (ADHOC_ID.to_string(), "Ad Hoc".to_string(), d.clone()),
        _ => return Err(AppError::Config("provide a prompt_id or a non-empty directive".into())),
    };

    let manifest = crate::commands::summary::load_manifest(app, &req.session_id).ok();
    let title = manifest.as_ref().and_then(|m| m.title.clone());
    let att_owned: Vec<(String, summarize::SpeakerRole)> = manifest
        .as_ref()
        .map(|m| m.attendees.iter().map(crate::commands::summary::attendee_to_role).collect())
        .unwrap_or_default();
    let att_refs: Vec<AttendeeRef> = att_owned
        .iter()
        .map(|(n, r)| AttendeeRef { display_name: n, role: *r })
        .collect();

    let settings = Settings::load_or_default(&app.profile.settings_path());
    let provider = settings.default_summary_provider.ok_or_else(|| {
        AppError::Config("No AI provider configured. Pick one in Settings → Providers.".into())
    })?;
    let completer = crate::commands::summary::build_ai_completer(&settings, vs, "analysis")?;
    let model = summarize::defaults::for_provider(provider.as_str())
        .map(|d| d.model.to_string())
        .unwrap_or_else(|| "local-model".into());

    let input = SummaryInput {
        session_id: &req.session_id,
        title: title.as_deref(),
        attendees: &att_refs,
        user_notes_md: "",
        transcript_md: &transcript_md,
        tag_prompts: &[],
        style_envelope: Envelope::Sectioned,
        style_directive: &directive,
    };
    let out = summarize::engine::drive(
        completer.as_ref(),
        provider.as_str(),
        &model,
        &input,
        crate::now_unix(),
        "Respond with a single JSON object only — every list field must be a JSON array, with no XML/HTML tags inside the values.",
    )
    .map_err(|e| crate::commands::summary::map_gateway_err(provider, "analysis", e))?;

    let result = AnalysisResult {
        prompt_id: pid.clone(),
        prompt_name: pname,
        markdown: out.markdown.clone(),
        generated_at_unix_seconds: out.generated_at_unix_seconds,
    };
    let dir = session_dir.join("analyses");
    syncsafe::create_dir_all(&dir)?;
    let base = sanitize_artifact_id(&pid);
    syncsafe::write(dir.join(format!("{base}.md")), result.markdown.as_bytes())?;
    syncsafe::write(dir.join(format!("{base}.json")), serde_json::to_vec_pretty(&result)?)?;
    Ok(result)
}

pub fn analysis_load_impl(
    app: &AppState,
    session_id: &str,
    prompt_id: &str,
) -> Result<Option<AnalysisResult>> {
    let p = app
        .profile
        .session_path(session_id)
        .join("analyses")
        .join(format!("{}.json", sanitize_artifact_id(prompt_id)));
    if !p.is_file() {
        return Ok(None);
    }
    let bytes = syncsafe::read(&p)?;
    Ok(serde_json::from_slice(&bytes).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> (AppState, tempfile::TempDir) {
        let tmp = tempfile::tempdir().unwrap();
        let profile = crate::profile::ProfileDir::at(tmp.path()).unwrap();
        (AppState::new(profile), tmp)
    }

    /// Unlocked vault + a session with a transcript — shared setup for the
    /// impl-level tests.
    fn app_with_session(transcript: &str) -> (AppState, crate::state::VaultState, tempfile::TempDir) {
        let (app, tmp) = app();
        let dir = app.profile.session_path("s1");
        syncsafe::create_dir_all(&dir).unwrap();
        syncsafe::write(dir.join("transcript.md"), transcript).unwrap();
        let vs = crate::state::VaultState::default();
        *vs.keys.lock().unwrap() = Some(Default::default());
        *vs.passphrase.lock().unwrap() = Some(zeroize::Zeroizing::new("correct horse battery staple".into()));
        (app, vs, tmp)
    }

    #[test]
    fn rejects_missing_prompt_and_directive() {
        let (app, vs, _t) = app_with_session("**Me**: hi");
        let err = run_analysis_impl(&app, &vs, RunAnalysisRequest {
            session_id: "s1".into(),
            prompt_id: None,
            directive_md: None,
        })
        .unwrap_err();
        assert!(err.to_string().contains("prompt_id or a non-empty directive"), "{err}");
        // Blank ad-hoc text is treated the same as absent.
        let err = run_analysis_impl(&app, &vs, RunAnalysisRequest {
            session_id: "s1".into(),
            prompt_id: None,
            directive_md: Some("   ".into()),
        })
        .unwrap_err();
        assert!(err.to_string().contains("prompt_id or a non-empty directive"), "{err}");
    }

    #[test]
    fn vault_locked_and_missing_transcript_error_clearly() {
        let (app, _t) = app();
        // Locked vault.
        let locked = crate::state::VaultState::default();
        let err = run_analysis_impl(&app, &locked, RunAnalysisRequest {
            session_id: "s1".into(), prompt_id: None, directive_md: Some("x".into()),
        })
        .unwrap_err();
        assert!(matches!(err, AppError::VaultLocked), "{err}");
        // Unlocked but no transcript on disk.
        let vs = crate::state::VaultState::default();
        *vs.keys.lock().unwrap() = Some(Default::default());
        let err = run_analysis_impl(&app, &vs, RunAnalysisRequest {
            session_id: "nope".into(), prompt_id: None, directive_md: Some("x".into()),
        })
        .unwrap_err();
        assert!(err.to_string().contains("no transcript.md"), "{err}");
    }

    #[test]
    fn unknown_prompt_id_errors_instead_of_silently_running_daisy() {
        let (app, vs, _t) = app_with_session("**Me**: hi");
        let err = run_analysis_impl(&app, &vs, RunAnalysisRequest {
            session_id: "s1".into(),
            prompt_id: Some("deleted-prompt".into()),
            directive_md: None,
        })
        .unwrap_err();
        assert!(err.to_string().contains("unknown prompt"), "{err}");
    }

    #[test]
    fn provider_500_maps_to_a_config_error_and_writes_no_artifact() {
        let (app, vs, _t) = app_with_session("**Me**: hi");
        let mut server = mockito::Server::new();
        server
            .mock("POST", "/v1/chat/completions")
            .with_status(500)
            .with_body("boom")
            .expect_at_least(1)
            .create();
        mock_provider(&app, &vs, &server);
        let err = run_analysis_impl(&app, &vs, RunAnalysisRequest {
            session_id: "s1".into(), prompt_id: None, directive_md: Some("x".into()),
        })
        .unwrap_err();
        assert!(err.to_string().contains("analysis"), "{err}");
        assert!(!app.profile.session_path("s1").join("analyses").join("adhoc.md").exists());
    }

    /// Point the lm_studio provider at a mock OpenAI-compatible server and run
    /// the full path: resolve prompt → engine → deterministic render → artifact.
    fn mock_provider(app: &AppState, vs: &crate::state::VaultState, server: &mockito::Server) {
        use crate::state::{ProviderConfig, ProviderId};
        let sp = app.profile.settings_path();
        let mut settings = crate::settings::Settings::load_or_default(&sp);
        settings.default_summary_provider = Some(ProviderId::LmStudio);
        settings.save(&sp).unwrap();
        vs.keys.lock().unwrap().as_mut().unwrap().providers.insert(
            ProviderId::LmStudio,
            ProviderConfig { api_key: None, model: None, base_url: Some(server.url()), },
        );
    }

    fn sectioned_response() -> String {
        // choices[0].message.content = the sectioned envelope as a JSON string.
        let content = serde_json::json!({
            "exec_summary": "A focused sync.",
            "sections": [
                {"heading": "Pilot scope", "bullets": true, "content": ["trimmed list agreed"]}
            ],
            "action_items": []
        })
        .to_string();
        serde_json::json!({"choices": [{"message": {"content": content}}]}).to_string()
    }

    #[test]
    fn adhoc_run_hits_provider_and_writes_artifact() {
        let (app, vs, _t) = app_with_session("**Me**: let's trim the pilot scope");
        let mut server = mockito::Server::new();
        let m = server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_body(sectioned_response())
            .create();
        mock_provider(&app, &vs, &server);

        let out = run_analysis_impl(&app, &vs, RunAnalysisRequest {
            session_id: "s1".into(),
            prompt_id: None,
            directive_md: Some("List scope decisions.".into()),
        })
        .unwrap();
        m.assert();
        assert_eq!(out.prompt_id, ADHOC_ID);
        assert_eq!(out.prompt_name, "Ad Hoc");
        assert!(out.markdown.contains("## Pilot scope"), "{}", out.markdown);
        assert!(out.markdown.contains("- trimmed list agreed"));
        // Artifact persisted under the reserved id.
        let dir = app.profile.session_path("s1").join("analyses");
        assert!(dir.join("adhoc.md").is_file());
        let loaded = analysis_load_impl(&app, "s1", ADHOC_ID).unwrap().unwrap();
        assert_eq!(loaded.markdown, out.markdown);
    }

    #[test]
    fn stored_prompt_run_uses_prompt_identity_and_sanitized_filename() {
        let (app, vs, _t) = app_with_session("**Me**: status update");
        let mut server = mockito::Server::new();
        server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_body(sectioned_response())
            .create();
        mock_provider(&app, &vs, &server);

        let out = run_analysis_impl(&app, &vs, RunAnalysisRequest {
            session_id: "s1".into(),
            prompt_id: Some("builtin:pm".into()),
            directive_md: None,
        })
        .unwrap();
        assert_eq!(out.prompt_id, "builtin:pm");
        assert_eq!(out.prompt_name, "Project Manager coaching");
        let dir = app.profile.session_path("s1").join("analyses");
        assert!(dir.join("builtin-pm.json").is_file(), "colon sanitized for Windows");
        let loaded = analysis_load_impl(&app, "s1", "builtin:pm").unwrap().unwrap();
        assert_eq!(loaded.prompt_name, "Project Manager coaching");
    }

    #[test]
    fn sanitizes_windows_hostile_ids() {
        assert_eq!(sanitize_artifact_id("builtin:pm"), "builtin-pm");
        assert_eq!(sanitize_artifact_id("a/b\\c*d"), "a-b-c-d");
        assert_eq!(sanitize_artifact_id("plain-id_1.2"), "plain-id_1.2");
    }

    #[test]
    fn load_missing_artifact_is_none() {
        let (app, _t) = app();
        syncsafe::create_dir_all(app.profile.session_path("s1")).unwrap();
        assert!(analysis_load_impl(&app, "s1", "builtin:pm").unwrap().is_none());
    }

    #[test]
    fn artifact_round_trips() {
        let (app, _t) = app();
        let dir = app.profile.session_path("s1").join("analyses");
        syncsafe::create_dir_all(&dir).unwrap();
        let r = AnalysisResult {
            prompt_id: "builtin:pm".into(),
            prompt_name: "Project Manager coaching".into(),
            markdown: "# hi".into(),
            generated_at_unix_seconds: 42,
        };
        syncsafe::write(dir.join("builtin-pm.json"), serde_json::to_vec(&r).unwrap()).unwrap();
        let got = analysis_load_impl(&app, "s1", "builtin:pm").unwrap().unwrap();
        assert_eq!(got.markdown, "# hi");
        assert_eq!(got.generated_at_unix_seconds, 42);
    }
}
