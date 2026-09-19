// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! SpeechAnalyzer bridge: safe Rust wrapper over the Swift shim's C ABI
//! (`sa_shim.swift`, compiled by build.rs on macOS). Off-macOS only
//! `sa_available()` exists and returns false — engine choice is gated on
//! runtime availability, never on compile-time platform.

pub mod batch;
pub mod realtime;

use crate::streaming::agreement::StreamToken;

/// One callback batch from the analyzer: `(tokens, is_final)`. Volatile
/// batches (`false`) carry the whole current hypothesis and replace the
/// previous one; final batches (`true`) are committed text that never
/// revises earlier finals.
pub type SaBatch = (Vec<StreamToken>, bool);

#[cfg(not(target_os = "macos"))]
pub fn sa_available() -> bool {
    false
}

/// Usable now: capability AND system-locale assets on disk.
#[cfg(not(target_os = "macos"))]
pub fn sa_ready() -> bool {
    false
}

#[cfg(not(target_os = "macos"))]
pub fn sa_assets_install_blocking(_locale: &str) -> Result<(), String> {
    Err("SpeechAnalyzer is macOS-only".into())
}

#[cfg(target_os = "macos")]
pub use mac::{sa_assets_install_blocking, sa_available, sa_ready, SaSession};

#[cfg(target_os = "macos")]
mod mac {
    use super::SaBatch;
    use crate::streaming::agreement::StreamToken;
    use std::ffi::{c_char, c_void, CStr, CString};
    use tokio::sync::mpsc;

    // Word batches cross as parallel arrays (texts/start_s/end_s) — a
    // struct-typed callback parameter is not C-representable from Swift.
    type SaWordsCb =
        extern "C" fn(*mut c_void, *const *const c_char, *const f64, *const f64, i32, bool);

    extern "C" {
        fn sa_is_available() -> bool;
        fn sa_assets_installed(locale: *const c_char) -> bool;
        fn sa_assets_install(locale: *const c_char, err: *mut c_char) -> bool;
        fn sa_create(
            locale: *const c_char,
            cb: SaWordsCb,
            ctx: *mut c_void,
            err: *mut c_char,
        ) -> *mut c_void;
        fn sa_feed(h: *mut c_void, samples: *const f32, n: i32) -> i32;
        fn sa_flush(h: *mut c_void);
        fn sa_destroy(h: *mut c_void);
    }

    /// The OS can transcribe on this machine (macOS 26+ and ≥1 supported
    /// locale). Cached on the Swift side.
    pub fn sa_available() -> bool {
        unsafe { sa_is_available() }
    }

    /// Usable now: capability AND system-locale assets on disk. Assets are
    /// fetched by `sa_assets_install_blocking` (engine selection / wizard).
    pub fn sa_ready() -> bool {
        if !sa_available() {
            return false;
        }
        let empty = std::ffi::CString::new("").unwrap();
        unsafe { sa_assets_installed(empty.as_ptr()) }
    }

    /// Blocking OS asset download (`""` = system locale). No byte progress
    /// exists on this path — call from `spawn_blocking` and show an
    /// indeterminate state.
    pub fn sa_assets_install_blocking(locale: &str) -> Result<(), String> {
        let c_locale = CString::new(locale).map_err(|e| e.to_string())?;
        let mut err = [0u8; 256];
        let ok = unsafe { sa_assets_install(c_locale.as_ptr(), err.as_mut_ptr() as *mut c_char) };
        if ok {
            Ok(())
        } else {
            Err(CStr::from_bytes_until_nul(&err)
                .map(|c| c.to_string_lossy().into_owned())
                .unwrap_or_else(|_| "unknown shim error".into()))
        }
    }

    struct CbCtx {
        tx: mpsc::UnboundedSender<SaBatch>,
    }

    extern "C" fn words_cb(
        ctx: *mut c_void,
        texts: *const *const c_char,
        starts: *const f64,
        ends: *const f64,
        n: i32,
        is_final: bool,
    ) {
        if ctx.is_null() || texts.is_null() || starts.is_null() || ends.is_null() {
            return;
        }
        let ctx = unsafe { &*(ctx as *const CbCtx) };
        let mut toks: Vec<StreamToken> = Vec::with_capacity(n.max(0) as usize);
        for i in 0..n.max(0) as isize {
            let t = unsafe { *texts.offset(i) };
            if t.is_null() {
                continue;
            }
            let text = unsafe { CStr::from_ptr(t) }.to_string_lossy().into_owned();
            toks.push(StreamToken {
                text,
                start_ms: (unsafe { *starts.offset(i) } * 1000.0) as i64,
                end_ms: (unsafe { *ends.offset(i) } * 1000.0) as i64,
            });
        }
        // Receiver gone = session shutting down; drop silently.
        let _ = ctx.tx.send((toks, is_final));
    }

    /// A live analyzer session. `feed` 16 kHz mono f32; batches arrive on the
    /// channel returned by `create`; `flush` ends input and blocks until the
    /// final callbacks have fired (call it from `spawn_blocking`).
    pub struct SaSession {
        h: *mut c_void,
        // Keeps the callback context alive for the shim's lifetime.
        _ctx: Box<CbCtx>,
    }

    // The shim handle is internally synchronized (calls block on an internal
    // queue); the raw pointer is only ever used from one owner.
    unsafe impl Send for SaSession {}

    impl SaSession {
        /// Empty `locale` = system locale.
        pub fn create(locale: &str) -> Result<(Self, mpsc::UnboundedReceiver<SaBatch>), String> {
            let (tx, rx) = mpsc::unbounded_channel();
            let ctx = Box::new(CbCtx { tx });
            let c_locale = CString::new(locale).map_err(|e| e.to_string())?;
            let mut err = [0u8; 256];
            let h = unsafe {
                sa_create(
                    c_locale.as_ptr(),
                    words_cb,
                    &*ctx as *const CbCtx as *mut c_void,
                    err.as_mut_ptr() as *mut c_char,
                )
            };
            if h.is_null() {
                let msg = CStr::from_bytes_until_nul(&err)
                    .map(|c| c.to_string_lossy().into_owned())
                    .unwrap_or_else(|_| "unknown shim error".into());
                return Err(msg);
            }
            Ok((Self { h, _ctx: ctx }, rx))
        }

        /// Returns false when the shim rejected the buffer.
        pub fn feed(&self, samples: &[f32]) -> bool {
            if samples.is_empty() {
                return true;
            }
            unsafe { sa_feed(self.h, samples.as_ptr(), samples.len() as i32) >= 0 }
        }

        /// End input + finalize; blocks until final callbacks fired.
        pub fn flush(&self) {
            unsafe { sa_flush(self.h) }
        }
    }

    impl Drop for SaSession {
        fn drop(&mut self) {
            unsafe { sa_destroy(self.h) }
        }
    }
}
