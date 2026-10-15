// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! System suspend / resume notifications, per OS.
//!
//! - Windows: `PowerRegisterSuspendResumeNotification` with
//!   `DEVICE_NOTIFY_CALLBACK` (`PBT_APMSUSPEND`, `PBT_APMRESUMEAUTOMATIC`).
//! - macOS: `IORegisterForSystemPower` on a dedicated run-loop thread
//!   (`kIOMessageSystemWillSleep`, acknowledged with `IOAllowPowerChange`
//!   after the handler returns; `kIOMessageSystemHasPoweredOn`).
//! - Linux: logind's `PrepareForSleep(bool)` signal on the system bus. While
//!   `delay_wanted` returns true the monitor holds a logind `sleep` delay
//!   inhibitor lock, released after the suspend handler returns and taken
//!   again on resume.
//!
//! The handler runs on a monitor-owned or OS thread, once per event. The
//! system proceeds into sleep once the `Suspend` handler returns, or after
//! the OS's own limit (Windows about 2 s, macOS 30 s, logind
//! `InhibitDelayMaxSec`).

use std::sync::Arc;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerEvent {
    /// The system is about to sleep.
    Suspend,
    /// The system woke from sleep.
    Resume,
}

pub type PowerHandler = Arc<dyn Fn(PowerEvent) + Send + Sync>;

/// Whether the Linux monitor should hold a delay inhibitor lock right now.
pub type DelayWanted = Arc<dyn Fn() -> bool + Send + Sync>;

/// When the Linux monitor asks logind for its delay lock: while wanted and
/// not held, once per stretch of wanting. A refusal is asked again only
/// after `want` turns false (recording stopped, or the system slept).
#[cfg(any(target_os = "linux", test))]
#[derive(Debug, Default)]
struct DelayRetry {
    refused: bool,
}

#[cfg(any(target_os = "linux", test))]
impl DelayRetry {
    fn should_request(&mut self, want: bool, held: bool) -> bool {
        if !want {
            self.refused = false;
            return false;
        }
        !held && !self.refused
    }

    fn refused(&mut self) {
        self.refused = true;
    }
}

/// Watches for system suspend and resume until dropped.
pub struct PowerMonitor {
    #[cfg(target_os = "linux")]
    _inner: linux::Monitor,
    #[cfg(target_os = "macos")]
    _inner: macos::Monitor,
    #[cfg(target_os = "windows")]
    _inner: windows::Monitor,
}

impl PowerMonitor {
    /// Starts the monitor. `None` when this OS offers no notification source
    /// or registration failed (logged).
    pub fn start(handler: PowerHandler, delay_wanted: DelayWanted) -> Option<Self> {
        #[cfg(target_os = "linux")]
        {
            linux::Monitor::start(handler, delay_wanted).map(|m| Self { _inner: m })
        }
        #[cfg(target_os = "macos")]
        {
            let _ = delay_wanted;
            macos::Monitor::start(handler).map(|m| Self { _inner: m })
        }
        #[cfg(target_os = "windows")]
        {
            let _ = delay_wanted;
            windows::Monitor::start(handler).map(|m| Self { _inner: m })
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
        {
            let _ = (handler, delay_wanted);
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::DelayRetry;

    #[test]
    fn a_refused_delay_lock_is_asked_again_only_after_want_turns_false() {
        let mut retry = DelayRetry::default();
        assert!(!retry.should_request(false, false), "not wanted");
        assert!(retry.should_request(true, false), "wanted, not held");
        retry.refused();
        assert!(!retry.should_request(true, false), "refused: no request every poll");
        assert!(!retry.should_request(true, false));
        assert!(!retry.should_request(false, false), "recording stopped or system asleep");
        assert!(retry.should_request(true, false), "asked again");
        assert!(!retry.should_request(true, true), "held");
    }
}
