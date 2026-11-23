// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Opts the current process out of Windows execution-speed throttling.
//!
//! Shared by the app and the `daisy-llm` worker, the two processes that run
//! heavy compute while windowless or minimized. Nothing happens off Windows.
//!

/// Turns execution-speed throttling off for the whole process and returns one
/// log line with the outcome and the state read back from the OS. `None`
/// off Windows.
pub fn opt_out_of_execution_throttling() -> Option<String> {
    #[cfg(target_os = "windows")]
    {
        Some(windows_impl::run())
    }
    #[cfg(not(target_os = "windows"))]
    {
        None
    }
}

#[cfg(target_os = "windows")]
mod windows_impl {
    use std::ffi::c_void;
    use windows::Win32::System::Threading::{
        GetCurrentProcess, GetProcessInformation, ProcessPowerThrottling, SetProcessInformation,
        PROCESS_POWER_THROTTLING_CURRENT_VERSION, PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
        PROCESS_POWER_THROTTLING_STATE,
    };

    const SIZE: u32 = std::mem::size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32;

    pub fn run() -> String {
        // ControlMask names the setting this call owns; StateMask 0 turns it off.
        let want = PROCESS_POWER_THROTTLING_STATE {
            Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
            ControlMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
            StateMask: 0,
        };
        // SAFETY: `want` is a live PROCESS_POWER_THROTTLING_STATE of `SIZE`
        // bytes; the pseudo-handle needs no close.
        let set = unsafe {
            SetProcessInformation(
                GetCurrentProcess(),
                ProcessPowerThrottling,
                &want as *const _ as *const c_void,
                SIZE,
            )
        };
        let mut got = PROCESS_POWER_THROTTLING_STATE {
            Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
            ControlMask: 0,
            StateMask: 0,
        };
        // SAFETY: `got` is a live PROCESS_POWER_THROTTLING_STATE of `SIZE` bytes.
        let read = unsafe {
            GetProcessInformation(
                GetCurrentProcess(),
                ProcessPowerThrottling,
                &mut got as *mut _ as *mut c_void,
                SIZE,
            )
        };
        let outcome = match set {
            Ok(()) => "execution-speed throttling opted out".to_string(),
            Err(e) => format!("execution-speed throttling opt-out failed: {e}"),
        };
        let state = match read {
            Ok(()) => format!(
                "control_mask={:#x} state_mask={:#x} execution_speed_throttled={}",
                got.ControlMask,
                got.StateMask,
                got.ControlMask & PROCESS_POWER_THROTTLING_EXECUTION_SPEED != 0
                    && got.StateMask & PROCESS_POWER_THROTTLING_EXECUTION_SPEED != 0,
            ),
            Err(e) => format!("readback failed: {e}"),
        };
        format!("windows power throttling: {outcome}; {state}")
    }
}
