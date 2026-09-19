// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! The Rust half of the CoreBluetooth shim. Every call crosses into
//! `shim.swift`, which owns the `CBPeripheralManager` and its queue.
use std::ffi::{c_char, c_void, CString};

use crate::Error;

extern "C" {
    fn ble_start(err: *mut c_char) -> *mut c_void;
    fn ble_add_service(
        handle: *mut c_void,
        service: *const c_char,
        wake: *const c_char,
        address: *const c_char,
        value: *const u8,
        len: i32,
        err: *mut c_char,
    ) -> bool;
    fn ble_set_address(handle: *mut c_void, service: *const c_char, value: *const u8, len: i32) -> bool;
    fn ble_notify(handle: *mut c_void, service: *const c_char, value: *const u8, len: i32) -> bool;
    fn ble_state(handle: *mut c_void) -> i32;
    fn ble_is_advertising(handle: *mut c_void) -> bool;
    fn ble_subscribers(handle: *mut c_void, service: *const c_char) -> i32;
    fn ble_stop(handle: *mut c_void);
}

/// The Swift peripheral. Stopped and released on drop.
pub struct Handle(*mut c_void);

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

impl Handle {
    pub fn start() -> Result<Self, Error> {
        let mut err = err_buf();
        let handle = unsafe { ble_start(err.as_mut_ptr()) };
        if handle.is_null() {
            return Err(Error::Radio(err_text(&err)));
        }
        Ok(Self(handle))
    }

    pub fn add_service(&self, service: &str, wake: &str, address: &str, value: &[u8]) -> Result<(), Error> {
        let (Ok(service), Ok(wake), Ok(address)) = (CString::new(service), CString::new(wake), CString::new(address))
        else {
            return Err(Error::Radio("uuid contains a nul byte".into()));
        };
        let mut err = err_buf();
        let ok = unsafe {
            ble_add_service(
                self.0,
                service.as_ptr(),
                wake.as_ptr(),
                address.as_ptr(),
                value.as_ptr(),
                value.len() as i32,
                err.as_mut_ptr(),
            )
        };
        if ok {
            Ok(())
        } else {
            Err(Error::Radio(err_text(&err)))
        }
    }

    pub fn set_address(&self, service: &str, value: &[u8]) -> bool {
        let Ok(service) = CString::new(service) else { return false };
        unsafe { ble_set_address(self.0, service.as_ptr(), value.as_ptr(), value.len() as i32) }
    }

    pub fn notify(&self, service: &str, value: &[u8]) -> bool {
        let Ok(service) = CString::new(service) else { return false };
        unsafe { ble_notify(self.0, service.as_ptr(), value.as_ptr(), value.len() as i32) }
    }

    pub fn state(&self) -> i32 {
        unsafe { ble_state(self.0) }
    }

    pub fn is_advertising(&self) -> bool {
        unsafe { ble_is_advertising(self.0) }
    }

    pub fn subscribers(&self, service: &str) -> i32 {
        let Ok(service) = CString::new(service) else { return 0 };
        unsafe { ble_subscribers(self.0, service.as_ptr()) }
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        unsafe { ble_stop(self.0) };
    }
}
