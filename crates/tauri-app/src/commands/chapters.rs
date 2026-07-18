//! Topic-chapter extraction for a finalized session. Runs the LLM over
//! `transcript.md` and persists the resulting chapter list as `chapters.json`
//! in the session directory.

use crate::error::{AppError, Result};
use crate::state::{AppState, ProviderConfig, ProviderId, VaultState};
use serde::{Deserialize, Serialize};
use summarize::chapters::{extract_chapters, Chapter, SessionChapters};

use crate::now_unix;

#[derive(Debug, Deserialize)]
pub struct ChaptersRequest {
    pub session_id: String,
    /// Provider override; falls back to `settings.default_summary_provider`.
    pub provider: Option<ProviderId>,
    pub model: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ChaptersResult {
    pub chapters: Vec<Chapter>,
    pub skipped: bool,
    pub reason: Option<String>,
}

/// Re-render or generate chapters for the given session. Loads `transcript.md`,
/// runs the chapter extractor, writes `chapters.json`, overwriting any
/// existing file.
pub fn extract_chapters_impl(
    app: &AppState,
    vs: &VaultState,
    req: ChaptersRequest,
) -> Result<ChaptersResult> {
    let root = app.profile.session_path(&req.session_id);
    if !root.is_dir() {
        return Err(AppError::SessionNotFound(req.session_id));
    }
    let transcript_path = root.join("transcript.md");
    let transcript = syncsafe::read_to_string(&transcript_path)
        .map_err(|e| AppError::Config(format!("read transcript.md: {e}")))?;
    if transcript.trim().is_empty() {
        return Ok(ChaptersResult {
            chapters: Vec::new(),
            skipped: true,
            reason: Some("transcript is empty".into()),
        });
    }

    let settings = crate::settings::Settings::load_or_default(&app.profile.settings_path());
    let provider_id = match req.provider.or(settings.default_summary_provider) {
        Some(p) => p,
        None => {
            // No AI provider configured: skip.
            return Ok(ChaptersResult {
                chapters: Vec::new(),
                skipped: true,
                reason: Some("no AI provider configured".into()),
            });
        }
    };

    let (provider_cfg, gateway): (
        Option<ProviderConfig>,
        Option<summarize::gateway::GatewayCreds>,
    ) = {
        let g = vs.keys.lock().unwrap();
        match g.as_ref() {
            Some(keys) => {
                let cfg = keys.providers.get(&provider_id).cloned();
                let gw = if provider_id == crate::state::ProviderId::DaisyGateway {
                    Some(crate::commands::summary::gateway_creds_from_keys(keys, "chapters")?)
                } else {
                    None
                };
                (cfg, gw)
            }
            None => (None, None),
        }
    };
    // Model name for the chapters.json provenance stamp. Daisy Cloud is
    // stamped by provider name.
    let model = if provider_id == crate::state::ProviderId::DaisyGateway {
        provider_id.as_str().to_string()
    } else {
        let (_, _, m) = crate::commands::summary::resolve_summary_creds(
            provider_id,
            req.model.clone(),
            None,
            provider_cfg.as_ref(),
        )?;
        m
    };
    let completer = crate::commands::summary::build_chat_completer_for(
        provider_id,
        provider_cfg.as_ref(),
        gateway,
    )?;

    let chapters = extract_chapters(completer.as_ref(), &transcript).map_err(|e| {
        crate::commands::summary::map_gateway_err(provider_id, "chapter extraction", e)
    })?;

    let out = SessionChapters {
        schema_version: SessionChapters::SCHEMA,
        session_id: req.session_id.clone(),
        model: model.clone(),
        generated_at_unix_seconds: now_unix(),
        chapters: chapters.clone(),
    };
    let path = root.join("chapters.json");
    let tmp = path.with_extension("json.tmp");
    syncsafe::write(&tmp, serde_json::to_vec_pretty(&out)?)?;
    syncsafe::rename(&tmp, &path)?;
    Ok(ChaptersResult {
        chapters,
        skipped: false,
        reason: None,
    })
}

/// Read `chapters.json` if present. Returns `None` if the file does not exist.
pub fn load_session_chapters_impl(
    app: &AppState,
    session_id: &str,
) -> Result<Option<SessionChapters>> {
    let path = app.profile.session_path(session_id).join("chapters.json");
    match syncsafe::read(&path) {
        Ok(b) => Ok(Some(serde_json::from_slice(&b).map_err(|e| {
            AppError::Config(format!("parse chapters.json: {e}"))
        })?)),
        Err(_) => Ok(None),
    }
}
