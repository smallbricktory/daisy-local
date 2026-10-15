// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Playback of a session's mixed-down `meeting.opus`. macOS and Windows get
//! the bytes over the Tauri IPC bridge and wrap them in a `Blob` URL on the
//! frontend; Linux plays the file from the `file://` URL of
//! [`session_playback_audio_url_impl`].
//!
//! Checks applied before returning bytes or a URL:
//!
//!   1. Caller must hold an unlocked vault.
//!   2. `session_id` is validated by
//!      [`crate::commands::session_store::is_safe_session_id`].
//!   3. The on-disk path is canonicalized and must start with
//!      `<profile>/sessions/`; that decides which path a command hands out.
//!   4. Bytes responses are capped at 256 MiB.

use crate::commands::session_store::is_safe_session_id;
use crate::error::{AppError, Result};
use crate::state::{AppState, VaultState};
use recording::mixdown::MEETING_AUDIO_NAME;
use std::path::PathBuf;

const MAX_PLAYBACK_BYTES: u64 = 256 * 1024 * 1024;

/// Returns the canonical path to a session's `meeting.opus` under the
/// sessions root, or an error if a check fails.
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

/// WKWebView (macOS) has no Ogg-Opus decoder; the opus is decoded to an
/// 8 kHz mono µ-law WAV for it. WebView2 and WebKitGTK play the opus bytes.
const WEBVIEW_PLAYS_OGG_OPUS: bool = !cfg!(target_os = "macos");

/// Return the session's playback audio for the frontend to wrap in a Blob URL.
/// The frontend sniffs the magic bytes to pick the Blob MIME type.
pub fn session_playback_audio_bytes_impl(
    app: &AppState,
    vs: &VaultState,
    session_id: &str,
) -> Result<Vec<u8>> {
    if !vs.is_unlocked() {
        return Err(AppError::VaultLocked);
    }
    let p = resolve_meeting_audio(app, session_id)?;
    let bytes = if WEBVIEW_PLAYS_OGG_OPUS {
        syncsafe::read(&p).map_err(|e| AppError::Config(format!("read {}: {e}", p.display())))?
    } else {
        recording::compress::decode_opus_to_ulaw_wav_bytes(&p)
            .map_err(|e| AppError::Config(format!("decode {} to wav: {e}", p.display())))?
    };
    if bytes.len() as u64 > MAX_PLAYBACK_BYTES {
        return Err(AppError::Config(format!(
            "playback audio too large ({} bytes > {} cap)",
            bytes.len(),
            MAX_PLAYBACK_BYTES
        )));
    }
    Ok(bytes)
}

/// Returns the `file://` URL of the session's `meeting.opus` on Linux, where
/// WebKitGTK plays and seeks it straight from disk. Returns `None` on other
/// platforms, which play the bytes from [`session_playback_audio_bytes_impl`].
pub fn session_playback_audio_url_impl(
    app: &AppState,
    vs: &VaultState,
    session_id: &str,
) -> Result<Option<String>> {
    if !cfg!(target_os = "linux") {
        return Ok(None);
    }
    playback_file_url(app, vs, session_id).map(Some)
}

/// The `file://` URL of the session's canonical, scope-confined
/// `meeting.opus`. Requires an unlocked vault.
pub fn playback_file_url(app: &AppState, vs: &VaultState, session_id: &str) -> Result<String> {
    if !vs.is_unlocked() {
        return Err(AppError::VaultLocked);
    }
    let p = resolve_meeting_audio(app, session_id)?;
    Ok(file_url(&p))
}

/// `file://` URL of an absolute Unix path. Every byte outside the RFC 3986
/// unreserved set and `/` is percent-encoded, so `@`, spaces, `%`, `#`, `?`
/// and non-ASCII names survive the round trip.
pub fn file_url(path: &std::path::Path) -> String {
    let mut out = String::from("file://");
    for &b in path.as_os_str().as_encoded_bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~' | b'/') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::file_url;
    use std::path::Path;

    #[test]
    fn file_url_encodes_every_byte_outside_the_unreserved_set() {
        assert_eq!(
            file_url(Path::new("/home/a b@c/sessions/daisy-1/meeting.opus")),
            "file:///home/a%20b%40c/sessions/daisy-1/meeting.opus"
        );
        assert_eq!(
            file_url(Path::new("/x/%#?;\u{e9}")),
            "file:///x/%25%23%3F%3B%C3%A9"
        );
    }
}
