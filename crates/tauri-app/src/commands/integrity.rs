//! Session integrity audit + auto-repair. Checks each finalized session for
//! missing derived artifacts and rebuilds what it can from what is on disk.
//! Unfinalized sessions are handled separately by `recover_orphan_sessions`.
//!
//! The startup sweep runs only the local, key-free repairs (dedup,
//! transcript.md re-render, meeting.opus mixdown). The LLM repairs (summary,
//! chapters) run only from an explicit `repair_session` call.

use crate::error::Result;
use crate::state::{AppState, VaultState};
use recording::manifest::SessionManifest;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepairKind {
    Dedup,
    RerenderMd,
    BuildOpus,
    Summary,
    Chapters,
}

/// False for manifests whose chunk paths are absolute or contain `..`.
fn manifest_chunks_safe(m: &SessionManifest) -> bool {
    let safe = |p: &Path| {
        !p.is_absolute() && !p.components().any(|s| matches!(s, std::path::Component::ParentDir))
    };
    m.chunks.iter().all(|c| {
        safe(c.mic_wav_relative.as_ref())
            && safe(c.system_wav_relative.as_ref())
            && c.mic_aec_wav_relative.as_deref().map(safe).unwrap_or(true)
    })
}

/// Audit one finalized session dir, returning the repairs whose output is
/// missing AND whose inputs are present, in dependency order. `include_paid`
/// gates the LLM repairs (summary/chapters). Non-finalized or unreadable
/// sessions return an empty list.
pub fn audit_session(session_dir: &Path, include_paid: bool) -> Vec<RepairKind> {
    let mut needs = Vec::new();
    let Ok(bytes) = syncsafe::read(session_dir.join("manifest.json")) else { return needs };
    let Ok(m) = serde_json::from_slice::<SessionManifest>(&bytes) else { return needs };
    if m.finalized_at_unix_seconds.is_none() {
        return needs;
    }

    let has = |f: &str| session_dir.join(f).is_file();

    // dedup ← transcript.json
    if has("transcript.json") && !has("transcript.dedup.json") {
        needs.push(RepairKind::Dedup);
    }
    // transcript.md ← dedup (or raw transcript). A dedup queued above runs
    // before the re-render.
    if (has("transcript.dedup.json") || has("transcript.json") || needs.contains(&RepairKind::Dedup))
        && !has("transcript.md")
    {
        needs.push(RepairKind::RerenderMd);
    }
    // meeting.opus ← chunk audio (+ safe manifest)
    if !session_dir.join(recording::mixdown::MEETING_AUDIO_NAME).is_file()
        && manifest_chunks_safe(&m)
        && m.chunks.iter().any(|c| {
            session_dir.join(&c.mic_wav_relative).is_file()
                || session_dir.join(&c.system_wav_relative).is_file()
        })
    {
        needs.push(RepairKind::BuildOpus);
    }
    // summary / chapters ← transcript.md + a provider; gated by include_paid.
    if include_paid && (has("transcript.md") || needs.contains(&RepairKind::RerenderMd)) {
        if !has("summary.json") {
            needs.push(RepairKind::Summary);
        }
        if !has("chapters.json") {
            needs.push(RepairKind::Chapters);
        }
    }
    needs
}

/// Run a single repair for a session. Caller emits `library:changed`.
pub fn repair_one(
    app: &AppState,
    vs: &VaultState,
    session_id: &str,
    kind: RepairKind,
) -> Result<()> {
    use crate::commands::{chapters, pipeline, session, summary};
    match kind {
        RepairKind::Dedup => {
            pipeline::dedup_impl(
                app,
                pipeline::DedupRequest {
                    session_id: session_id.to_string(),
                },
            )?;
            Ok(())
        }
        RepairKind::RerenderMd => {
            session::rerender_session_transcript_impl(app, session_id)
        }
        RepairKind::BuildOpus => {
            let dir = app.profile.session_path(session_id);
            let bytes = syncsafe::read(dir.join("manifest.json"))?;
            let m: SessionManifest = serde_json::from_slice(&bytes)?;
            if !manifest_chunks_safe(&m) {
                return Err(crate::error::AppError::Config(
                    "manifest chunk path is absolute or contains '..'".into(),
                ));
            }
            recording::mixdown::build_meeting_audio(
                &dir,
                &m,
                &recording::compress::CompressParams::default(),
            )
            .map_err(|e| crate::error::AppError::Config(format!("build meeting.opus: {e}")))?;
            Ok(())
        }
        RepairKind::Summary => {
            summary::summary_generate_impl(
                app,
                vs,
                summary::SummaryGenerateRequest {
                    session_id: session_id.to_string(),
                    provider: None,
                    model: None,
                    force: None,
                    prompt_id: None,
                },
            )?;
            Ok(())
        }
        RepairKind::Chapters => {
            chapters::extract_chapters_impl(
                app,
                vs,
                chapters::ChaptersRequest {
                    session_id: session_id.to_string(),
                    provider: None,
                    model: None,
                },
            )?;
            Ok(())
        }
    }
}

/// Walk every session and run the local repairs (dedup / transcript.md /
/// meeting.opus). LLM repairs are not run here. Emits `library:changed
/// updated` once per repaired session. Runs serially, one repair at a time.
/// Returns (repaired_sessions, total_repairs).
pub fn audit_and_repair_local<R: tauri::Runtime, E: tauri::Emitter<R>>(
    app: &AppState,
    vs: &VaultState,
    emitter: &E,
) -> (usize, usize) {
    let sessions_dir = app.profile.root().join("sessions");
    let Ok(entries) = std::fs::read_dir(&sessions_dir) else { return (0, 0) };
    let mut sessions = 0usize;
    let mut repairs = 0usize;
    for ent in entries.flatten() {
        let p = ent.path();
        if !p.is_dir() {
            continue;
        }
        let needs = audit_session(&p, /* include_paid */ false);
        if needs.is_empty() {
            continue;
        }
        let sid = p.file_name().and_then(|s| s.to_str()).unwrap_or("").to_string();
        if sid.is_empty() {
            continue;
        }
        let mut any = false;
        for kind in needs {
            match repair_one(app, vs, &sid, kind) {
                Ok(()) => {
                    log::info!("integrity: repaired {kind:?} for {sid}");
                    repairs += 1;
                    any = true;
                }
                Err(e) => log::warn!("integrity: {kind:?} for {sid}: {e}"),
            }
        }
        if any {
            sessions += 1;
            crate::library_events::emit(
                emitter,
                crate::library_events::LibraryChangeKind::Updated,
                &sid,
            );
        }
    }
    (sessions, repairs)
}
