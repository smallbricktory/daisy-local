// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Linux playback of a session's `meeting.opus`. The decrypted audio is
//! written to a file in memory-backed storage and the player loads its
//! `file://` URL.
//!
//! - The storage is `$XDG_RUNTIME_DIR/daisy-media` when `XDG_RUNTIME_DIR` is
//!   an absolute path to a directory this user owns, with mode 0700, on
//!   tmpfs or ramfs. Otherwise it is `/dev/shm/daisy-media-<uid>` when
//!   `/dev/shm` is tmpfs or ramfs; that directory is created 0700 and
//!   refused when another user owns it. With neither, playback is refused
//!   and export still works. Memory-backed pages can reach swap when swap
//!   is unencrypted.
//! - Each process writes in its own subdirectory named by its pid. Startup
//!   deletes the subdirectories of processes that are no longer running.
//! - Directories are 0700 and each file 0600, under a random name that
//!   carries no session id. A file is written beside its final name and
//!   renamed into place, so the player never reads a partial file.
//! - One file exists per process: a new one replaces the last. The player
//!   loads it when a recording's page opens and releases it when the page
//!   closes; it is also deleted when that session is deleted, when the vault
//!   locks, before a profile switch restarts the app and at exit. A crash
//!   leaves it until the next launch or logout.

use crate::commands::playback::resolve_meeting_audio;
use crate::error::{AppError, Result};
use crate::state::{AppState, VaultState};
use recording::mixdown::MEETING_AUDIO_NAME;
use std::ffi::OsString;
use std::fs;
use std::io::{self, Read};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// The directory's name under the runtime directory.
const DIR_NAME: &str = "daisy-media";

/// `statfs` magic numbers of the memory-backed filesystems.
const TMPFS_MAGIC: u32 = 0x0102_1994;
const RAMFS_MAGIC: u32 = 0x8584_58f6;

/// The file the player may be reading and the session it holds.
struct Current {
    path: PathBuf,
    session: String,
}

/// This process's playback directory and the one file in it the player may
/// be reading.
pub struct Slot {
    /// The shared directory: `daisy-media` (or `daisy-media-<uid>`).
    base: PathBuf,
    /// This process's subdirectory of `base`.
    dir: PathBuf,
    current: Mutex<Option<Current>>,
}

impl Slot {
    /// The slot of process `pid` under `base`.
    pub fn new(base: PathBuf, pid: u32) -> Self {
        let dir = base.join(pid.to_string());
        Self { base, dir, current: Mutex::new(None) }
    }

    fn current(&self) -> std::sync::MutexGuard<'_, Option<Current>> {
        self.current.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Creates the shared directory (0700, tightening one that exists) and
    /// deletes everything in it except the subdirectories of other running
    /// processes.
    pub fn sweep(&self) -> io::Result<()> {
        let mut cur = self.current();
        ensure_dir(&self.base)?;
        for entry in fs::read_dir(&self.base)? {
            let entry = entry?;
            let path = entry.path();
            let other_live = entry
                .file_name()
                .to_str()
                .and_then(|n| n.parse::<u32>().ok())
                .is_some_and(|pid| path != self.dir && pid_alive(pid));
            if other_live {
                continue;
            }
            if entry.file_type()?.is_dir() {
                fs::remove_dir_all(&path)?;
            } else {
                fs::remove_file(&path)?;
            }
        }
        *cur = None;
        Ok(())
    }

    /// Writes `audio` (session `session`'s) to a new file and makes it the
    /// current one; the file it replaces is deleted. When `wanted` says no
    /// by the time the copy ends, the new file is deleted and `None` is
    /// returned. Copies run outside the slot's lock, so a `clear` waits for
    /// none of them.
    pub fn write(&self, session: &str, audio: &mut dyn Read, wanted: &dyn Fn() -> bool) -> io::Result<Option<PathBuf>> {
        ensure_dir(&self.base)?;
        ensure_dir(&self.dir)?;
        let name = uuid::Uuid::new_v4().simple().to_string();
        let part = self.dir.join(format!(".{name}.part"));
        let dst = self.dir.join(format!("{name}.opus"));
        let written = (|| {
            let mut f = fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&part)?;
            io::copy(audio, &mut f)?;
            fs::rename(&part, &dst)
        })();
        if let Err(e) = written {
            remove_quietly(&part);
            return Err(e);
        }
        let mut cur = self.current();
        if !wanted() {
            drop(cur);
            remove_quietly(&dst);
            return Ok(None);
        }
        let new = Current { path: dst.clone(), session: session.to_string() };
        if let Some(old) = cur.replace(new) {
            remove_quietly(&old.path);
        }
        Ok(Some(dst))
    }

    /// Deletes the current file when `is_it` says it is the one released;
    /// returns whether it was. A file a newer one replaced is gone already.
    fn release_if(&self, is_it: impl Fn(&Current) -> bool) -> bool {
        let mut cur = self.current();
        if !cur.as_ref().is_some_and(&is_it) {
            return false;
        }
        if let Some(old) = cur.take() {
            remove_quietly(&old.path);
        }
        true
    }

    /// Deletes the current file when it is `path`; returns whether it was.
    pub fn release(&self, path: &Path) -> bool {
        self.release_if(|c| c.path == path)
    }

    /// Deletes the current file when it holds session `session`'s audio.
    pub fn release_session(&self, session: &str) -> bool {
        self.release_if(|c| c.session == session)
    }

    /// Deletes the current file.
    pub fn clear(&self) {
        if let Some(old) = self.current().take() {
            remove_quietly(&old.path);
        }
    }
}

/// Creates `dir` with mode 0700, or sets 0700 on the directory there. A
/// symlink, a file, or a directory another user owns at `dir` is an error.
fn ensure_dir(dir: &Path) -> io::Result<()> {
    let m = match fs::symlink_metadata(dir) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => match fs::DirBuilder::new().mode(0o700).create(dir) {
            Ok(()) => return fs::set_permissions(dir, fs::Permissions::from_mode(0o700)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => fs::symlink_metadata(dir)?,
            Err(e) => return Err(e),
        },
        m => m?,
    };
    if !m.file_type().is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("{} is not a directory", dir.display()),
        ));
    }
    if m.uid() != euid() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{} belongs to another user", dir.display()),
        ));
    }
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
}

fn remove_quietly(path: &Path) {
    if let Err(e) = fs::remove_file(path) {
        if e.kind() != io::ErrorKind::NotFound {
            log::warn!("playback file {}: not removed: {e}", path.display());
        }
    }
}

fn euid() -> u32 {
    // SAFETY: geteuid has no preconditions and cannot fail.
    unsafe { libc::geteuid() }
}

/// True while a process with id `pid` exists.
fn pid_alive(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else { return false };
    if pid <= 0 {
        return false;
    }
    // SAFETY: signal 0 checks only that the process exists.
    if unsafe { libc::kill(pid, 0) } == 0 {
        return true;
    }
    io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// True when `path` is on tmpfs or ramfs.
fn is_memory_backed(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(c) = std::ffi::CString::new(path.as_os_str().as_bytes()) else { return false };
    // SAFETY: `c` is a valid C string and `s` a zeroed statfs that the call
    // fills in.
    let mut s: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statfs(c.as_ptr(), &mut s) } != 0 {
        return false;
    }
    let kind = s.f_type as u32;
    kind == TMPFS_MAGIC || kind == RAMFS_MAGIC
}

/// The playback directory: `daisy-media` under `runtime_dir` (the value of
/// `XDG_RUNTIME_DIR`) when that is an absolute path to a directory user
/// `uid` owns, with mode 0700, that `memory` says is memory-backed; else
/// `daisy-media-<uid>` under `shm` when `memory` says `shm` is; else `None`.
fn dir_with(runtime_dir: Option<OsString>, shm: &Path, uid: u32, memory: &dyn Fn(&Path) -> bool) -> Option<PathBuf> {
    let private = |p: &Path| {
        p.is_absolute()
            && fs::symlink_metadata(p)
                .is_ok_and(|m| m.file_type().is_dir() && m.uid() == uid && m.mode() & 0o777 == 0o700)
    };
    if let Some(run) = runtime_dir.map(PathBuf::from).filter(|p| private(p) && memory(p)) {
        return Some(run.join(DIR_NAME));
    }
    if shm.is_absolute() && fs::symlink_metadata(shm).is_ok_and(|m| m.file_type().is_dir()) && memory(shm) {
        return Some(shm.join(format!("{DIR_NAME}-{uid}")));
    }
    None
}

/// [`dir_with`] for this process: its effective uid, `/dev/shm`, and the
/// filesystem's own type.
pub fn dir_in(runtime_dir: Option<OsString>) -> Option<PathBuf> {
    dir_with(runtime_dir, Path::new("/dev/shm"), euid(), &is_memory_backed)
}

/// `file://` URL of an absolute path. Every byte outside the RFC 3986
/// unreserved set and `/` is percent-encoded.
pub fn file_url(path: &Path) -> String {
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

/// The app's slot; `None` inside when no memory-backed directory exists.
static SLOT: OnceLock<Option<Slot>> = OnceLock::new();

/// Sets the app's playback directory and sweeps it. Runs once, at startup.
/// With `None`, playback is refused.
pub fn init(base: Option<PathBuf>) {
    let slot = SLOT.get_or_init(|| base.map(|b| Slot::new(b, std::process::id())));
    match slot {
        Some(slot) => {
            if let Err(e) = slot.sweep() {
                log::warn!("playback directory {}: {e}", slot.base.display());
            }
        }
        None => log::warn!("playback: neither XDG_RUNTIME_DIR nor /dev/shm is private memory-backed storage; playback is off"),
    }
}

fn slot() -> Option<&'static Slot> {
    SLOT.get().and_then(Option::as_ref)
}

/// Decrypts the session's `meeting.opus` into the playback directory and
/// returns the file's `file://` URL. Requires an unlocked vault.
pub fn playback_url(app: &AppState, vs: &VaultState, session_id: &str) -> Result<String> {
    if !vs.is_unlocked() {
        return Err(AppError::VaultLocked);
    }
    resolve_meeting_audio(app, session_id)?;
    let slot = slot().ok_or_else(|| {
        AppError::Config("playback needs memory-backed storage (XDG_RUNTIME_DIR or /dev/shm); export the audio to listen".into())
    })?;
    // Read through the session path: a sealed file opens under the path
    // inside its session folder.
    let src = app.profile.session_path(session_id).join(MEETING_AUDIO_NAME);
    let path = sealfile::fs::open_stream(&src)
        .and_then(|mut r| slot.write(session_id, &mut r, &|| vs.is_unlocked()))
        .map_err(|e| AppError::Config(format!("playback audio: {e}")))?;
    // A lock during the copy deleted the file.
    let path = path.ok_or(AppError::VaultLocked)?;
    Ok(file_url(&path))
}

/// Deletes the playback file when `url` is its URL; returns whether it was.
pub fn release_url(url: &str) -> bool {
    slot().is_some_and(|slot| slot.release_if(|c| file_url(&c.path) == url))
}

/// Deletes the playback file when it holds session `session_id`'s audio;
/// a session's delete does this.
pub fn release_session(session_id: &str) {
    if let Some(slot) = slot() {
        slot.release_session(session_id);
    }
}

/// Deletes the playback file; the vault does this on lock, the app before a
/// restart and at exit.
pub fn clear() {
    if let Some(slot) = slot() {
        slot.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::ProfileDir;
    use std::sync::mpsc;

    const PID: u32 = 4242;
    fn yes() -> bool {
        true
    }

    fn mode(p: &Path) -> u32 {
        fs::metadata(p).unwrap().permissions().mode() & 0o777
    }

    fn files(dir: &Path) -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = fs::read_dir(dir).unwrap().map(|e| e.unwrap().path()).collect();
        v.sort();
        v
    }

    fn write(slot: &Slot, audio: &[u8]) -> PathBuf {
        slot.write("s", &mut &audio[..], &yes).unwrap().unwrap()
    }

    #[test]
    fn write_creates_a_private_file_under_a_random_name() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().join(DIR_NAME);
        let slot = Slot::new(base.clone(), PID);
        let p = write(&slot, b"OggS audio");
        assert_eq!(fs::read(&p).unwrap(), b"OggS audio");
        assert_eq!(mode(&base), 0o700);
        assert_eq!(mode(&slot.dir), 0o700);
        assert_eq!(mode(&p), 0o600);
        assert_eq!(p.parent().unwrap(), base.join(PID.to_string()));
        let name = p.file_name().unwrap().to_str().unwrap();
        assert!(name.ends_with(".opus") && name.len() == 32 + 5, "{name}");
        assert_eq!(files(&slot.dir), vec![p.clone()], "no partial file stays behind");
        let q = write(&slot, b"OggS other");
        assert_ne!(p, q);
    }

    #[test]
    fn a_new_file_replaces_the_one_in_use() {
        let tmp = tempfile::tempdir().unwrap();
        let slot = Slot::new(tmp.path().join(DIR_NAME), PID);
        let first = write(&slot, b"one");
        let second = write(&slot, b"two");
        assert!(!first.exists());
        assert_eq!(files(&slot.dir), vec![second.clone()]);
        // Releasing the replaced file leaves the current one.
        assert!(!slot.release(&first));
        assert!(second.exists());
        assert!(slot.release(&second));
        assert!(!second.exists());
        assert!(!slot.release(&second));
    }

    #[test]
    fn a_failed_write_leaves_no_file_and_keeps_the_current_one() {
        struct Broken;
        impl Read for Broken {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                Err(io::Error::other("read failed"))
            }
        }
        let tmp = tempfile::tempdir().unwrap();
        let slot = Slot::new(tmp.path().join(DIR_NAME), PID);
        let kept = write(&slot, b"kept");
        assert!(slot.write("s", &mut Broken, &yes).is_err());
        assert_eq!(files(&slot.dir), vec![kept]);
    }

    #[test]
    fn clear_deletes_the_current_file() {
        let tmp = tempfile::tempdir().unwrap();
        let slot = Slot::new(tmp.path().join(DIR_NAME), PID);
        let p = write(&slot, b"audio");
        slot.clear();
        assert!(!p.exists());
        slot.clear();
    }

    #[test]
    fn deleting_a_session_releases_only_its_own_file() {
        let tmp = tempfile::tempdir().unwrap();
        let slot = Slot::new(tmp.path().join(DIR_NAME), PID);
        let p = slot.write("daisy-1", &mut &b"audio"[..], &yes).unwrap().unwrap();
        assert!(!slot.release_session("daisy-2"));
        assert!(p.exists());
        assert!(slot.release_session("daisy-1"));
        assert!(!p.exists());
    }

    #[test]
    fn sweep_empties_the_directory_but_keeps_a_running_process_s() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().join(DIR_NAME);
        fs::create_dir(&base).unwrap();
        fs::set_permissions(&base, fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(base.join("left-by-a-crash.opus"), b"x").unwrap();
        fs::write(base.join(".half.part"), b"x").unwrap();
        fs::create_dir(base.join("stray")).unwrap();
        // A dead process's subdirectory, this pid's own from a crash, and a
        // running process's (the test runner's parent).
        let dead = base.join(i32::MAX.to_string());
        let live = base.join(std::os::unix::process::parent_id().to_string());
        let own = base.join(PID.to_string());
        for d in [&dead, &live, &own] {
            fs::create_dir(d).unwrap();
            fs::write(d.join("a.opus"), b"x").unwrap();
        }
        assert!(!pid_alive(i32::MAX as u32));
        Slot::new(base.clone(), PID).sweep().unwrap();
        assert_eq!(files(&base), vec![live.clone()]);
        assert!(live.join("a.opus").exists());
        assert_eq!(mode(&base), 0o700);
    }

    #[test]
    fn a_symlink_in_place_of_the_directory_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let elsewhere = tmp.path().join("elsewhere");
        fs::create_dir(&elsewhere).unwrap();
        let base = tmp.path().join(DIR_NAME);
        std::os::unix::fs::symlink(&elsewhere, &base).unwrap();
        let slot = Slot::new(base, PID);
        assert!(slot.sweep().is_err());
        assert!(slot.write("s", &mut &b"audio"[..], &yes).is_err());
        assert!(files(&elsewhere).is_empty());
    }

    #[test]
    fn the_directory_is_private_memory_backed_storage_or_none() {
        let tmp = tempfile::tempdir().unwrap();
        let uid = euid();
        let run = tmp.path().join("run");
        let shm = tmp.path().join("shm");
        fs::create_dir(&run).unwrap();
        fs::create_dir(&shm).unwrap();
        fs::set_permissions(&run, fs::Permissions::from_mode(0o700)).unwrap();
        let all = |_: &Path| true;
        let none = |_: &Path| false;
        let shm_only = |p: &Path| p == shm;

        assert_eq!(dir_with(Some(run.clone().into()), &shm, uid, &all), Some(run.join(DIR_NAME)));
        let in_shm = Some(shm.join(format!("{DIR_NAME}-{uid}")));
        // Unset, relative, on disk, another user's, readable by others, a
        // symlink: /dev/shm instead.
        assert_eq!(dir_with(None, &shm, uid, &all), in_shm);
        assert_eq!(dir_with(Some("".into()), &shm, uid, &all), in_shm);
        assert_eq!(dir_with(Some("run".into()), &shm, uid, &all), in_shm);
        assert_eq!(dir_with(Some(run.clone().into()), &shm, uid, &shm_only), in_shm);
        assert_eq!(dir_with(Some(run.clone().into()), &shm, uid + 1, &all), Some(shm.join(format!("{DIR_NAME}-{}", uid + 1))));
        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(&run, &link).unwrap();
        assert_eq!(dir_with(Some(link.into()), &shm, uid, &all), in_shm);
        fs::set_permissions(&run, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(dir_with(Some(run.clone().into()), &shm, uid, &all), in_shm);
        // Nothing memory-backed: no directory.
        assert_eq!(dir_with(Some(run.into()), &shm, uid, &none), None);
        assert_eq!(dir_with(None, &tmp.path().join("missing"), uid, &all), None);
    }

    #[test]
    fn a_directory_another_user_owns_is_refused() {
        if euid() == 0 {
            return;
        }
        // /dev/shm itself and / belong to root.
        let slot = Slot::new(PathBuf::from("/"), PID);
        assert_eq!(slot.sweep().unwrap_err().kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(slot.write("s", &mut &b"audio"[..], &yes).unwrap_err().kind(), io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn tmpfs_is_memory_backed_and_procfs_is_not() {
        assert!(is_memory_backed(Path::new("/dev/shm")));
        assert!(!is_memory_backed(Path::new("/proc")));
        assert!(!is_memory_backed(Path::new("/no/such/dir")));
    }

    #[test]
    fn file_urls_encode_reserved_bytes() {
        assert_eq!(file_url(Path::new("/run/user/1000/daisy-media/ab.opus")), "file:///run/user/1000/daisy-media/ab.opus");
        assert_eq!(file_url(Path::new("/a b/@#%.opus")), "file:///a%20b/%40%23%25.opus");
    }

    /// A reader that hands out `audio` once `go` fires.
    struct Gated {
        go: mpsc::Receiver<()>,
        audio: &'static [u8],
    }
    impl Read for Gated {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if !self.audio.is_empty() {
                let _ = self.go.recv();
            }
            let n = self.audio.len().min(buf.len());
            buf[..n].copy_from_slice(&self.audio[..n]);
            self.audio = &self.audio[n..];
            Ok(n)
        }
    }

    #[test]
    fn writes_that_overlap_leave_the_last_to_finish_current() {
        let tmp = tempfile::tempdir().unwrap();
        let slot = Slot::new(tmp.path().join(DIR_NAME), PID);
        let (go, wait) = mpsc::channel();
        std::thread::scope(|s| {
            // A starts first and finishes after B.
            let a = s.spawn(|| slot.write("a", &mut Gated { go: wait, audio: b"A" }, &yes).unwrap().unwrap());
            let b = write(&slot, b"B");
            go.send(()).unwrap();
            let a = a.join().unwrap();
            assert!(!b.exists(), "A replaced B");
            assert_eq!(fs::read(&a).unwrap(), b"A");
            assert_eq!(files(&slot.dir), vec![a.clone()]);
            assert!(!slot.release(&b));
            assert!(slot.release(&a));
        });
    }

    #[test]
    fn writes_that_start_together_on_a_new_directory_all_land() {
        for _ in 0..50 {
            let tmp = tempfile::tempdir().unwrap();
            let slot = Slot::new(tmp.path().join(DIR_NAME), PID);
            let start = std::sync::Barrier::new(4);
            std::thread::scope(|s| {
                let writers: Vec<_> = (0..4)
                    .map(|_| {
                        s.spawn(|| {
                            start.wait();
                            slot.write("s", &mut &b"audio"[..], &yes)
                        })
                    })
                    .collect();
                for w in writers {
                    assert!(w.join().unwrap().unwrap().is_some());
                }
            });
        }
    }

    #[test]
    fn a_lock_during_a_copy_waits_for_none_of_it_and_leaves_no_file() {
        let tmp = tempfile::tempdir().unwrap();
        let slot = Slot::new(tmp.path().join(DIR_NAME), PID);
        let kept = write(&slot, b"old");
        let unlocked = std::sync::atomic::AtomicBool::new(true);
        let (go, wait) = mpsc::channel();
        std::thread::scope(|s| {
            let copy = s.spawn(|| {
                slot.write("s", &mut Gated { go: wait, audio: b"new" }, &|| unlocked.load(std::sync::atomic::Ordering::SeqCst))
            });
            // The lock, mid-copy: the flag, then clear, which returns while
            // the copy still waits.
            unlocked.store(false, std::sync::atomic::Ordering::SeqCst);
            slot.clear();
            assert!(!kept.exists());
            go.send(()).unwrap();
            assert!(copy.join().unwrap().unwrap().is_none());
        });
        assert!(files(&slot.dir).is_empty(), "{:?}", files(&slot.dir));
    }

    /// The app's slot end to end: the decrypted audio of a session, a
    /// release by URL, a session's delete and the vault lock.
    #[test]
    fn playback_url_writes_the_audio_and_a_lock_deletes_it() {
        let _ring = crate::test_keys::exclusive();
        let tmp = tempfile::tempdir().unwrap();
        init(Some(tmp.path().join(DIR_NAME)));
        let current = || slot().unwrap().current().as_ref().map(|c| c.path.clone());
        let profile = ProfileDir::at(tmp.path().join("daisy")).unwrap();
        let app = AppState::new(profile);
        let vs = VaultState::new();
        *vs.keys.lock().unwrap() = Some(Default::default());
        for id in ["daisy-1", "daisy-2"] {
            let dir = app.profile.session_path(id);
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join(MEETING_AUDIO_NAME), format!("OggS {id}")).unwrap();
        }

        let url = playback_url(&app, &vs, "daisy-1").unwrap();
        assert!(!url.contains("daisy-1"), "{url}");
        let path = current().unwrap();
        assert_eq!(path.parent().unwrap(), tmp.path().join(DIR_NAME).join(std::process::id().to_string()));
        assert_eq!(file_url(&path), url);
        assert_eq!(fs::read(&path).unwrap(), b"OggS daisy-1");

        // Another session's audio replaces it; a release of the old URL
        // leaves the new file.
        let url2 = playback_url(&app, &vs, "daisy-2").unwrap();
        assert!(!path.exists());
        assert!(!release_url(&url));
        let path2 = current().unwrap();
        assert!(path2.exists());
        assert!(release_url(&url2));
        assert!(!path2.exists());

        assert!(matches!(playback_url(&app, &vs, "../escape"), Err(AppError::Config(_))));
        assert!(current().is_none());

        // Deleting the session releases its file.
        playback_url(&app, &vs, "daisy-2").unwrap();
        let path_del = current().unwrap();
        crate::commands::session::delete_session_impl(&app, &vs, "daisy-2").unwrap();
        assert!(!path_del.exists());
        assert!(current().is_none());

        playback_url(&app, &vs, "daisy-1").unwrap();
        let path3 = current().unwrap();
        crate::commands::lifecycle::lock_vault_impl(&app, &vs).unwrap();
        assert!(!path3.exists());
        assert!(matches!(playback_url(&app, &vs, "daisy-1"), Err(AppError::VaultLocked)));
        let dir = path3.parent().unwrap();
        assert!(files(dir).is_empty());
    }
}
