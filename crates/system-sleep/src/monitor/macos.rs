// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! macOS: `IORegisterForSystemPower` serviced by a dedicated CFRunLoop
//! thread. Every callback, registration and teardown happens on that thread.

use super::{PowerEvent, PowerHandler};
use std::os::raw::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::JoinHandle;

type IoConnect = u32;
type IoObject = u32;
type IoNotificationPortRef = *mut c_void;
type CfRunLoopRef = *mut c_void;
type CfRunLoopSourceRef = *mut c_void;
type CfStringRef = *const c_void;

type IoServiceInterestCallback =
    extern "C" fn(refcon: *mut c_void, service: IoObject, message_type: u32, argument: *mut c_void);

#[link(name = "IOKit", kind = "framework")]
extern "C" {
    fn IORegisterForSystemPower(
        refcon: *mut c_void,
        the_port_ref: *mut IoNotificationPortRef,
        callback: IoServiceInterestCallback,
        notifier: *mut IoObject,
    ) -> IoConnect;
    fn IODeregisterForSystemPower(notifier: *mut IoObject) -> i32;
    fn IOAllowPowerChange(kernel_port: IoConnect, notification_id: isize) -> i32;
    fn IONotificationPortGetRunLoopSource(notify: IoNotificationPortRef) -> CfRunLoopSourceRef;
    fn IONotificationPortDestroy(notify: IoNotificationPortRef);
    fn IOServiceClose(connect: IoConnect) -> i32;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    static kCFRunLoopDefaultMode: CfStringRef;
    fn CFRunLoopGetCurrent() -> CfRunLoopRef;
    fn CFRunLoopAddSource(rl: CfRunLoopRef, source: CfRunLoopSourceRef, mode: CfStringRef);
    fn CFRunLoopRemoveSource(rl: CfRunLoopRef, source: CfRunLoopSourceRef, mode: CfStringRef);
    fn CFRunLoopRunInMode(mode: CfStringRef, seconds: f64, return_after_source_handled: u8) -> i32;
}

/// `iokit_common_msg(x)` from IOMessage.h: `sys_iokit | sub_iokit_common | x`.
const fn iokit_common_msg(message: u32) -> u32 {
    0xE000_0000 | message
}
const K_IO_MESSAGE_CAN_SYSTEM_SLEEP: u32 = iokit_common_msg(0x270);
const K_IO_MESSAGE_SYSTEM_WILL_SLEEP: u32 = iokit_common_msg(0x280);
const K_IO_MESSAGE_SYSTEM_HAS_POWERED_ON: u32 = iokit_common_msg(0x300);

/// Callback state, owned by the run-loop thread for the registration's
/// lifetime.
struct Context {
    handler: PowerHandler,
    root_port: IoConnect,
}

extern "C" fn on_system_power(refcon: *mut c_void, _service: IoObject, message_type: u32, argument: *mut c_void) {
    // SAFETY: refcon is the run-loop thread's `Context`, which outlives the
    // registration; callbacks run on that thread only.
    let ctx = unsafe { &*(refcon as *const Context) };
    match message_type {
        K_IO_MESSAGE_CAN_SYSTEM_SLEEP => unsafe {
            IOAllowPowerChange(ctx.root_port, argument as isize);
        },
        K_IO_MESSAGE_SYSTEM_WILL_SLEEP => {
            (ctx.handler)(PowerEvent::Suspend);
            unsafe {
                IOAllowPowerChange(ctx.root_port, argument as isize);
            }
        }
        K_IO_MESSAGE_SYSTEM_HAS_POWERED_ON => (ctx.handler)(PowerEvent::Resume),
        _ => {}
    }
}

pub struct Monitor {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Monitor {
    pub fn start(handler: PowerHandler) -> Option<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let (ready_tx, ready_rx) = mpsc::channel::<bool>();
        let thread_stop = Arc::clone(&stop);
        let thread = std::thread::Builder::new()
            .name("daisy-power-monitor".into())
            .spawn(move || run(handler, thread_stop, ready_tx))
            .map_err(|e| log::warn!("power monitor: thread spawn failed: {e}"))
            .ok()?;
        if ready_rx.recv().unwrap_or(false) {
            log::info!("power monitor: watching suspend/resume (IORegisterForSystemPower)");
            Some(Self { stop, thread: Some(thread) })
        } else {
            let _ = thread.join();
            None
        }
    }
}

impl Drop for Monitor {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn run(handler: PowerHandler, stop: Arc<AtomicBool>, ready: mpsc::Sender<bool>) {
    let ctx = Box::new(Context { handler, root_port: 0 });
    let ctx_ptr = Box::into_raw(ctx);
    let mut port: IoNotificationPortRef = std::ptr::null_mut();
    let mut notifier: IoObject = 0;
    // SAFETY: ctx_ptr stays valid until after IODeregisterForSystemPower
    // below; the source is serviced only by this thread's run loop.
    unsafe {
        let root_port = IORegisterForSystemPower(ctx_ptr as *mut c_void, &mut port, on_system_power, &mut notifier);
        if root_port == 0 {
            log::warn!("power monitor: IORegisterForSystemPower failed");
            drop(Box::from_raw(ctx_ptr));
            let _ = ready.send(false);
            return;
        }
        (*ctx_ptr).root_port = root_port;
        let run_loop = CFRunLoopGetCurrent();
        let source = IONotificationPortGetRunLoopSource(port);
        CFRunLoopAddSource(run_loop, source, kCFRunLoopDefaultMode);
        let _ = ready.send(true);
        while !stop.load(Ordering::Relaxed) {
            CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.5, 0);
        }
        CFRunLoopRemoveSource(run_loop, source, kCFRunLoopDefaultMode);
        IODeregisterForSystemPower(&mut notifier);
        IOServiceClose(root_port);
        IONotificationPortDestroy(port);
        drop(Box::from_raw(ctx_ptr));
    }
}
