//! List sessions in the profile dir.

use crate::error::Result;
use crate::state::AppState;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

#[derive(Debug, Clone, Serialize)]
pub struct SessionListEntry {
    pub session_id: String,
    pub created_at_unix_seconds: i64,
    pub finalized_at_unix_seconds: Option<i64>,
    /// Total duration across all chunks, or None if no chunks closed.
    pub duration_seconds: Option<u64>,
    /// Display title from the manifest.
    pub title: Option<String>,
    /// Tag IDs attached to this session.
    pub tag_ids: Vec<String>,
    pub has_transcript: bool,
    pub has_dedup: bool,
    pub has_summary: bool,
    /// Recovered from a force-quit/crash interruption (not a clean Stop).
    #[serde(default)]
    pub interrupted: bool,
}

/// Process-global cache for the session list.
///
/// Cached `entries` are returned when the `sessions_dir` path AND its mtime
/// are unchanged. Manifest content edits inside a subdir do not bump the
/// parent mtime; those are covered by [`invalidate_library_cache`], called
/// from the `library:changed` emit path.
struct CachedList {
    dir: PathBuf,
    mtime: Option<SystemTime>,
    entries: Vec<SessionListEntry>,
}

fn library_cache() -> &'static Mutex<Option<CachedList>> {
    static CACHE: OnceLock<Mutex<Option<CachedList>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(None))
}

/// Drop the cached session list. Called by the `library:changed` emit path.
pub fn invalidate_library_cache() {
    *library_cache().lock().unwrap() = None;
}

/// Lists sessions sorted by created_at_unix_seconds DESC (newest first).
pub fn list_sessions_impl(app: &AppState) -> Result<Vec<SessionListEntry>> {
    let dir = app.profile.sessions_dir();
    if !dir.exists() {
        return Ok(vec![]);
    }
    let mtime = std::fs::metadata(&dir).and_then(|m| m.modified()).ok();
    {
        let guard = library_cache().lock().unwrap();
        if let Some(c) = guard.as_ref() {
            if c.dir == dir && c.mtime == mtime {
                return Ok(c.entries.clone());
            }
        }
    }
    let out = scan_sessions(&dir)?;
    *library_cache().lock().unwrap() = Some(CachedList {
        dir,
        mtime,
        entries: out.clone(),
    });
    Ok(out)
}

/// The actual filesystem scan (uncached). Returns entries sorted newest-first.
fn scan_sessions(dir: &Path) -> Result<Vec<SessionListEntry>> {
    let mut out: Vec<SessionListEntry> = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let manifest_path = entry.path().join("manifest.json");
        if !manifest_path.is_file() {
            continue;
        }
        let bytes = match syncsafe::read(&manifest_path) {
            Ok(b) => b,
            Err(_) => continue,
        };
        #[derive(serde::Deserialize)]
        struct M {
            session_id: String,
            created_at_unix_seconds: i64,
            finalized_at_unix_seconds: Option<i64>,
            #[serde(default)]
            chunks: Vec<C>,
            #[serde(default)]
            title: Option<String>,
            #[serde(default)]
            tag_ids: Vec<String>,
            #[serde(default)]
            interrupted: bool,
        }
        #[derive(serde::Deserialize)]
        struct C {
            #[serde(default)]
            duration_seconds: Option<u64>,
        }
        let m: M = match serde_json::from_slice(&bytes) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let dur: u64 = m
            .chunks
            .iter()
            .filter_map(|c| c.duration_seconds)
            .sum();
        let dur = if dur > 0 { Some(dur) } else { None };
        out.push(SessionListEntry {
            session_id: m.session_id,
            created_at_unix_seconds: m.created_at_unix_seconds,
            finalized_at_unix_seconds: m.finalized_at_unix_seconds,
            duration_seconds: dur,
            title: m.title,
            tag_ids: m.tag_ids,
            has_transcript: entry.path().join("transcript.json").is_file(),
            has_dedup: entry.path().join("transcript.dedup.json").is_file(),
            has_summary: entry.path().join("summary.md").is_file(),
            interrupted: m.interrupted,
        });
    }
    out.sort_by(|a, b| b.created_at_unix_seconds.cmp(&a.created_at_unix_seconds));
    Ok(out)
}
