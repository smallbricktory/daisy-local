//! Settings → Recordings: stats, safe bulk delete.
//!
//! "Safe delete" only touches recordings that are orphaned (no readable
//! `manifest.json`) or already transcribed (`transcript.json` /
//! `transcript.dedup.json` present). It deletes the `chunks/` subtree (the
//! raw WAVs) and recreates it empty. `meeting.opus` (stereo archive, L=mic,
//! R=system), `manifest.json`, `transcript*.json`, `summary.md`, and notes
//! are never touched.

use crate::error::Result;
use crate::state::AppState;
use recording::mixdown::MEETING_AUDIO_NAME;
use serde::Serialize;
use std::path::Path;

#[derive(Debug, Default, Serialize)]
pub struct RecordingsStats {
    pub session_count: u32,
    pub wav_bytes: u64,
    pub opus_bytes: u64,
    /// Sessions eligible for the "delete all" action (orphaned or transcribed).
    pub deletable_session_count: u32,
    /// Bytes that "delete all" reclaims: WAVs only; meeting.opus is kept.
    pub deletable_bytes: u64,
}

/// A session dir is deletable iff it is an orphan (no readable
/// `manifest.json`) or it has been transcribed.
fn is_deletable(session_dir: &Path) -> bool {
    let manifest_ok = syncsafe::read(session_dir.join("manifest.json"))
        .ok()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
        .is_some();
    if !manifest_ok {
        return true; // orphan / corrupt manifest
    }
    session_dir.join("transcript.json").is_file()
        || session_dir.join("transcript.dedup.json").is_file()
}

/// `(wav_bytes, opus_bytes)` for a session: WAVs under `<session_dir>/chunks`,
/// plus the size of `<session_dir>/meeting.opus` (0 if absent).
fn chunk_audio_bytes(session_dir: &Path) -> (u64, u64) {
    let chunks = session_dir.join("chunks");
    let mut wav = 0u64;
    for entry in walkdir::WalkDir::new(&chunks)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if !entry.file_type().is_file() {
            continue;
        }
        if entry.path().extension().and_then(|e| e.to_str()) == Some("wav") {
            wav += entry.metadata().map(|m| m.len()).unwrap_or(0);
        }
    }
    let opus = std::fs::metadata(session_dir.join(MEETING_AUDIO_NAME))
        .map(|m| m.len())
        .unwrap_or(0);
    (wav, opus)
}

fn session_dirs(app: &AppState) -> Result<Vec<std::path::PathBuf>> {
    let dir = app.profile.sessions_dir();
    if !dir.exists() {
        return Ok(vec![]);
    }
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&dir)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            out.push(entry.path());
        }
    }
    Ok(out)
}

pub fn recordings_stats_impl(app: &AppState) -> Result<RecordingsStats> {
    let mut s = RecordingsStats::default();
    for p in session_dirs(app)? {
        s.session_count += 1;
        let (wav, opus) = chunk_audio_bytes(&p);
        s.wav_bytes += wav;
        s.opus_bytes += opus;
        if is_deletable(&p) {
            s.deletable_session_count += 1;
            s.deletable_bytes += wav; // clear keeps meeting.opus
        }
    }
    Ok(s)
}

#[derive(Debug, Default, Serialize)]
pub struct DeleteSummary {
    pub deleted_sessions: u32,
    pub freed_bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
struct DeleteProgress {
    done: u32,
    total: u32,
    current: String,
}

pub fn delete_recordings_impl(
    app: &AppState,
    app_handle: &tauri::AppHandle,
) -> Result<DeleteSummary> {
    use tauri::Emitter;
    let targets: Vec<std::path::PathBuf> = session_dirs(app)?
        .into_iter()
        .filter(|p| is_deletable(p))
        .collect();
    let total = targets.len() as u32;
    let mut summary = DeleteSummary::default();
    for (i, p) in targets.iter().enumerate() {
        // Remove only the raw chunk WAVs; meeting.opus is kept.
        let (wav, _opus) = chunk_audio_bytes(p);
        let chunks = p.join("chunks");
        if chunks.is_dir() {
            syncsafe::remove_dir_all(&chunks)?;
            syncsafe::create_dir_all(&chunks)?;
        }
        summary.freed_bytes += wav;
        summary.deleted_sessions += 1;
        let _ = app_handle.emit(
            "daisy://recordings/delete_progress",
            DeleteProgress {
                done: (i + 1) as u32,
                total,
                current: p
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .to_string(),
            },
        );
    }
    Ok(summary)
}

