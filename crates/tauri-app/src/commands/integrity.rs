// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
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

/// The chunk audio files the manifest names, under `session_dir`.
fn chunk_files<'a>(session_dir: &'a Path, m: &'a SessionManifest) -> impl Iterator<Item = std::path::PathBuf> + 'a {
    m.chunks.iter().flat_map(move |c| {
        [Some(&c.mic_wav_relative), Some(&c.system_wav_relative), c.mic_aec_wav_relative.as_ref()]
            .into_iter()
            .flatten()
            .map(move |rel| syncsafe::join_rel(session_dir, rel))
    })
}

/// Audit one finalized session dir, returning the repairs whose output is
/// missing AND whose inputs are present, in dependency order. `include_paid`
/// gates the LLM repairs (summary/chapters). Non-finalized or unreadable
/// sessions return an empty list.
pub fn audit_session(session_dir: &Path, include_paid: bool) -> Vec<RepairKind> {
    let mut needs = Vec::new();
    let Ok(bytes) = sealfile::fs::read(session_dir.join("manifest.json")) else { return needs };
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
    // meeting.opus ← chunk audio (+ safe manifest), every chunk file there
    // opening under a key held.
    if !session_dir.join(recording::mixdown::MEETING_AUDIO_NAME).is_file()
        && manifest_chunks_safe(&m)
        && m.chunks.iter().any(|c| {
            syncsafe::join_rel(session_dir, &c.mic_wav_relative).is_file()
                || syncsafe::join_rel(session_dir, &c.system_wav_relative).is_file()
        })
        && chunk_files(session_dir, &m).all(|p| !p.is_file() || sealfile::fs::open_stream(&p).is_ok())
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
            let bytes = sealfile::fs::read(dir.join("manifest.json"))?;
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
    let mut sessions = 0usize;
    let mut repairs = 0usize;
    for p in crate::commands::session_store::session_dirs(&sessions_dir).unwrap_or_default() {
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

/// Run the local, key-free repairs for one session and return how many ran.
/// Callers that drop a session's derived artifacts (truncate/split) use this
/// to rebuild them in the same call. LLM repairs are not run.
pub fn repair_local_now(app: &AppState, vs: &VaultState, session_id: &str) -> usize {
    let dir = app.profile.session_path(session_id);
    let mut repaired = 0usize;
    for kind in audit_session(&dir, /* include_paid */ false) {
        match repair_one(app, vs, session_id, kind) {
            Ok(()) => {
                log::info!("integrity: rebuilt {kind:?} for {session_id} after edit");
                repaired += 1;
            }
            Err(e) => log::warn!("integrity: {kind:?} for {session_id} after edit: {e}"),
        }
    }
    repaired
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meeting_audio_waits_while_a_chunk_opens_under_no_key_held() {
        use std::io::Write as _;
        let _x = crate::test_keys::exclusive();
        let td = tempfile::tempdir().unwrap();
        let id = "daisy-1700000000";
        let dir = td.path().join("sessions").join(id);
        std::fs::create_dir_all(dir.join("chunks/0001")).unwrap();
        let (ours, theirs) = (sealfile::ContentSecret::generate().0, sealfile::ContentSecret::generate().0);
        let keep = sealfile::ContentSecret::from_bytes(*theirs.to_bytes());
        sealfile::keyring::unlock(ours);
        let manifest = serde_json::json!({
            "schema_version": 4, "session_id": id, "created_at_unix_seconds": 1_700_000_000, "sample_rate": 16000,
            "channels": 1, "finalized_at_unix_seconds": 1_700_000_100,
            "chunks": [{
                "index": 1, "started_at_unix_seconds": 10, "ended_at_unix_seconds": 20, "duration_seconds": 10,
                "mic_wav_relative": "chunks/0001/mic.wav", "system_wav_relative": "chunks/0001/system.wav",
                "mic_aec_wav_relative": null,
            }],
        });
        sealfile::fs::write(dir.join("manifest.json"), serde_json::to_vec(&manifest).unwrap()).unwrap();
        // Their desktop sealed the chunk audio under its key.
        let key = sealfile::keyfile::derive_session_key(&theirs, id);
        let header = sealfile::stream::StreamHeader::pcm(sealfile::stream::PcmFormat { sample_rate: 16_000, channels: 1, bits_per_sample: 16 });
        for rel in ["chunks/0001/mic.wav", "chunks/0001/system.wav"] {
            let mut w = sealfile::stream::StreamWriter::new(std::fs::File::create(dir.join(rel)).unwrap(), &key, rel, header).unwrap();
            w.write_all(&[3u8; 64_000]).unwrap();
            w.finish().unwrap();
        }
        assert!(!audit_session(&dir, false).contains(&RepairKind::BuildOpus), "their key is not held");
        sealfile::keyring::add_secret(keep);
        assert!(audit_session(&dir, false).contains(&RepairKind::BuildOpus));
        sealfile::keyring::lock();
    }
}
