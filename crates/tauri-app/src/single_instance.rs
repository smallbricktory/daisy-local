// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Single-instance guard: one daisy per profile.
//!
//! A lockfile in the profile root holds the owner PID. A launch is blocked
//! only when that PID is a live process with the same executable name as
//! this one. A stale lockfile is silently reclaimed by the next launch;
//! nothing is cleaned up on exit.

use std::path::Path;
use sysinfo::{get_current_pid, Pid, ProcessesToUpdate, System};

const LOCK_FILE: &str = "daisy.lock";

/// True when another live daisy instance already owns this profile.
pub fn another_instance_alive(profile_root: &Path) -> bool {
    let path = profile_root.join(LOCK_FILE);
    let Ok(contents) = syncsafe::read_to_string(&path) else {
        return false; // no lockfile → free
    };
    let Ok(pid) = contents.trim().parse::<u32>() else {
        return false; // garbage lockfile → treat as free, claim() overwrites it
    };
    if pid == std::process::id() {
        return false; // our own (re-entrant) claim
    }
    pid_is_sibling(pid)
}

/// Record this process as the profile's owner (overwrites any stale lockfile).
pub fn claim(profile_root: &Path) {
    let path = profile_root.join(LOCK_FILE);
    if let Err(e) = syncsafe::write(&path, std::process::id().to_string()) {
        log::warn!("single-instance: lockfile write failed for {}: {e}", path.display());
    }
}

/// True if `pid` is a live process whose executable name matches this one's.
/// A thread id is never a sibling, including one of this process's threads.
pub(crate) fn pid_is_sibling(pid: u32) -> bool {
    let Ok(me) = get_current_pid() else { return false };
    let target = Pid::from_u32(pid);
    let mut sys = System::new();
    sys.refresh_processes(ProcessesToUpdate::Some(&[target, me]), true);
    let my_name = sys.process(me).map(|p| p.name().to_string_lossy().into_owned());
    match (sys.process(target), my_name) {
        (Some(other), Some(my_name)) => {
            other.thread_kind().is_none() && other.name().to_string_lossy() == my_name
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_lockfile_is_free() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!another_instance_alive(dir.path()));
    }

    #[test]
    fn garbage_lockfile_is_free() {
        let dir = tempfile::tempdir().unwrap();
        syncsafe::write(dir.path().join(LOCK_FILE), "not-a-pid").unwrap();
        assert!(!another_instance_alive(dir.path()));
    }

    #[test]
    fn own_pid_is_not_a_blocker() {
        // A lockfile naming this process never blocks it.
        let dir = tempfile::tempdir().unwrap();
        claim(dir.path());
        assert!(!another_instance_alive(dir.path()));
    }

    /// Holds a child copy of this test binary alive for the sibling test.
    #[test]
    #[ignore = "run by another_live_copy_of_this_binary_is_a_sibling"]
    fn sibling_child_waits() {
        std::thread::sleep(std::time::Duration::from_secs(30));
    }

    #[test]
    fn another_live_copy_of_this_binary_is_a_sibling() {
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "single_instance::tests::sibling_child_waits", "--ignored", "--nocapture"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let sibling = pid_is_sibling(child.id());
        let _ = child.kill();
        let _ = child.wait();
        assert!(sibling, "a live copy of this binary was not taken for another instance");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_thread_of_this_process_is_not_a_sibling() {
        let comm = std::fs::read_to_string("/proc/self/comm").unwrap().trim().to_string();
        let (tid_tx, tid_rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
        let t = std::thread::Builder::new()
            .name(comm)
            .spawn(move || {
                tid_tx.send(unsafe { libc::gettid() } as u32).unwrap();
                let _ = done_rx.recv();
            })
            .unwrap();
        let tid = tid_rx.recv().unwrap();
        assert_ne!(tid, std::process::id());
        let sibling = pid_is_sibling(tid);
        done_tx.send(()).unwrap();
        t.join().unwrap();
        assert!(!sibling, "thread {tid} of this process was taken for another instance");
    }

    #[test]
    fn dead_pid_is_reclaimable() {
        // PID 0 is never a real userland process → treated as a stale lockfile.
        let dir = tempfile::tempdir().unwrap();
        syncsafe::write(dir.path().join(LOCK_FILE), "0").unwrap();
        assert!(!another_instance_alive(dir.path()));
    }
}
