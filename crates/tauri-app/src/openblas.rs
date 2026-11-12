// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Best-effort cap on OpenBLAS intra-op threads: dlopens libopenblas,
//! resolves `openblas_set_num_threads`, and calls it. Failure is silent;
//! the `OPENBLAS_NUM_THREADS`/`OMP_NUM_THREADS` env vars remain the fallback.

#[cfg(target_os = "linux")]
pub fn cap_openblas_threads(n: i32) {
    use std::ffi::{c_int, c_void};
    type SetThreads = unsafe extern "C" fn(c_int);
    unsafe {
        const RTLD_LAZY: c_int = 1;
        const RTLD_GLOBAL: c_int = 256;
        extern "C" {
            fn dlopen(filename: *const i8, flag: c_int) -> *mut c_void;
            fn dlsym(handle: *mut c_void, symbol: *const i8) -> *mut c_void;
        }
        for soname in [
            b"libopenblas.so.0\0".as_ptr() as *const i8,
            b"libopenblas.so\0".as_ptr() as *const i8,
            b"libopenblas-pthread.so.0\0".as_ptr() as *const i8,
        ] {
            let h = dlopen(soname, RTLD_LAZY | RTLD_GLOBAL);
            if h.is_null() {
                continue;
            }
            let sym = dlsym(h, b"openblas_set_num_threads\0".as_ptr() as *const i8);
            if !sym.is_null() {
                let f: SetThreads = std::mem::transmute(sym);
                f(n as c_int);
                log::info!("cap_openblas_threads: openblas_set_num_threads({n}) called");
                return;
            }
        }
        log::warn!(
            "cap_openblas_threads: libopenblas symbol not found; relying on env-var fallback"
        );
    }
}

#[cfg(not(target_os = "linux"))]
pub fn cap_openblas_threads(_n: i32) {}

/// Live [`FinalizeCap`] guards.
struct Caps {
    held: usize,
}

impl Caps {
    /// A guard is taken; returns the cap to apply, if it changes.
    fn take(&mut self, budget: i32) -> Option<i32> {
        self.held += 1;
        (self.held == 1).then_some(budget)
    }

    /// A guard is dropped; returns the cap to apply, if it changes.
    fn release(&mut self) -> Option<i32> {
        self.held -= 1;
        (self.held == 0).then_some(1)
    }
}

static FINALIZE_CAPS: std::sync::Mutex<Caps> = std::sync::Mutex::new(Caps { held: 0 });

/// Holds the OpenBLAS thread cap at the finalize budget while any guard lives.
pub struct FinalizeCap;

pub fn finalize_cap() -> FinalizeCap {
    let mut caps = FINALIZE_CAPS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(n) = caps.take(transcript::cpu::finalize_cpu_budget() as i32) {
        transcript::cpu::run_at_normal_priority(move || cap_openblas_threads(n));
    }
    FinalizeCap
}

impl Drop for FinalizeCap {
    fn drop(&mut self) {
        let mut caps = FINALIZE_CAPS.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(n) = caps.release() {
            transcript::cpu::run_at_normal_priority(move || cap_openblas_threads(n));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_and_overlapping_guards_restore_the_previous_cap() {
        let mut caps = Caps { held: 0 };
        assert_eq!(caps.take(6), Some(6), "the first guard raises the cap");
        assert_eq!(caps.take(6), None, "a nested guard keeps it");
        assert_eq!(caps.release(), None, "the nested drop leaves the outer guard's cap");
        // Overlapping lifetimes: the first-created guard drops while a later one lives.
        assert_eq!(caps.take(6), None);
        assert_eq!(caps.release(), None, "a live guard keeps its cap after another drops");
        assert_eq!(caps.release(), Some(1), "the last drop restores the recording-time cap");
    }
}
