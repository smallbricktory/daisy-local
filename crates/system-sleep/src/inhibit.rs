// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Sleep / idle inhibition, per OS.
//!
//! Best-effort RAII guard: Linux spawns `systemd-inhibit … cat` and holds its
//! stdin pipe; macOS creates an IOKit `PreventUserIdleSystemSleep` assertion;
//! Windows holds a `PowerRequestSystemRequired` power request, listed by
//! `powercfg /requests`. Acquire and release are logged at INFO with the
//! mechanism and reason. Failure is logged and recording continues.

#[cfg(target_os = "linux")]
use std::process::{Child, Command, Stdio};

pub struct SleepInhibitor {
    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos", target_os = "windows")),
        allow(dead_code)
    )]
    reason: String,
    #[cfg(target_os = "linux")]
    child: Option<Child>,
    #[cfg(target_os = "macos")]
    assertion: Option<u32>,
    #[cfg(target_os = "windows")]
    request: Option<windows_request::PowerRequest>,
}

// ── Linux: systemd-inhibit child ────────────────────────────────────────────
#[cfg(target_os = "linux")]
const LINUX_MECHANISM: &str = "systemd-inhibit sleep:idle:handle-lid-switch mode=block";

#[cfg(target_os = "linux")]
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
        let child = match result {
            Ok(child) => {
                log::info!(
                    "sleep inhibitor: acquired ({LINUX_MECHANISM}, pid {}; reason: {why})",
                    child.id()
                );
                Some(child)
            }
            Err(e) => {
                log::warn!("sleep inhibitor: systemd-inhibit unavailable: {e}; sleep inhibition disabled");
                None
            }
        };
        Self { reason: why.to_string(), child }
    }
}

#[cfg(target_os = "linux")]
impl Drop for SleepInhibitor {
    fn drop(&mut self) {
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
            log::info!("sleep inhibitor: released ({LINUX_MECHANISM}; reason: {})", self.reason);
        }
    }
}

// ── macOS: IOKit power assertion ────────────────────────────────────────────
#[cfg(target_os = "macos")]
mod iokit {
    use std::os::raw::c_void;

    #[link(name = "IOKit", kind = "framework")]
    extern "C" {
        pub fn IOPMAssertionCreateWithName(
            assertion_type: *const c_void, // CFStringRef
            level: u32,
            name: *const c_void, // CFStringRef
            id: *mut u32,
        ) -> i32;
        pub fn IOPMAssertionRelease(id: u32) -> i32;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        pub fn CFStringCreateWithCString(
            alloc: *const c_void,
            c_str: *const std::os::raw::c_char,
            encoding: u32,
        ) -> *const c_void;
        pub fn CFRelease(cf: *const c_void);
    }
    pub const K_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;
    pub const K_IOPM_ASSERTION_LEVEL_ON: u32 = 255;
}

#[cfg(target_os = "macos")]
const MACOS_MECHANISM: &str = "IOPMAssertion PreventUserIdleSystemSleep";

#[cfg(target_os = "macos")]
impl SleepInhibitor {
    pub fn try_acquire(why: &str) -> Self {
        use iokit::*;
        let type_c = std::ffi::CString::new("PreventUserIdleSystemSleep").unwrap();
        let why_c = std::ffi::CString::new(why).unwrap_or_default();
        let mut id: u32 = 0;
        let assertion = unsafe {
            let cf_type = CFStringCreateWithCString(
                std::ptr::null(),
                type_c.as_ptr(),
                K_CF_STRING_ENCODING_UTF8,
            );
            let cf_name = CFStringCreateWithCString(
                std::ptr::null(),
                why_c.as_ptr(),
                K_CF_STRING_ENCODING_UTF8,
            );
            let rc =
                IOPMAssertionCreateWithName(cf_type, K_IOPM_ASSERTION_LEVEL_ON, cf_name, &mut id);
            if !cf_type.is_null() {
                CFRelease(cf_type);
            }
            if !cf_name.is_null() {
                CFRelease(cf_name);
            }
            if rc == 0 {
                log::info!("sleep inhibitor: acquired ({MACOS_MECHANISM}, id {id}; reason: {why})");
                Some(id)
            } else {
                log::warn!(
                    "sleep inhibitor: IOPMAssertionCreateWithName rc={rc}; sleep inhibition disabled"
                );
                None
            }
        };
        Self { reason: why.to_string(), assertion }
    }
}

#[cfg(target_os = "macos")]
impl Drop for SleepInhibitor {
    fn drop(&mut self) {
        if let Some(id) = self.assertion.take() {
            let rc = unsafe { iokit::IOPMAssertionRelease(id) };
            log::info!(
                "sleep inhibitor: released ({MACOS_MECHANISM}, id {id}, rc={rc}; reason: {})",
                self.reason
            );
        }
    }
}

// ── Windows: power request ──────────────────────────────────────────────────
#[cfg(target_os = "windows")]
mod windows_request {
    use windows::core::PWSTR;
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::System::Power::{
        PowerClearRequest, PowerCreateRequest, PowerRequestSystemRequired, PowerSetRequest,
    };
    use windows::Win32::System::Threading::{
        POWER_REQUEST_CONTEXT_SIMPLE_STRING, REASON_CONTEXT, REASON_CONTEXT_0,
    };

    /// `POWER_REQUEST_CONTEXT_VERSION` from minwinbase.h.
    const POWER_REQUEST_CONTEXT_VERSION: u32 = 0;

    pub const MECHANISM: &str = "PowerSetRequest PowerRequestSystemRequired";

    /// An open power request object with `PowerRequestSystemRequired` set.
    pub struct PowerRequest(HANDLE);

    // SAFETY: a power request handle is a kernel object handle; PowerClearRequest
    // and CloseHandle accept it from any thread.
    unsafe impl Send for PowerRequest {}
    unsafe impl Sync for PowerRequest {}

    impl PowerRequest {
        pub fn create(why: &str) -> windows::core::Result<Self> {
            let mut wide: Vec<u16> = why.encode_utf16().chain(std::iter::once(0)).collect();
            let context = REASON_CONTEXT {
                Version: POWER_REQUEST_CONTEXT_VERSION,
                Flags: POWER_REQUEST_CONTEXT_SIMPLE_STRING,
                Reason: REASON_CONTEXT_0 { SimpleReasonString: PWSTR(wide.as_mut_ptr()) },
            };
            let handle = unsafe { PowerCreateRequest(&context) }?;
            if let Err(e) = unsafe { PowerSetRequest(handle, PowerRequestSystemRequired) } {
                unsafe {
                    let _ = CloseHandle(handle);
                }
                return Err(e);
            }
            Ok(Self(handle))
        }
    }

    impl Drop for PowerRequest {
        fn drop(&mut self) {
            unsafe {
                if let Err(e) = PowerClearRequest(self.0, PowerRequestSystemRequired) {
                    log::warn!("sleep inhibitor: PowerClearRequest failed: {e}");
                }
                let _ = CloseHandle(self.0);
            }
        }
    }
}

#[cfg(target_os = "windows")]
impl SleepInhibitor {
    pub fn try_acquire(why: &str) -> Self {
        let request = match windows_request::PowerRequest::create(why) {
            Ok(r) => {
                log::info!(
                    "sleep inhibitor: acquired ({}; reason: {why})",
                    windows_request::MECHANISM
                );
                Some(r)
            }
            Err(e) => {
                log::warn!("sleep inhibitor: power request failed: {e}; sleep inhibition disabled");
                None
            }
        };
        Self { reason: why.to_string(), request }
    }
}

#[cfg(target_os = "windows")]
impl Drop for SleepInhibitor {
    fn drop(&mut self) {
        if let Some(request) = self.request.take() {
            drop(request);
            log::info!(
                "sleep inhibitor: released ({}; reason: {})",
                windows_request::MECHANISM,
                self.reason
            );
        }
    }
}

// ── Other targets: no-op ────────────────────────────────────────────────────
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
impl SleepInhibitor {
    pub fn try_acquire(why: &str) -> Self {
        Self { reason: why.to_string() }
    }
}
