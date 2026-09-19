// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! One home for session-store primitives: walking the sessions root,
//! reading manifests, picking the transcript file, validating session ids,
//! and loading transcript text.

use std::path::{Path, PathBuf};

/// Session directories under `root`: entries that are real directories
/// (symlinks are not followed), contain a `manifest.json`, and whose name
/// doesn't start with `.`. Unordered; a missing `root` yields `Ok(empty)`,
/// any other read failure propagates.
pub fn session_dirs(root: &Path) -> std::io::Result<Vec<PathBuf>> {
    let rd = match std::fs::read_dir(root) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut out = Vec::new();
    for e in rd.flatten() {
        if !e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        if e.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        let p = e.path();
        if !p.join("manifest.json").is_file() {
            continue;
        }
        out.push(p);
    }
    Ok(out)
}

/// Best-effort read of a session dir's manifest; `None` when missing or
/// unparseable. The one home for the read+parse incantation.
pub fn read_manifest_opt(root: &Path) -> Option<::recording::manifest::SessionManifest> {
    let b = syncsafe::read(root.join("manifest.json")).ok()?;
    let mut m = serde_json::from_slice(&b).ok()?;
    ::recording::speakers::apply_to_manifest(root, &mut m).ok()?;
    Some(m)
}

/// The session's transcript JSON: `transcript.dedup.json` when present, else
/// `transcript.json`, else `None`.
pub fn preferred_transcript_path(root: &Path) -> Option<PathBuf> {
    let dedup = root.join("transcript.dedup.json");
    if dedup.is_file() {
        return Some(dedup);
    }
    let plain = root.join("transcript.json");
    if plain.is_file() {
        return Some(plain);
    }
    None
}

/// One session-id guard for every path built from a caller-supplied id:
/// non-empty, under 256 bytes, ASCII alphanumeric plus `-` `_` `.`, and no
/// leading dot (which also rejects `.` and `..`).
pub fn is_safe_session_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() < 256
        && !s.starts_with('.')
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
}

/// Which text `load_transcript_md` returns and when it returns `None`.
pub enum TranscriptSource {
    /// `transcript.md`, unfiltered; `None` only when unreadable. Callers
    /// turn `None` into their own error.
    RequireTranscript,
    /// `transcript.md` when the file exists, else `notes.md`; `None` when
    /// the chosen file is unreadable or blank.
    TranscriptElseNotes,
    /// `transcript.md`; `None` when unreadable or blank.
    Optional,
}

/// Load a session's transcript text per `policy` (see [`TranscriptSource`]).
pub fn load_transcript_md(root: &Path, policy: TranscriptSource) -> Option<String> {
    let transcript = root.join("transcript.md");
    match policy {
        TranscriptSource::RequireTranscript => syncsafe::read_to_string(transcript).ok(),
        TranscriptSource::Optional => syncsafe::read_to_string(transcript)
            .ok()
            .filter(|s| !s.trim().is_empty()),
        TranscriptSource::TranscriptElseNotes => {
            let md = if transcript.is_file() {
                syncsafe::read_to_string(&transcript).unwrap_or_default()
            } else {
                syncsafe::read_to_string(root.join("notes.md")).unwrap_or_default()
            };
            if md.trim().is_empty() {
                None
            } else {
                Some(md)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_session_ids() {
        assert!(is_safe_session_id("daisy-1784000000"));
        assert!(is_safe_session_id("2026-06-09T10-00-00-abcd"));
        assert!(is_safe_session_id("daisy-import-1.2"));
        assert!(!is_safe_session_id(""));
        assert!(!is_safe_session_id("."));
        assert!(!is_safe_session_id(".."));
        assert!(!is_safe_session_id(".hidden"));
        assert!(!is_safe_session_id("../other"));
        assert!(!is_safe_session_id("a/b"));
        assert!(!is_safe_session_id("a\\b"));
        assert!(!is_safe_session_id("a b"));
        assert!(!is_safe_session_id("a\0b"));
        assert!(!is_safe_session_id("séance"));
    }

    #[test]
    fn session_dirs_filters_non_sessions() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        // Valid session dir.
        std::fs::create_dir(root.join("daisy-1")).unwrap();
        std::fs::write(root.join("daisy-1/manifest.json"), b"{}").unwrap();
        // No manifest.
        std::fs::create_dir(root.join("daisy-2")).unwrap();
        // Dot-dir with manifest.
        std::fs::create_dir(root.join(".trash")).unwrap();
        std::fs::write(root.join(".trash/manifest.json"), b"{}").unwrap();
        // Stray file.
        std::fs::write(root.join("stray.txt"), b"x").unwrap();

        let dirs = session_dirs(root).unwrap();
        assert_eq!(dirs, vec![root.join("daisy-1")]);
        assert!(session_dirs(&root.join("missing")).unwrap().is_empty());
        // A root that exists but isn't a directory is a real error.
        assert!(session_dirs(&root.join("stray.txt")).is_err());
    }

    #[test]
    fn preferred_transcript_prefers_dedup() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        assert_eq!(preferred_transcript_path(root), None);
        std::fs::write(root.join("transcript.json"), b"{}").unwrap();
        assert_eq!(
            preferred_transcript_path(root),
            Some(root.join("transcript.json"))
        );
        std::fs::write(root.join("transcript.dedup.json"), b"{}").unwrap();
        assert_eq!(
            preferred_transcript_path(root),
            Some(root.join("transcript.dedup.json"))
        );
    }

    #[test]
    fn load_transcript_md_policies() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(root.join("notes.md"), b"the notes").unwrap();

        // No transcript.md: only TranscriptElseNotes falls through.
        assert_eq!(
            load_transcript_md(root, TranscriptSource::RequireTranscript),
            None
        );
        assert_eq!(load_transcript_md(root, TranscriptSource::Optional), None);
        assert_eq!(
            load_transcript_md(root, TranscriptSource::TranscriptElseNotes),
            Some("the notes".into())
        );

        // Blank transcript.md: RequireTranscript passes it through; the
        // others treat it as absent (no notes fallback — the file exists).
        std::fs::write(root.join("transcript.md"), b"  \n").unwrap();
        assert_eq!(
            load_transcript_md(root, TranscriptSource::RequireTranscript),
            Some("  \n".into())
        );
        assert_eq!(load_transcript_md(root, TranscriptSource::Optional), None);
        assert_eq!(
            load_transcript_md(root, TranscriptSource::TranscriptElseNotes),
            None
        );

        std::fs::write(root.join("transcript.md"), b"words").unwrap();
        assert_eq!(
            load_transcript_md(root, TranscriptSource::Optional),
            Some("words".into())
        );
        assert_eq!(
            load_transcript_md(root, TranscriptSource::TranscriptElseNotes),
            Some("words".into())
        );
    }
}
