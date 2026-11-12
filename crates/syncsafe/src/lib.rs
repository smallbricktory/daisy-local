// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Filesystem operations that tolerate the transient failures sync tools
//! (Syncthing, OneDrive, Dropbox) cause on a live profile directory:
//! short-lived locks and sharing violations surface as `PermissionDenied`
//! (os 5) or os 32/33 on Windows. Every operation retries those with a
//! bounded backoff (~4 s total) before giving up; all other errors return
//! immediately.

use std::fs;
use std::io;
use std::path::Path;
use std::time::Duration;

const BACKOFF_MS: &[u64] = &[20, 50, 100, 250, 500, 1000, 2000];

fn is_transient(e: &io::Error) -> bool {
    if e.kind() == io::ErrorKind::PermissionDenied || e.kind() == io::ErrorKind::Interrupted {
        return true;
    }
    // 32 = ERROR_SHARING_VIOLATION, 33 = ERROR_LOCK_VIOLATION.
    matches!(e.raw_os_error(), Some(5) | Some(32) | Some(33))
}

/// Run `op`, retrying transient sharing/lock failures with backoff.
pub fn retry<T>(mut op: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    let mut last: Option<io::Error> = None;
    for (i, delay) in std::iter::once(&0u64).chain(BACKOFF_MS).enumerate() {
        if *delay > 0 {
            std::thread::sleep(Duration::from_millis(*delay));
        }
        match op() {
            Ok(v) => {
                if i > 0 {
                    log::debug!("syncsafe: succeeded after {i} retr{}", if i == 1 { "y" } else { "ies" });
                }
                return Ok(v);
            }
            Err(e) if is_transient(&e) => last = Some(e),
            Err(e) => return Err(e),
        }
    }
    Err(last.unwrap_or_else(|| io::Error::other("syncsafe: retries exhausted")))
}

pub fn read(path: impl AsRef<Path>) -> io::Result<Vec<u8>> {
    let p = path.as_ref();
    retry(|| fs::read(p))
}

pub fn read_to_string(path: impl AsRef<Path>) -> io::Result<String> {
    let p = path.as_ref();
    retry(|| fs::read_to_string(p))
}

pub fn write(path: impl AsRef<Path>, contents: impl AsRef<[u8]>) -> io::Result<()> {
    let (p, c) = (path.as_ref(), contents.as_ref());
    guard_sealed(p, c);
    retry(|| fs::write(p, c))
}

/// Write a temp file in the same directory, then rename over `path`.
/// Readers never observe a partial file; both the write and the rename are
/// retried.
pub fn write_atomic(path: impl AsRef<Path>, contents: impl AsRef<[u8]>) -> io::Result<()> {
    let p = path.as_ref();
    let tmp = tmp_path(p);
    let c = contents.as_ref();
    guard_sealed(p, c);
    if let Err(e) = retry(|| fs::write(&tmp, c)) {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    let renamed = retry(|| fs::rename(&tmp, p));
    if renamed.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    renamed
}

/// Like [`write_atomic`], but the temp file is fsynced before the rename so
/// the contents survive power loss.
pub fn write_atomic_synced(path: impl AsRef<Path>, contents: impl AsRef<[u8]>) -> io::Result<()> {
    use std::io::Write as _;
    let p = path.as_ref();
    let tmp = tmp_path(p);
    let c = contents.as_ref();
    guard_sealed(p, c);
    if let Err(e) = retry(|| {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(c)?;
        f.sync_all()
    }) {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    let renamed = retry(|| fs::rename(&tmp, p));
    if renamed.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    renamed
}

/// A temp name no other writer of this path will pick.
fn tmp_path(p: &Path) -> std::path::PathBuf {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut name = p.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".tmp.{}.{seq}", std::process::id()));
    p.with_file_name(name)
}

pub fn rename(from: impl AsRef<Path>, to: impl AsRef<Path>) -> io::Result<()> {
    let (f, t) = (from.as_ref(), to.as_ref());
    retry(|| fs::rename(f, t))
}

pub fn remove_file(path: impl AsRef<Path>) -> io::Result<()> {
    let p = path.as_ref();
    retry(|| fs::remove_file(p))
}

pub fn create_dir_all(path: impl AsRef<Path>) -> io::Result<()> {
    let p = path.as_ref();
    retry(|| fs::create_dir_all(p))
}

/// Creates the file at `path`, replacing one that is there. Debug builds
/// assert that `path` is not a session file other than its `key.json`: a
/// writer that seals what it writes creates the file with
/// [`create_sealed`].
pub fn create(path: impl AsRef<Path>) -> io::Result<fs::File> {
    let p = path.as_ref();
    if cfg!(debug_assertions) {
        if let Some((_, rel)) = session_file(p) {
            debug_assert!(guard_exempt(&rel), "plaintext file created in a session: {}", p.display());
        }
    }
    retry(|| fs::File::create(p))
}

/// [`create`] for a writer that writes only sealed bytes (`DSS1` or
/// `DSR1`) to the file, a session file included. Debug builds assert that
/// a session file is not named like a sync tool's copy of `key.json`.
pub fn create_sealed(path: impl AsRef<Path>) -> io::Result<fs::File> {
    let p = path.as_ref();
    guard_name(p);
    retry(|| fs::File::create(p))
}

pub fn open(path: impl AsRef<Path>) -> io::Result<fs::File> {
    let p = path.as_ref();
    retry(|| fs::File::open(p))
}

pub fn remove_dir_all(path: impl AsRef<Path>) -> io::Result<()> {
    let p = path.as_ref();
    retry(|| fs::remove_dir_all(p))
}

/// Append `contents` to `path`, creating it if absent (JSONL stores).
pub fn append(path: impl AsRef<Path>, contents: impl AsRef<[u8]>) -> io::Result<()> {
    use std::io::Write as _;
    let (p, c) = (path.as_ref(), contents.as_ref());
    retry(|| {
        let mut f = fs::OpenOptions::new().create(true).append(true).open(p)?;
        f.write_all(c)
    })
}

/// What a stat says about a file a caller needs to load.
#[derive(Debug)]
pub enum FileState {
    /// A regular file.
    Present,
    /// Nothing at the path, or something other than a regular file.
    Missing,
    /// The stat failed with an error other than not-found: the file may
    /// exist but can't be reached.
    Unreadable(io::Error),
}

/// Stats `path` once and classifies it; only `NotFound` reads as missing.
pub fn file_state(path: impl AsRef<Path>) -> FileState {
    match fs::metadata(path.as_ref()) {
        Ok(m) if m.is_file() => FileState::Present,
        Ok(_) => FileState::Missing,
        Err(e) if e.kind() == io::ErrorKind::NotFound => FileState::Missing,
        Err(e) => FileState::Unreadable(e),
    }
}

/// The profiles' sessions folders this process registered
/// ([`register_sessions_dir`]).
static SESSIONS_DIRS: std::sync::RwLock<Vec<std::path::PathBuf>> = std::sync::RwLock::new(Vec::new());

/// Registers a profile's sessions folder. A path under it is matched
/// against it ([`session_file`]), whatever the folders above it are named.
pub fn register_sessions_dir(dir: impl Into<std::path::PathBuf>) {
    let dir = dir.into();
    let mut dirs = SESSIONS_DIRS.write().unwrap_or_else(|e| e.into_inner());
    if !dirs.contains(&dir) {
        dirs.push(dir);
    }
}

/// The session folder and the `/`-separated path inside it, for a file under
/// `<..>/sessions/<id>/` or `<..>/sessions/.<id>.importing/`. `None` for any
/// other path, a folder named with a leading `.` that is not an import in
/// progress included.
///
/// A path under a registered sessions folder ([`register_sessions_dir`]) is
/// matched against that folder. Any other path follows the name rule: the
/// last `sessions` component followed by a session folder and a file
/// counts, except the `sessions` folder of a sync inbox
/// (`sync.<machine>.inbox`).
pub fn session_file(path: &Path) -> Option<(std::path::PathBuf, String)> {
    use std::path::Component;
    let registered = SESSIONS_DIRS
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .filter(|dir| path.starts_with(dir))
        .max_by_key(|dir| dir.components().count())
        .cloned();
    let parts: Vec<Component<'_>> = path.components().collect();
    let i = match registered {
        Some(dir) => dir.components().count().checked_sub(1)?,
        None => {
            let i = (0..parts.len().saturating_sub(2))
                .rev()
                .find(|&i| parts[i] == Component::Normal("sessions".as_ref()))?;
            if let Some(Component::Normal(parent)) = i.checked_sub(1).map(|p| parts[p]) {
                if is_inbox_name(&parent.to_string_lossy()) {
                    return None;
                }
            }
            i
        }
    };
    if parts.len() < i + 3 {
        return None;
    }
    let Component::Normal(id) = parts[i + 1] else { return None };
    let id = id.to_str()?;
    let importing = id.strip_prefix('.').and_then(|n| n.strip_suffix(".importing")).is_some_and(|n| !n.is_empty());
    if id.is_empty() || (id.starts_with('.') && !importing) {
        return None;
    }
    let mut rel = Vec::with_capacity(parts.len() - i - 2);
    for part in &parts[i + 2..] {
        let Component::Normal(name) = part else { return None };
        rel.push(name.to_str()?);
    }
    Some((parts[..=i + 1].iter().collect(), rel.join("/")))
}

/// A sync inbox folder's name: `sync.<machine>.inbox`.
fn is_inbox_name(name: &str) -> bool {
    name.strip_prefix("sync.").and_then(|n| n.strip_suffix(".inbox")).is_some_and(|m| !m.is_empty())
}

/// Debug builds assert that a whole-file write under a session folder
/// carries sealed bytes (`DSS1` or `DSR1`) and is not named like a sync
/// tool's copy of `key.json`.
fn guard_sealed(path: &Path, contents: &[u8]) {
    if cfg!(debug_assertions) {
        if let Some((_, rel)) = session_file(path) {
            debug_assert!(
                guard_exempt(&rel) || contents.starts_with(b"DSS1") || contents.starts_with(b"DSR1"),
                "plaintext write to a session file: {}",
                path.display()
            );
        }
        guard_name(path);
    }
}

/// Debug builds assert that Daisy writes no session file named like a sync
/// tool's copy of `key.json` ([`is_key_file_copy`]).
fn guard_name(path: &Path) {
    if cfg!(debug_assertions) {
        if let Some((_, rel)) = session_file(path) {
            debug_assert!(!is_key_file_copy(&rel), "session file named like a copy of key.json: {}", path.display());
        }
    }
}

/// The session file the guard lets through as plaintext: the wrapped
/// session key at the top of the session folder.
fn guard_exempt(rel: &str) -> bool {
    rel == "key.json"
}

/// True when `rel`, a path inside a session folder, is a sync tool's
/// conflict copy of the session's `key.json` ([`is_conflict_copy`]).
pub fn is_key_file_copy(rel: &str) -> bool {
    is_conflict_copy(rel, "key.json")
}

/// True when `name` is a file sync tool's conflict copy of the file named
/// `original` in the same folder: Syncthing's
/// `<stem>.sync-conflict-<…>.<ext>`, Dropbox's
/// `<stem> (<…> conflicted copy <…>).<ext>`, OneDrive's
/// `<stem>-<machine>.<ext>`, iCloud Drive's `<stem> <n>.<ext>` with `<n>`
/// from 2, and Google Drive's `<stem>[Conflict].<ext>` and
/// `<stem> (<n>).<ext>` with `<n>` from 1. `<ext>` is the part of
/// `original` after its last `.`; `<machine>` is 1 to 15 ASCII letters,
/// digits and `-` that starts with a letter or digit, the shape of a
/// Windows computer name; `<n>` is 1 to 4 digits without a leading zero.
pub fn is_conflict_copy(name: &str, original: &str) -> bool {
    let (stem, ext) = match original.rsplit_once('.') {
        Some((stem, ext)) => (stem, Some(ext)),
        None => (original, None),
    };
    let Some(rest) = name.strip_prefix(stem) else { return false };
    let middle = match ext {
        Some(ext) => match rest.strip_suffix(ext).and_then(|r| r.strip_suffix('.')) {
            Some(m) => m,
            None => return false,
        },
        None => rest,
    };
    let machine = |m: &str| {
        (1..=15).contains(&m.len())
            && m.starts_with(|c: char| c.is_ascii_alphanumeric())
            && m.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
    };
    let number = |n: &str, from: u32| {
        (1..=4).contains(&n.len())
            && !n.starts_with('0')
            && n.bytes().all(|b| b.is_ascii_digit())
            && n.parse::<u32>().is_ok_and(|n| n >= from)
    };
    middle.starts_with(".sync-conflict-")
        || (middle.starts_with(" (") && middle.ends_with(')') && middle.contains("conflicted copy"))
        || middle.strip_prefix('-').is_some_and(machine)
        || middle.strip_prefix(' ').is_some_and(|n| number(n, 2))
        || middle == "[Conflict]"
        || middle.strip_prefix(" (").and_then(|m| m.strip_suffix(')')).is_some_and(|n| number(n, 1))
}

/// `root` joined with a stored relative path (`chunks/0001/mic.wav`), one
/// component at a time, so the result carries the platform separator
/// throughout and stays under `root`. Both `/` and `\` separate components
/// in `rel`. Only plain names are joined: empty, `.` and `..` components, a
/// leading root, and a drive or UNC prefix are dropped.
pub fn join_rel(root: impl AsRef<Path>, rel: impl AsRef<Path>) -> std::path::PathBuf {
    use std::path::Component;
    let mut out = root.as_ref().to_path_buf();
    for part in rel.as_ref().to_string_lossy().split(['/', '\\']) {
        let mut parts = Path::new(part).components();
        if let (Some(Component::Normal(name)), None) = (parts.next(), parts.next()) {
            out.push(name);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    #[test]
    fn file_state_tells_missing_from_unreachable() {
        let td = std::env::temp_dir().join(format!("syncsafe-state-{}", std::process::id()));
        fs::create_dir_all(&td).unwrap();
        let file = td.join("model.onnx");
        fs::write(&file, b"x").unwrap();
        assert!(matches!(file_state(&file), FileState::Present));
        assert!(matches!(file_state(td.join("absent.onnx")), FileState::Missing));
        assert!(matches!(file_state(&td), FileState::Missing), "a directory is not the file");
        // A path through a regular file fails with ENOTDIR, which is not
        // not-found: the stat could not answer.
        #[cfg(unix)]
        assert!(matches!(file_state(file.join("inner")), FileState::Unreadable(_)));
        let _ = fs::remove_dir_all(&td);
    }

    #[test]
    fn a_profile_root_named_inbox_holds_session_files() {
        let profile = std::env::temp_dir().join(format!("syncsafe-roots-{}", std::process::id())).join("inbox");
        let sessions = profile.join("sessions");
        let (dir, rel) = session_file(&sessions.join("daisy-1/notes.md")).expect("by name");
        assert_eq!((dir, rel.as_str()), (sessions.join("daisy-1"), "notes.md"));
        let dotted = profile.with_file_name("work.inbox").join("sessions/daisy-1/notes.md");
        assert!(session_file(&dotted).is_some(), "only sync.<machine>.inbox is an inbox");
        let inbox = profile.join("sync.mac-1.inbox/sessions/f9310070/daisy-1/manifest.json");
        assert!(session_file(&inbox).is_none());
        assert!(session_file(&profile.join("sync..inbox/sessions/a/b")).is_some());

        // Registered, a sessions folder under a sync inbox's name still holds sessions.
        let odd = profile.with_file_name("sync.box.inbox").join("sessions");
        assert!(session_file(&odd.join("daisy-2/notes.md")).is_none());
        register_sessions_dir(&odd);
        let (dir, rel) = session_file(&odd.join("daisy-2/notes.md")).expect("registered");
        assert_eq!((dir, rel.as_str()), (odd.join("daisy-2"), "notes.md"));
        assert!(session_file(&odd.join("daisy-2")).is_none());
        assert!(session_file(&odd.join(".trash/notes.md")).is_none());
    }

    #[test]
    fn a_registered_sessions_folder_wins_over_a_nested_one() {
        let sessions = std::env::temp_dir().join(format!("syncsafe-nested-{}", std::process::id())).join("sessions");
        let nested = sessions.join("daisy-1/attachments/sessions/x/file.bin");
        let (dir, _) = session_file(&nested).unwrap();
        assert_eq!(dir, sessions.join("daisy-1/attachments/sessions/x"), "by name: the last `sessions`");
        register_sessions_dir(&sessions);
        let (dir, rel) = session_file(&nested).unwrap();
        assert_eq!((dir, rel.as_str()), (sessions.join("daisy-1"), "attachments/sessions/x/file.bin"));
    }

    #[test]
    fn conflict_copies_are_named_the_way_sync_tools_name_them() {
        for name in [
            "key.sync-conflict-20261027-101010-ABCDEFG.json",
            "key (Laptop's conflicted copy 2026-10-27).json",
            "key-LAPTOP.json",
            "key-DESKTOP-7.json",
            "key 2.json",
            "key 13.json",
            "key[Conflict].json",
            "key (1).json",
            "key (12).json",
        ] {
            assert!(is_key_file_copy(name), "{name}");
        }
        for name in [
            "key.json",
            "key.json.tmp.1.2",
            "keynote.json",
            "key-a.b.json",
            "key-.json",
            "key--x.json",
            "key-a-machine-name-too-long.json",
            "chunks/key-LAPTOP.json",
            "key 1.json",
            "key 0.json",
            "key 02.json",
            "key 12345.json",
            "key 2 .json",
            "key2.json",
            "key .json",
            "key [Conflict].json",
            "key[conflict].json",
            "key (0).json",
            "key (01).json",
            "key ().json",
            "key (a).json",
            "key(1).json",
            "chunks/key 2.json",
        ] {
            assert!(!is_key_file_copy(name), "{name}");
        }
        assert!(is_conflict_copy("notes.sync-conflict-20261027-101010-ABCDEFG.md", "notes.md"));
        assert!(is_conflict_copy("notes 2.md", "notes.md"));
        assert!(is_conflict_copy("notes[Conflict].md", "notes.md"));
        assert!(!is_conflict_copy("notes 2.md.tmp", "notes.md"));
        assert!(!is_conflict_copy("notes.md", "notes.md"));
    }

    #[test]
    #[should_panic(expected = "named like a copy of key.json")]
    fn guard_refuses_a_session_file_named_like_a_key_copy() {
        let dir = std::env::temp_dir().join(format!("syncsafe-guard-name-{}", std::process::id())).join("sessions/daisy-1");
        fs::create_dir_all(&dir).unwrap();
        let _ = create_sealed(dir.join("key-LAPTOP.json"));
    }

    #[test]
    fn guard_lets_only_the_top_key_json_through() {
        assert!(guard_exempt("key.json"));
        assert!(!guard_exempt("chunks/key.json"));
    }

    #[test]
    #[should_panic(expected = "plaintext file created in a session")]
    fn guard_refuses_a_plaintext_create_in_a_session() {
        let dir = std::env::temp_dir().join(format!("syncsafe-guard-{}", std::process::id())).join("sessions/daisy-1");
        fs::create_dir_all(&dir).unwrap();
        let _ = create(dir.join("mic.wav"));
    }

    #[test]
    fn create_sealed_and_create_outside_a_session_pass_the_guard() {
        let root = std::env::temp_dir().join(format!("syncsafe-guard-ok-{}", std::process::id()));
        let dir = root.join("sessions/daisy-1");
        fs::create_dir_all(&dir).unwrap();
        create_sealed(dir.join("mic.wav")).unwrap();
        create(dir.join("key.json")).unwrap();
        create(root.join("export.wav")).unwrap();
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn join_rel_uses_one_separator() {
        let root = Path::new("root");
        let want: std::path::PathBuf = ["root", "chunks", "0001", "mic.wav"].iter().collect();
        assert_eq!(join_rel(root, "chunks/0001/mic.wav"), want);
        assert_eq!(join_rel(root, "chunks\\0001\\mic.wav"), want);
        assert_eq!(join_rel(root, "./chunks//0001/mic.wav"), want);
        assert_eq!(join_rel(root, ""), root);
    }

    #[test]
    fn join_rel_never_leaves_root() {
        let root = Path::new("root");
        let inside: std::path::PathBuf = ["root", "chunks", "mic.wav"].iter().collect();
        assert_eq!(join_rel(root, "../../chunks/mic.wav"), inside);
        assert_eq!(join_rel(root, "chunks/../mic.wav"), ["root", "chunks", "mic.wav"].iter().collect::<std::path::PathBuf>());
        assert_eq!(join_rel(root, "/chunks/mic.wav"), inside);
        assert_eq!(join_rel(root, "\\\\server\\share\\chunks\\mic.wav"), ["root", "server", "share", "chunks", "mic.wav"].iter().collect::<std::path::PathBuf>());
        // A drive prefix is a component of its own on Windows and is
        // dropped there; elsewhere `C:` is an ordinary file name.
        let drive = join_rel(root, "C:\\Windows\\mic.wav");
        assert!(drive.starts_with(root), "{}", drive.display());
        #[cfg(windows)]
        assert_eq!(drive, ["root", "Windows", "mic.wav"].iter().collect::<std::path::PathBuf>());
        for rel in ["..", "../..", "/", "\\", "C:", "C:\\"] {
            let p = join_rel(root, rel);
            assert!(p.starts_with(root) && !p.components().any(|c| c == std::path::Component::ParentDir), "{rel}: {}", p.display());
        }
    }

    /// Every temp file a write left behind in `dir`.
    fn leftover_tmps(dir: &Path) -> Vec<std::path::PathBuf> {
        fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.to_string_lossy().contains(".tmp"))
            .collect()
    }

    #[test]
    fn retries_transient_then_succeeds() {
        let calls = AtomicU32::new(0);
        let out = retry(|| {
            if calls.fetch_add(1, Ordering::SeqCst) < 3 {
                Err(io::Error::from_raw_os_error(32))
            } else {
                Ok(7)
            }
        })
        .unwrap();
        assert_eq!(out, 7);
        assert_eq!(calls.load(Ordering::SeqCst), 4);
    }

    #[test]
    fn permission_denied_retries_and_eventually_fails() {
        let calls = AtomicU32::new(0);
        let err = retry::<()>(|| {
            calls.fetch_add(1, Ordering::SeqCst);
            Err(io::Error::new(io::ErrorKind::PermissionDenied, "locked"))
        })
        .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(calls.load(Ordering::SeqCst), 1 + BACKOFF_MS.len() as u32);
    }

    #[test]
    fn non_transient_fails_immediately() {
        let calls = AtomicU32::new(0);
        let err = retry::<()>(|| {
            calls.fetch_add(1, Ordering::SeqCst);
            Err(io::Error::new(io::ErrorKind::NotFound, "gone"))
        })
        .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn concurrent_writers_all_succeed() {
        let dir = std::env::temp_dir().join(format!("syncsafe-race-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let target = dir.join("shared.json");
        std::thread::scope(|s| {
            for i in 0..8 {
                let target = target.clone();
                s.spawn(move || {
                    for _ in 0..20 {
                        write_atomic(&target, format!("{i}").as_bytes()).expect("write_atomic");
                    }
                });
            }
        });
        let leftovers = leftover_tmps(&dir);
        assert!(leftovers.is_empty(), "temp files left behind: {leftovers:?}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn write_atomic_replaces_and_cleans_tmp() {
        let dir = std::env::temp_dir().join(format!("syncsafe-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let target = dir.join("value.json");
        write_atomic(&target, b"one").unwrap();
        write_atomic(&target, b"two").unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"two");
        let leftovers = leftover_tmps(&dir);
        assert!(leftovers.is_empty(), "temp files left behind: {leftovers:?}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn append_creates_and_appends() {
        let dir = std::env::temp_dir().join(format!("syncsafe-append-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let target = dir.join("log.jsonl");
        append(&target, b"a\n").unwrap();
        append(&target, b"b\n").unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"a\nb\n");
        let _ = fs::remove_dir_all(&dir);
    }
}
