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

/// Write `<path>.tmp` in the same directory, then rename over `path`.
/// Readers never observe a partial file; both the write and the rename are
/// retried.
pub fn write_atomic(path: impl AsRef<Path>, contents: impl AsRef<[u8]>) -> io::Result<()> {
    let p = path.as_ref();
    let tmp = tmp_path(p);
    let c = contents.as_ref();
    retry(|| fs::write(&tmp, c))?;
    let renamed = retry(|| fs::rename(&tmp, p));
    if renamed.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    renamed
}

fn tmp_path(p: &Path) -> std::path::PathBuf {
    let mut name = p.file_name().unwrap_or_default().to_os_string();
    name.push(".tmp");
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

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
    fn write_atomic_replaces_and_cleans_tmp() {
        let dir = std::env::temp_dir().join(format!("syncsafe-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let target = dir.join("value.json");
        write_atomic(&target, b"one").unwrap();
        write_atomic(&target, b"two").unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"two");
        assert!(!tmp_path(&target).exists());
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
