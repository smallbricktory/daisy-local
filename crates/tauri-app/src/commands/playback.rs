//! Playback of a session's mixed-down `meeting.opus`, served as bytes over
//! the Tauri IPC bridge and wrapped in a `Blob` URL on the frontend.
//!
//! Checks applied before returning bytes:
//!
//!   1. Caller must hold an unlocked vault.
//!   2. `session_id` is validated against a strict character set
//!      (alphanumeric + `-_.`) and rejected if it's `.` / `..`.
//!   3. The on-disk path is canonicalized and must start with
//!      `<profile>/sessions/`.
//!   4. Response size is capped at 256 MiB.

use crate::error::{AppError, Result};
use crate::state::{AppState, VaultState};
use recording::mixdown::MEETING_AUDIO_NAME;
use std::path::PathBuf;

const MAX_PLAYBACK_BYTES: u64 = 256 * 1024 * 1024;

fn is_safe_session_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() < 256
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
        && s != "."
        && s != ".."
}

/// Returns the canonical, scope-confined path to a session's `meeting.opus`,
/// or an error if any of the security checks fail.
fn resolve_meeting_audio(app: &AppState, session_id: &str) -> Result<PathBuf> {
    if !is_safe_session_id(session_id) {
        return Err(AppError::Config(format!("invalid session id: {session_id}")));
    }
    let sessions_root = app.profile.sessions_dir();
    let target = app.profile.session_path(session_id).join(MEETING_AUDIO_NAME);
    if !target.is_file() {
        return Err(AppError::Config("meeting.opus not found".into()));
    }
    let canon_target = target
        .canonicalize()
        .map_err(|e| AppError::Config(format!("canonicalize playback path: {e}")))?;
    let canon_root = sessions_root
        .canonicalize()
        .map_err(|e| AppError::Config(format!("canonicalize sessions root: {e}")))?;
    if !canon_target.starts_with(&canon_root) {
        return Err(AppError::Config(
            "playback path escapes the sessions directory (symlink?)".into(),
        ));
    }
    Ok(canon_target)
}

/// True iff the session has a `meeting.opus` on disk. Requires an unlocked
/// vault and a validated session id.
pub fn session_has_playback_audio_impl(
    app: &AppState,
    vs: &VaultState,
    session_id: &str,
) -> bool {
    if !vs.is_unlocked() {
        return false;
    }
    resolve_meeting_audio(app, session_id).is_ok()
}

/// Return the session's playback audio for the frontend to wrap in a Blob URL.
///
/// On macOS the opus is decoded to an 8 kHz mono µ-law WAV; on Linux and
/// Windows the raw Ogg-Opus bytes are served. The frontend sniffs the magic
/// bytes to pick the Blob MIME type.
pub fn session_playback_audio_bytes_impl(
    app: &AppState,
    vs: &VaultState,
    session_id: &str,
) -> Result<Vec<u8>> {
    if !vs.is_unlocked() {
        return Err(AppError::VaultLocked);
    }
    let p = resolve_meeting_audio(app, session_id)?;

    #[cfg(target_os = "macos")]
    {
        let wav = recording::compress::decode_opus_to_ulaw_wav_bytes(&p)
            .map_err(|e| AppError::Config(format!("decode {} to wav: {e}", p.display())))?;
        if wav.len() as u64 > MAX_PLAYBACK_BYTES {
            return Err(AppError::Config(format!(
                "playback audio too large ({} bytes > {} cap)",
                wav.len(),
                MAX_PLAYBACK_BYTES
            )));
        }
        Ok(wav)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let meta = std::fs::metadata(&p)
            .map_err(|e| AppError::Config(format!("stat {}: {e}", p.display())))?;
        if meta.len() > MAX_PLAYBACK_BYTES {
            return Err(AppError::Config(format!(
                "playback file too large ({} bytes > {} cap)",
                meta.len(),
                MAX_PLAYBACK_BYTES
            )));
        }
        syncsafe::read(&p).map_err(|e| AppError::Config(format!("read {}: {e}", p.display())))
    }
}
