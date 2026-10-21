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
    retry(|| fs::write(p, c))
}

/// Write a temp file in the same directory, then rename over `path`.
/// Readers never observe a partial file; both the write and the rename are
/// retried.
pub fn write_atomic(path: impl AsRef<Path>, contents: impl AsRef<[u8]>) -> io::Result<()> {
    let p = path.as_ref();
    let tmp = tmp_path(p);
    let c = contents.as_ref();
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

pub fn create(path: impl AsRef<Path>) -> io::Result<fs::File> {
    let p = path.as_ref();
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
