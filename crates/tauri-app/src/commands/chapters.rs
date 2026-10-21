// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Topic-chapter extraction for a finalized session. Runs the LLM over
//! `transcript.md` and persists the resulting chapter list as `chapters.json`
//! in the session directory.

use crate::error::{AppError, Result};
use crate::state::{AppState, ProviderId, VaultState};
use serde::{Deserialize, Serialize};
use summarize::chapters::{extract_chapters, hms_to_secs, Chapter, SessionChapters};

use crate::now_unix;

#[derive(Debug, Deserialize)]
pub struct ChaptersRequest {
    pub session_id: String,
    /// Provider override; falls back to this machine's AI provider.
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
    let transcript = match syncsafe::read_to_string(&transcript_path) {
        Ok(t) => t,
        // A session that was never transcribed has nothing to chapter, the
        // same as one whose transcript is blank.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(AppError::Config(format!("read transcript.md: {e}"))),
    };
    if transcript.trim().is_empty() {
        return Ok(ChaptersResult {
            chapters: Vec::new(),
            skipped: true,
            reason: Some("transcript is empty".into()),
        });
    }

    let Some(resolved) =
        crate::commands::llm::resolve_optional(app, vs, req.provider, None, "chapters")?
    else {
        return Ok(ChaptersResult {
            chapters: Vec::new(),
            skipped: true,
            reason: Some("no AI provider configured".into()),
        });
    };
    let provider_id = resolved.provider;
    // Model name for the chapters.json provenance stamp. Daisy Cloud is
    // stamped by provider name.
    let model = if provider_id == ProviderId::DaisyGateway {
        provider_id.as_str().to_string()
    } else {
        let (_, _, m) = crate::commands::summary::resolve_summary_creds(
            provider_id,
            req.model.clone(),
            None,
            resolved.cfg.as_ref(),
        )?;
        m
    };
    let completer = resolved.chat_completer()?;

    let budget = resolved.context_budget();
    let chapters = extract_chapters(completer.as_ref(), &transcript, budget.as_ref()).map_err(|e| {
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
    syncsafe::write_atomic(&path, serde_json::to_vec_pretty(&out)?)?;
    fold_chapters(&root, &chapters)?;
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

/// Chapters ride inside the summary on the wire (protocol §7); chapters.json
/// stays for this desktop's own pane. A session with no summary yet is left
/// alone: the summarizer runs first in the pipeline and re-folds on its own.
pub(crate) fn fold_chapters(session_dir: &std::path::Path, chapters: &[Chapter]) -> Result<()> {
    let path = session_dir.join("summary.json");
    let Ok(bytes) = syncsafe::read(&path) else {
        return Ok(());
    };
    let mut summary = summarize::SessionSummary::decode(&bytes)
        .map_err(|e| AppError::Config(format!("parse summary.json: {e}")))?;
    summary.chapters = chapters
        .iter()
        .map(|c| summarize::SummaryChapter {
            title: c.title.clone(),
            start_ms: Some(hms_to_secs(&c.start_hms) as u64 * 1000),
            summary: c.summary.clone(),
        })
        .collect();
    syncsafe::write_atomic(&path, summary.encode())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chapters_fold_into_the_summary() {
        let dir = tempfile::tempdir().unwrap();
        let summary = summarize::SessionSummary::hand_written("daisy-1", "test", "# md", 1);
        std::fs::write(dir.path().join("summary.json"), summary.encode()).unwrap();
        let chapters = vec![Chapter {
            title: "Open".into(),
            start_hms: "00:00:05".into(),
            summary: None,
        }];
        fold_chapters(dir.path(), &chapters).unwrap();
        let folded = summarize::SessionSummary::decode(
            &std::fs::read(dir.path().join("summary.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(folded.chapters.len(), 1);
        assert_eq!(folded.chapters[0].start_ms, Some(5000));
        assert_eq!(folded.markdown, "# md");
    }
}
