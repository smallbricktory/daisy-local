//! Sleep / idle inhibition via systemd-inhibit.
//!
//! Best-effort RAII guard. Spawns `systemd-inhibit ... cat` and holds its
//! stdin pipe; killing the child on Drop releases the inhibitor lock. When
//! systemd-inhibit is unavailable (non-systemd distros, sandboxed envs), a
//! warning is logged and recording continues without the lock.

use std::process::{Child, Command, Stdio};

pub struct SleepInhibitor {
    child: Option<Child>,
}

impl SleepInhibitor {
    /// Try to acquire a sleep+idle inhibitor lock for the given reason.
    /// Always returns a value — failure is logged but not propagated.
    pub fn try_acquire(why: &str) -> Self {
        let result = Command::new("systemd-inhibit")
            .args([
                "--what=sleep:idle:handle-lid-switch",
                "--who=daisy",
                &format!("--why={why}"),
                "--mode=block",
                "cat",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        match result {
            Ok(child) => {
                log::debug!("acquired sleep inhibitor (pid {})", child.id());
                Self { child: Some(child) }
            }
            Err(e) => {
                log::warn!("systemd-inhibit unavailable: {e}; sleep inhibition disabled");
                Self { child: None }
            }
        }
    }
}

impl Drop for SleepInhibitor {
    fn drop(&mut self) {
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}
