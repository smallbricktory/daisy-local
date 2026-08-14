//! Sleep / idle inhibition, per OS.
//!
//! Best-effort RAII guard: Linux spawns `systemd-inhibit … cat` and holds its
//! stdin pipe; macOS creates an IOKit power assertion; Windows sets the
//! thread execution state. Failure is logged and recording continues.

#[cfg(target_os = "linux")]
use std::process::{Child, Command, Stdio};

pub struct SleepInhibitor {
    #[cfg(target_os = "linux")]
    child: Option<Child>,
    #[cfg(target_os = "macos")]
    assertion: Option<u32>,
    #[cfg(target_os = "windows")]
    release: Option<std::sync::mpsc::Sender<()>>,
}

// ── Linux: systemd-inhibit child ────────────────────────────────────────────
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

#[cfg(target_os = "linux")]
impl Drop for SleepInhibitor {
    fn drop(&mut self) {
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
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
                log::debug!("acquired sleep assertion (id {id})");
                Some(id)
            } else {
                log::warn!("IOPMAssertionCreateWithName rc={rc}; sleep inhibition disabled");
                None
            }
        };
        Self { assertion }
    }
}

#[cfg(target_os = "macos")]
impl Drop for SleepInhibitor {
    fn drop(&mut self) {
        if let Some(id) = self.assertion.take() {
            unsafe {
                let _ = iokit::IOPMAssertionRelease(id);
            }
        }
    }
}

// ── Windows: thread execution state ─────────────────────────────────────────
#[cfg(target_os = "windows")]
impl SleepInhibitor {
    // ES_CONTINUOUS is per-thread state; a dedicated parked thread holds it
    // so acquire and release happen on the same thread.
    pub fn try_acquire(why: &str) -> Self {
        use windows::Win32::System::Power::{
            SetThreadExecutionState, ES_CONTINUOUS, ES_SYSTEM_REQUIRED,
        };
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let reason = why.to_string();
        let spawned = std::thread::Builder::new()
            .name("daisy-sleep-inhibit".into())
            .spawn(move || {
                let prev = unsafe { SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED) };
                if prev.0 == 0 {
                    log::warn!(
                        "SetThreadExecutionState failed; sleep inhibition disabled ({reason})"
                    );
                    return;
                }
                log::debug!("acquired execution-state sleep inhibition");
                let _ = rx.recv();
                unsafe {
                    let _ = SetThreadExecutionState(ES_CONTINUOUS);
                }
            });
        match spawned {
            Ok(_) => Self { release: Some(tx) },
            Err(e) => {
                log::warn!("sleep inhibitor thread failed: {e}");
                Self { release: None }
            }
        }
    }
}

#[cfg(target_os = "windows")]
impl Drop for SleepInhibitor {
    fn drop(&mut self) {
        if let Some(tx) = self.release.take() {
            let _ = tx.send(());
        }
    }
}

// ── Other targets: no-op ────────────────────────────────────────────────────
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
impl SleepInhibitor {
    pub fn try_acquire(_why: &str) -> Self {
        Self {}
    }
}
