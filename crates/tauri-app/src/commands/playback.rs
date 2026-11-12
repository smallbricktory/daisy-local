// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Playback of a session's mixed-down `meeting.opus`. On macOS and Windows
//! the webview plays it from the `daisy-media://` URL of
//! [`session_playback_url`], which [`crate::media_scheme`] serves; on Linux
//! from a decrypted file (`crate::playback_file`).
//!
//! Checks applied before offering or serving audio:
//!
//!   1. Caller must hold an unlocked vault.
//!   2. `session_id` is validated by
//!      [`crate::commands::session_store::is_safe_session_id`].
//!   3. The on-disk path is canonicalized and must start with
//!      `<profile>/sessions/`.
//!   4. Responses without a range are capped at 256 MiB.

use crate::commands::session_store::is_safe_session_id;
use crate::error::{AppError, Result};
use crate::state::{AppState, VaultState};
use recording::mixdown::MEETING_AUDIO_NAME;
use std::path::PathBuf;

pub(crate) const MAX_PLAYBACK_BYTES: u64 = 256 * 1024 * 1024;

/// Returns the canonical path to a session's `meeting.opus` under the
/// sessions root, or an error if a check fails.
pub(crate) fn resolve_meeting_audio(app: &AppState, session_id: &str) -> Result<PathBuf> {
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
pub(crate) const WEBVIEW_PLAYS_OGG_OPUS: bool = !cfg!(target_os = "macos");

/// The URL the player loads a session's audio from.
pub fn session_playback_url(session_id: &str) -> String {
    let origin = if cfg!(any(windows, target_os = "android")) {
        "http://daisy-media.localhost"
    } else {
        "daisy-media://localhost"
    };
    format!("{origin}/session/{session_id}/meeting")
}
