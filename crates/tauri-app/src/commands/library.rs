// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
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
    /// How the audio reached this profile: `phone_room` (synced from a
    /// phone), `imported_file` (an audio file), absent for a desktop
    /// recording.
    pub recording_kind: Option<String>,
    /// The session's manifest does not open with the content keys open: its
    /// key is wrapped to a content key that is not open, or the manifest is
    /// sealed under a key no open one gives. Every other field but
    /// `session_id`, `created_at_unix_seconds` and `waiting_for_key` is
    /// empty.
    #[serde(default)]
    pub locked: bool,
    /// A locked session whose content key may still open without the
    /// recovery words: its key files or its vault file have not synced in,
    /// or its vault file has not been tried yet
    /// ([`crate::commands::lifecycle::waiting_for_key`]). A manifest no
    /// open key opens while every key of the profile is open waits too.
    #[serde(default)]
    pub waiting_for_key: bool,
    /// Calendar link from the manifest. Backend-only (tag suggestions);
    /// skipped on the wire so the frontend payload is unchanged.
    #[serde(skip_serializing)]
    pub calendar: Option<recording::manifest::CalendarLink>,
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
    crate::commands::session_store::require_unlocked()?;
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
    let waiting = |public: Option<&sealfile::ContentPublic>| crate::commands::lifecycle::waiting_for_key(app, public);
    let out = scan_sessions(&dir, &waiting)?;
    *library_cache().lock().unwrap() = Some(CachedList {
        dir,
        mtime,
        entries: out.clone(),
    });
    Ok(out)
}

/// The actual filesystem scan (uncached). Returns entries sorted
/// newest-first. `waiting` answers [`SessionListEntry::waiting_for_key`]
/// for the content key a session's `key.json` is wrapped to, `None` when
/// that is not known.
fn scan_sessions(dir: &Path, waiting: &dyn Fn(Option<&sealfile::ContentPublic>) -> bool) -> Result<Vec<SessionListEntry>> {
    let mut out: Vec<SessionListEntry> = Vec::new();
    for path in crate::commands::session_store::session_dirs(dir)? {
        let bytes = match sealfile::fs::read(path.join("manifest.json")) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                let wrapped_to = syncsafe::read(path.join(sealfile::keyring::KEY_FILE))
                    .ok()
                    .and_then(|b| sealfile::keyfile::wrapped_to(&b).ok());
                out.push(locked_entry(&path, waiting(wrapped_to.as_ref())));
                continue;
            }
            Err(e) if e.get_ref().and_then(|i| i.downcast_ref::<sealfile::SealError>()).is_some_and(|s| matches!(s, sealfile::SealError::Auth)) => {
                out.push(locked_entry(&path, waiting(None) || sealfile::keyring::holds_every_profile_key()));
                continue;
            }
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
            #[serde(default)]
            recording_kind: Option<String>,
            /// Read as raw JSON: a malformed `calendar` object degrades to `None`.
            #[serde(default)]
            calendar: serde_json::Value,
        }
        #[derive(serde::Deserialize)]
        struct C {
            #[serde(default, deserialize_with = "recording::manifest::whole_seconds")]
            duration_seconds: Option<u64>,
            /// One chunk per index counts, the first, as a full manifest read
            /// keeps.
            #[serde(default)]
            index: Option<u32>,
        }
        let m: M = match serde_json::from_slice(&bytes) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let mut seen = std::collections::HashSet::new();
        let dur: u64 = m
            .chunks
            .iter()
            .filter(|c| c.index.is_none_or(|i| seen.insert(i)))
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
            has_transcript: path.join("transcript.json").is_file(),
            has_dedup: path.join("transcript.dedup.json").is_file(),
            has_summary: path.join("summary.md").is_file(),
            interrupted: m.interrupted,
            recording_kind: m.recording_kind,
            locked: false,
            waiting_for_key: false,
            calendar: serde_json::from_value(m.calendar).ok(),
        });
    }
    out.sort_by(|a, b| b.created_at_unix_seconds.cmp(&a.created_at_unix_seconds));
    Ok(out)
}

/// The row of a session whose manifest does not open with the keys held:
/// its folder name, and the creation time its id carries (`daisy-<unix
/// seconds>`), else its `key.json`'s modification time.
fn locked_entry(dir: &Path, waiting_for_key: bool) -> SessionListEntry {
    let session_id = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let created = session_id
        .rsplit('-')
        .next()
        .and_then(|t| t.parse::<i64>().ok())
        .or_else(|| {
            let modified = std::fs::metadata(dir.join(sealfile::keyring::KEY_FILE)).and_then(|m| m.modified()).ok()?;
            modified.duration_since(std::time::UNIX_EPOCH).ok().map(|d| d.as_secs() as i64)
        })
        .unwrap_or(0);
    SessionListEntry {
        session_id,
        created_at_unix_seconds: created,
        finalized_at_unix_seconds: None,
        duration_seconds: None,
        title: None,
        tag_ids: Vec::new(),
        has_transcript: false,
        has_dedup: false,
        has_summary: false,
        interrupted: false,
        recording_kind: None,
        locked: true,
        waiting_for_key,
        calendar: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::AppError;

    #[test]
    fn locked_library_returns_vault_locked() {
        let _x = crate::test_keys::exclusive();
        let td = tempfile::tempdir().unwrap();
        let app = AppState::new(crate::profile::ProfileDir::at(td.path().join("daisy")).unwrap());
        sealfile::keyring::unlock(sealfile::ContentSecret::generate().0);
        let mut pins = Vec::new();
        for id in ["daisy-1", "daisy-2"] {
            let dir = app.profile.session_path(id);
            std::fs::create_dir_all(&dir).unwrap();
            if id == "daisy-2" {
                pins.push(sealfile::keyring::pin(&dir));
            }
            sealfile::fs::write(dir.join("manifest.json"), br#"{"session_id":"x","created_at_unix_seconds":1}"#).unwrap();
            sealfile::fs::write(dir.join("notes.md"), b"agenda").unwrap();
        }
        assert_eq!(list_sessions_impl(&app).unwrap().len(), 2);

        sealfile::keyring::lock();
        let locked = |r: Result<()>| matches!(r, Err(AppError::VaultLocked));
        assert!(locked(list_sessions_impl(&app).map(drop)));
        let search = crate::commands::search::SearchRequest {
            query: Some("agenda".into()),
            tag_ids: None,
            contact_ids: None,
            date_from: None,
            date_to: None,
        };
        assert!(locked(crate::commands::search::search_sessions_impl(&app, search).map(drop)));
        assert!(locked(crate::commands::session::read_session_impl(&app, "daisy-1").map(drop)));
        assert!(locked(crate::commands::meeting::session_notes_load_impl(&app, "daisy-1").map(drop)));
        assert!(locked(crate::commands::summary::summary_load_impl(&app, "daisy-1").map(drop)));

        // A session whose key this process holds stays open.
        assert_eq!(crate::commands::meeting::session_notes_load_impl(&app, "daisy-2").unwrap(), "agenda");
        drop(pins);
    }

    #[test]
    fn a_session_under_a_content_key_not_open_lists_as_locked() {
        let _x = crate::test_keys::exclusive();
        let td = tempfile::tempdir().unwrap();
        let app = AppState::new(crate::profile::ProfileDir::at(td.path().join("daisy")).unwrap());
        let (old, current) = (sealfile::ContentSecret::generate().0, sealfile::ContentSecret::generate().0);
        let dir = app.profile.session_path("daisy-1700000000");
        std::fs::create_dir_all(&dir).unwrap();
        let key = sealfile::keyfile::derive_session_key(&old, "daisy-1700000000");
        std::fs::write(dir.join("key.json"), sealfile::keyfile::wrap_session_key(&old.public(), &key)).unwrap();
        let manifest = br#"{"session_id":"daisy-1700000000","created_at_unix_seconds":1700000000,"title":"Budget"}"#;
        std::fs::write(dir.join("manifest.json"), sealfile::stream::seal_whole(&key, "manifest.json", manifest)).unwrap();

        sealfile::keyring::unlock(current);
        invalidate_library_cache();
        let rows = list_sessions_impl(&app).unwrap();
        assert_eq!(rows.len(), 1);
        assert!(rows[0].locked);
        assert_eq!((rows[0].session_id.as_str(), rows[0].created_at_unix_seconds), ("daisy-1700000000", 1_700_000_000));
        assert_eq!(rows[0].title, None);
        assert!(rows[0].waiting_for_key, "no file of its key has synced in");

        sealfile::keyring::add_secret(old);
        invalidate_library_cache();
        let rows = list_sessions_impl(&app).unwrap();
        assert!(!rows[0].locked);
        assert_eq!(rows[0].title.as_deref(), Some("Budget"));
        sealfile::keyring::lock();
    }

    #[test]
    fn a_locked_row_waits_until_its_keys_vault_file_is_there_and_tried() {
        use crate::commands::lifecycle::{encryption_status_impl, init_vault_impl, open_content_key};
        let _x = crate::test_keys::exclusive();
        let td = tempfile::tempdir().unwrap();
        let app = AppState::new(crate::profile::ProfileDir::at(td.path().join("daisy")).unwrap());
        let vs = crate::state::VaultState::new();
        sealfile::keyring::lock();
        init_vault_impl(&app, &vs, "correct-horse-battery-staple-extended-22").unwrap();
        open_content_key(&app, &vs).unwrap();
        let ours = sealfile::keyring::content_public().unwrap();
        let theirs = sealfile::ContentSecret::generate().0;
        let id = "daisy-1700000000";
        let kt = sealfile::keyfile::derive_session_key(&theirs, id);
        // Theirs: wrapped to their key. Merged: our key.json, their manifest.
        let their_dir = app.profile.session_path(id);
        std::fs::create_dir_all(&their_dir).unwrap();
        std::fs::write(their_dir.join("key.json"), sealfile::keyfile::wrap_session_key(&theirs.public(), &kt)).unwrap();
        std::fs::write(their_dir.join("manifest.json"), sealfile::stream::seal_whole(&kt, "manifest.json", b"{}")).unwrap();
        let merged_id = "daisy-1700000300";
        let merged_dir = app.profile.session_path(merged_id);
        std::fs::create_dir_all(&merged_dir).unwrap();
        sealfile::keyring::create_session_key(&merged_dir).unwrap();
        let km = sealfile::keyfile::derive_session_key(&theirs, merged_id);
        std::fs::write(merged_dir.join("manifest.json"), sealfile::stream::seal_whole(&km, "manifest.json", b"{}")).unwrap();
        assert_eq!(
            sealfile::keyfile::wrapped_to(&std::fs::read(merged_dir.join("key.json")).unwrap()).unwrap(),
            ours,
            "the merged session's key.json is ours"
        );
        let rows = |what: &str| {
            invalidate_library_cache();
            let rows = list_sessions_impl(&app).unwrap();
            let row = |id: &str| rows.iter().find(|r| r.session_id == id).cloned().unwrap_or_else(|| panic!("{what}: {id} is listed"));
            (row(id), row(merged_id))
        };

        // Their public key file has synced in, their vault file not yet.
        let files = sealfile::keyset::files_of(app.profile.root(), &theirs.public());
        std::fs::write(files.pub_path(), format!("{}\n", base64::Engine::encode(&base64::engine::general_purpose::STANDARD, theirs.public().0))).unwrap();
        let (t, m) = rows("pub only");
        assert!(t.locked && t.waiting_for_key);
        assert!(m.locked && m.waiting_for_key, "a manifest under a key not open lists");

        // Their vault file arrives sealed under another passphrase and is tried.
        let env = vault::encrypt(&*theirs.to_bytes(), "lantern-orchard-copper-violet-meadow-77").unwrap();
        std::fs::write(files.vault_path(), serde_json::to_vec(&env).unwrap()).unwrap();
        let (t, _) = rows("vault not tried");
        assert!(t.waiting_for_key, "not tried yet");
        assert_eq!(encryption_status_impl(&app, &vs).content_key, "ok");
        let (t, m) = rows("tried");
        assert!(t.locked && !t.waiting_for_key, "only the recovery words open it now");
        assert!(m.locked && !m.waiting_for_key);
        sealfile::keyring::lock();
    }
}
