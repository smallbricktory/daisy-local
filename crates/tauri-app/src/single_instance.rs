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
fn pid_is_sibling(pid: u32) -> bool {
    let Ok(me) = get_current_pid() else { return false };
    let target = Pid::from_u32(pid);
    let mut sys = System::new();
    sys.refresh_processes(ProcessesToUpdate::Some(&[target, me]), true);
    let my_name = sys.process(me).map(|p| p.name().to_string_lossy().into_owned());
    match (sys.process(target), my_name) {
        (Some(other), Some(my_name)) => other.name().to_string_lossy() == my_name,
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

    #[test]
    fn dead_pid_is_reclaimable() {
        // PID 0 is never a real userland process → treated as a stale lockfile.
        let dir = tempfile::tempdir().unwrap();
        syncsafe::write(dir.path().join(LOCK_FILE), "0").unwrap();
        assert!(!another_instance_alive(dir.path()));
    }
}
