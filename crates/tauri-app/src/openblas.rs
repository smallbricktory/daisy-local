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
