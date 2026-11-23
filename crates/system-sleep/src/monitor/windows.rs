// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Windows: `PowerRegisterSuspendResumeNotification` callback registration.
//! The callback reads the handler from a process-wide slot; one monitor runs
//! per process, and a callback arriving after unregistration finds the slot
//! empty.

use super::{PowerEvent, PowerHandler};
use std::sync::Mutex;
use windows::Win32::Foundation::{ERROR_SUCCESS, HANDLE};
use windows::Win32::System::Power::{
    PowerRegisterSuspendResumeNotification, PowerUnregisterSuspendResumeNotification,
    DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS, HPOWERNOTIFY,
};
use windows::Win32::UI::WindowsAndMessaging::{
    DEVICE_NOTIFY_CALLBACK, PBT_APMRESUMEAUTOMATIC, PBT_APMSUSPEND,
};

static HANDLER: Mutex<Option<PowerHandler>> = Mutex::new(None);

unsafe extern "system" fn on_power_event(
    _context: *const core::ffi::c_void,
    event_type: u32,
    _setting: *const core::ffi::c_void,
) -> u32 {
    let event = match event_type {
        PBT_APMSUSPEND => PowerEvent::Suspend,
        PBT_APMRESUMEAUTOMATIC => PowerEvent::Resume,
        _ => return ERROR_SUCCESS.0,
    };
    let handler = HANDLER.lock().ok().and_then(|slot| slot.clone());
    if let Some(handler) = handler {
        handler(event);
    }
    ERROR_SUCCESS.0
}

/// The subscription parameters, kept alive for the registration's lifetime.
struct Subscription(Box<DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS>);

// SAFETY: the parameters are written once before registration and never
// read or written by this process afterwards.
unsafe impl Send for Subscription {}
unsafe impl Sync for Subscription {}

pub struct Monitor {
    registration: HPOWERNOTIFY,
    subscription: Option<Subscription>,
}

impl Monitor {
    pub fn start(handler: PowerHandler) -> Option<Self> {
        {
            let mut slot = HANDLER.lock().ok()?;
            if slot.is_some() {
                log::warn!("power monitor: already running in this process");
                return None;
            }
            *slot = Some(handler);
        }
        let subscription = Subscription(Box::new(DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS {
            Callback: Some(on_power_event),
            Context: std::ptr::null_mut(),
        }));
        let recipient = HANDLE(&*subscription.0 as *const DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS as *mut _);
        let mut registration: *mut core::ffi::c_void = std::ptr::null_mut();
        let rc = unsafe {
            PowerRegisterSuspendResumeNotification(DEVICE_NOTIFY_CALLBACK, recipient, &mut registration)
        };
        if rc != ERROR_SUCCESS {
            log::warn!("power monitor: PowerRegisterSuspendResumeNotification failed ({rc:?})");
            if let Ok(mut slot) = HANDLER.lock() {
                *slot = None;
            }
            return None;
        }
        log::info!("power monitor: watching suspend/resume (PowerRegisterSuspendResumeNotification)");
        Some(Self { registration: HPOWERNOTIFY(registration as isize), subscription: Some(subscription) })
    }
}

impl Drop for Monitor {
    fn drop(&mut self) {
        let rc = unsafe { PowerUnregisterSuspendResumeNotification(self.registration) };
        if rc != ERROR_SUCCESS {
            log::warn!("power monitor: PowerUnregisterSuspendResumeNotification failed ({rc:?})");
            std::mem::forget(self.subscription.take());
        }
        if let Ok(mut slot) = HANDLER.lock() {
            *slot = None;
        }
    }
}
