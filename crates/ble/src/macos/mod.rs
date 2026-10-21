// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! The Rust half of the CoreBluetooth shim. Every call crosses into
//! `shim.swift`, which owns the `CBPeripheralManager` and its queue; challenge
//! writes and address reads cross back through [`on_challenge`] and
//! [`on_address`] on that queue.
use std::collections::HashMap;
use std::ffi::{c_char, c_void, CStr, CString};
use std::sync::{Arc, Mutex};

use crate::{Error, Responder, Written};

/// Answers of [`on_challenge`], mapped to ATT results by the shim.
const WRITE_KEPT: i32 = 0;
const WRITE_WRONG_LENGTH: i32 = 1;
const WRITE_FAILED: i32 = 2;
const WRITE_UNKNOWN_SERVICE: i32 = 3;
const WRITE_LIMITED: i32 = 4;

/// Negative answers of [`on_address`]; zero and above is a length.
const READ_NOT_PERMITTED: i32 = -1;
const READ_UNKNOWN_SERVICE: i32 = -2;
const READ_FAILED: i32 = -3;

type ChallengeFn = extern "C" fn(*mut c_void, *const c_char, *const c_char, *const u8, i32) -> i32;
type AddressFn = extern "C" fn(*mut c_void, *const c_char, *const c_char, *mut u8, i32) -> i32;

extern "C" {
    fn ble_start(on_challenge: ChallengeFn, on_address: AddressFn, ctx: *mut c_void, err: *mut c_char) -> *mut c_void;
    fn ble_add_service(
        handle: *mut c_void,
        service: *const c_char,
        wake: *const c_char,
        address: *const c_char,
        challenge: *const c_char,
        err: *mut c_char,
    ) -> bool;
    fn ble_notify(handle: *mut c_void, service: *const c_char, value: *const u8, len: i32) -> bool;
    fn ble_state(handle: *mut c_void) -> i32;
    fn ble_is_advertising(handle: *mut c_void) -> bool;
    fn ble_subscribers(handle: *mut c_void, service: *const c_char) -> i32;
    fn ble_stop(handle: *mut c_void);
}

/// Each published service's responder, by uppercase service uuid.
#[derive(Default)]
struct Registry(Mutex<HashMap<String, Arc<dyn Responder>>>);

impl Registry {
    fn get(&self, service: &str) -> Option<Arc<dyn Responder>> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).get(service).cloned()
    }
}

/// The Swift peripheral. Stopped and released on drop; the registry outlives
/// the stop, which returns only once no callback is running.
pub struct Handle {
    peripheral: *mut c_void,
    registry: Box<Registry>,
}

// The Swift side serializes every call on its own lock and queue.
unsafe impl Send for Handle {}
unsafe impl Sync for Handle {}

fn err_buf() -> [c_char; 256] {
    [0; 256]
}

fn err_text(buf: &[c_char; 256]) -> String {
    let bytes: Vec<u8> = buf.iter().take_while(|c| **c != 0).map(|c| *c as u8).collect();
    String::from_utf8_lossy(&bytes).to_string()
}

/// A C string from the shim, or `None` for a null or non-UTF-8 one.
///
/// # Safety
/// `p` is null or a nul-terminated string alive for the call.
unsafe fn text<'a>(p: *const c_char) -> Option<&'a str> {
    if p.is_null() {
        return None;
    }
    CStr::from_ptr(p).to_str().ok()
}

/// A central wrote to a challenge characteristic.
extern "C" fn on_challenge(
    ctx: *mut c_void,
    service: *const c_char,
    central: *const c_char,
    value: *const u8,
    len: i32,
) -> i32 {
    let answer = std::panic::catch_unwind(|| {
        // SAFETY: `ctx` is the registry `Handle` owns, alive until `ble_stop`
        // returns; the strings and bytes are the shim's, alive for the call.
        let registry = unsafe { &*(ctx as *const Registry) };
        let (Some(service), Some(central)) = (unsafe { text(service) }, unsafe { text(central) }) else {
            return WRITE_UNKNOWN_SERVICE;
        };
        let Some(responder) = registry.get(service) else { return WRITE_UNKNOWN_SERVICE };
        let value: &[u8] = match usize::try_from(len) {
            Ok(n) if n > 0 && !value.is_null() => unsafe { std::slice::from_raw_parts(value, n) },
            _ => &[],
        };
        match responder.challenge(central, value) {
            Written::Kept => WRITE_KEPT,
            Written::WrongLength => WRITE_WRONG_LENGTH,
            Written::Failed => WRITE_FAILED,
            Written::Limited => WRITE_LIMITED,
        }
    });
    answer.unwrap_or(WRITE_FAILED)
}

/// A central reads an address characteristic: the length copied into `out`,
/// or a negative answer.
extern "C" fn on_address(ctx: *mut c_void, service: *const c_char, central: *const c_char, out: *mut u8, cap: i32) -> i32 {
    let answer = std::panic::catch_unwind(|| {
        // SAFETY: as in `on_challenge`; `out` holds `cap` writable bytes.
        let registry = unsafe { &*(ctx as *const Registry) };
        let (Some(service), Some(central)) = (unsafe { text(service) }, unsafe { text(central) }) else {
            return READ_UNKNOWN_SERVICE;
        };
        let Some(responder) = registry.get(service) else { return READ_UNKNOWN_SERVICE };
        let Some(bytes) = responder.address(central) else { return READ_NOT_PERMITTED };
        let (Ok(cap), Ok(len)) = (usize::try_from(cap), i32::try_from(bytes.len())) else { return READ_FAILED };
        if bytes.len() > cap || out.is_null() {
            return READ_FAILED;
        }
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), out, bytes.len()) };
        len
    });
    answer.unwrap_or(READ_FAILED)
}

impl Handle {
    pub fn start() -> Result<Self, Error> {
        let registry = Box::<Registry>::default();
        let ctx = &*registry as *const Registry as *mut c_void;
        let mut err = err_buf();
        let peripheral = unsafe { ble_start(on_challenge, on_address, ctx, err.as_mut_ptr()) };
        if peripheral.is_null() {
            return Err(Error::Radio(err_text(&err)));
        }
        Ok(Self { peripheral, registry })
    }

    pub fn add_service(
        &self,
        service: &str,
        wake: &str,
        address: &str,
        challenge: &str,
        responder: Arc<dyn Responder>,
    ) -> Result<(), Error> {
        let key = service.to_uppercase();
        let (Ok(service), Ok(wake), Ok(address), Ok(challenge)) =
            (CString::new(key.as_str()), CString::new(wake), CString::new(address), CString::new(challenge))
        else {
            return Err(Error::Radio("uuid contains a nul byte".into()));
        };
        self.registry.0.lock().unwrap_or_else(|e| e.into_inner()).insert(key.clone(), responder);
        let mut err = err_buf();
        let ok = unsafe {
            ble_add_service(
                self.peripheral,
                service.as_ptr(),
                wake.as_ptr(),
                address.as_ptr(),
                challenge.as_ptr(),
                err.as_mut_ptr(),
            )
        };
        if ok {
            Ok(())
        } else {
            self.registry.0.lock().unwrap_or_else(|e| e.into_inner()).remove(&key);
            Err(Error::Radio(err_text(&err)))
        }
    }

    pub fn notify(&self, service: &str, value: &[u8]) -> bool {
        let Ok(service) = CString::new(service) else { return false };
        unsafe { ble_notify(self.peripheral, service.as_ptr(), value.as_ptr(), value.len() as i32) }
    }

    pub fn state(&self) -> i32 {
        unsafe { ble_state(self.peripheral) }
    }

    pub fn is_advertising(&self) -> bool {
        unsafe { ble_is_advertising(self.peripheral) }
    }

    pub fn subscribers(&self, service: &str) -> i32 {
        let Ok(service) = CString::new(service) else { return 0 };
        unsafe { ble_subscribers(self.peripheral, service.as_ptr()) }
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        unsafe { ble_stop(self.peripheral) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Keeps one fixed reply for a 16-byte write, per central.
    #[derive(Default)]
    struct Echo(Mutex<HashMap<String, Vec<u8>>>);

    impl Responder for Echo {
        fn challenge(&self, central: &str, value: &[u8]) -> Written {
            if value.len() != 16 {
                return Written::WrongLength;
            }
            self.0.lock().unwrap().insert(central.into(), value.to_vec());
            Written::Kept
        }
        fn address(&self, central: &str) -> Option<Vec<u8>> {
            self.0.lock().unwrap().get(central).cloned()
        }
    }

    const SERVICE: &str = "D4A87EF9-7B7A-4AD3-806E-000000000001";

    fn registry() -> Box<Registry> {
        let r = Box::<Registry>::default();
        r.0.lock().unwrap().insert(SERVICE.into(), Arc::new(Echo::default()));
        r
    }

    fn c(s: &str) -> CString {
        CString::new(s).unwrap()
    }

    fn write(r: &Registry, service: &str, central: &str, value: &[u8]) -> i32 {
        let ctx = r as *const Registry as *mut c_void;
        on_challenge(ctx, c(service).as_ptr(), c(central).as_ptr(), value.as_ptr(), value.len() as i32)
    }

    fn read(r: &Registry, service: &str, central: &str, cap: usize) -> (i32, Vec<u8>) {
        let ctx = r as *const Registry as *mut c_void;
        let mut out = vec![0u8; cap];
        let n = on_address(ctx, c(service).as_ptr(), c(central).as_ptr(), out.as_mut_ptr(), cap as i32);
        (n, out)
    }

    #[test]
    fn a_read_before_any_challenge_is_not_permitted() {
        let r = registry();
        assert_eq!(read(&r, SERVICE, "central-a", 512).0, READ_NOT_PERMITTED);
    }

    #[test]
    fn a_kept_challenge_is_what_that_central_reads() {
        let r = registry();
        assert_eq!(write(&r, SERVICE, "central-a", &[3u8; 16]), WRITE_KEPT);
        let (n, out) = read(&r, SERVICE, "central-a", 512);
        assert_eq!(n, 16);
        assert_eq!(&out[..16], &[3u8; 16]);
        assert_eq!(read(&r, SERVICE, "central-b", 512).0, READ_NOT_PERMITTED, "another central");
    }

    #[test]
    fn the_responder_decides_the_length() {
        let r = registry();
        assert_eq!(write(&r, SERVICE, "central-a", &[3u8; 15]), WRITE_WRONG_LENGTH);
        assert_eq!(write(&r, SERVICE, "central-a", &[]), WRITE_WRONG_LENGTH);
    }

    #[test]
    fn an_unknown_service_is_answered_as_one() {
        let r = registry();
        let other = "D4A87EF9-7B7A-4AD3-806E-000000000002";
        assert_eq!(write(&r, other, "central-a", &[3u8; 16]), WRITE_UNKNOWN_SERVICE);
        assert_eq!(read(&r, other, "central-a", 512).0, READ_UNKNOWN_SERVICE);
    }

    #[test]
    fn a_reply_larger_than_the_buffer_fails() {
        let r = registry();
        write(&r, SERVICE, "central-a", &[3u8; 16]);
        assert_eq!(read(&r, SERVICE, "central-a", 8).0, READ_FAILED);
    }
}
