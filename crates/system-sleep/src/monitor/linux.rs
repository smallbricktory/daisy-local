// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Linux: logind `PrepareForSleep(bool)` on the system bus, plus a `sleep`
//! delay inhibitor lock held while `delay_wanted` returns true. A signal is
//! acted on only when its sender is the current owner of
//! `org.freedesktop.login1`.

use super::{DelayRetry, DelayWanted, PowerEvent, PowerHandler};
use dbus::blocking::Connection;
use dbus::message::MatchRule;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

const LOGIND: &str = "org.freedesktop.login1";
const LOGIND_PATH: &str = "/org/freedesktop/login1";
const LOGIND_MANAGER: &str = "org.freedesktop.login1.Manager";
const CALL_TIMEOUT: Duration = Duration::from_secs(5);
const POLL: Duration = Duration::from_millis(500);

pub struct Monitor {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Monitor {
    pub fn start(handler: PowerHandler, delay_wanted: DelayWanted) -> Option<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let (ready_tx, ready_rx) = mpsc::channel::<bool>();
        let thread_stop = Arc::clone(&stop);
        let thread = std::thread::Builder::new()
            .name("daisy-power-monitor".into())
            .spawn(move || {
                let session = match Session::connect() {
                    Ok(s) => s,
                    Err(e) => {
                        log::warn!("power monitor: logind unavailable on the system bus: {e}");
                        let _ = ready_tx.send(false);
                        return;
                    }
                };
                let _ = ready_tx.send(true);
                session.run(&handler, &delay_wanted, &thread_stop);
            })
            .map_err(|e| log::warn!("power monitor: thread spawn failed: {e}"))
            .ok()?;
        if ready_rx.recv().unwrap_or(false) {
            log::info!("power monitor: watching suspend/resume (logind PrepareForSleep)");
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

struct Session {
    conn: Connection,
    /// `PrepareForSleep` arguments with the sender's unique bus name, queued
    /// by the match callback.
    queued: Arc<Mutex<Vec<(bool, String)>>>,
    logind_owner: String,
}

impl Session {
    fn connect() -> Result<Self, dbus::Error> {
        let conn = Connection::new_system()?;
        let logind_owner = name_owner(&conn)?;
        let queued: Arc<Mutex<Vec<(bool, String)>>> = Arc::default();
        let sink = Arc::clone(&queued);
        let rule = MatchRule::new_signal(LOGIND_MANAGER, "PrepareForSleep")
            .with_sender(LOGIND)
            .with_path(LOGIND_PATH);
        conn.add_match(rule, move |(start,): (bool,), _, msg| {
            let sender = msg.sender().map(|s| s.to_string()).unwrap_or_default();
            if let Ok(mut q) = sink.lock() {
                q.push((start, sender));
            }
            true
        })?;
        Ok(Self { conn, queued, logind_owner })
    }

    fn run(mut self, handler: &PowerHandler, delay_wanted: &DelayWanted, stop: &AtomicBool) {
        let mut lock: Option<dbus::arg::OwnedFd> = None;
        let mut retry = DelayRetry::default();
        let mut sleeping = false;
        while !stop.load(Ordering::Relaxed) {
            if let Err(e) = self.conn.process(POLL) {
                log::warn!("power monitor: system bus connection lost: {e}");
                break;
            }
            let events = self.queued.lock().map(|mut q| std::mem::take(&mut *q)).unwrap_or_default();
            for (start, sender) in events {
                if !self.sent_by_logind(&sender) {
                    log::warn!("power monitor: ignored PrepareForSleep from {sender}, not the logind owner");
                    continue;
                }
                if start {
                    handler(PowerEvent::Suspend);
                    sleeping = true;
                    if lock.take().is_some() {
                        log::info!("power monitor: released logind sleep delay lock");
                    }
                } else {
                    sleeping = false;
                    handler(PowerEvent::Resume);
                }
            }
            let want = !sleeping && delay_wanted();
            if retry.should_request(want, lock.is_some()) {
                match take_delay_lock(&self.conn) {
                    Ok(fd) => {
                        log::info!("power monitor: holding logind sleep delay lock");
                        lock = Some(fd);
                    }
                    Err(e) => {
                        retry.refused();
                        log::warn!(
                            "power monitor: logind delay lock refused: {e}; asking again at the next recording or wake"
                        );
                    }
                }
            } else if !want && lock.take().is_some() {
                log::info!("power monitor: released logind sleep delay lock");
            }
        }
    }

    /// True when `sender` owns `org.freedesktop.login1`, re-resolving the
    /// owner once on a mismatch (logind restarted).
    fn sent_by_logind(&mut self, sender: &str) -> bool {
        if sender == self.logind_owner {
            return true;
        }
        match name_owner(&self.conn) {
            Ok(owner) => {
                self.logind_owner = owner;
                sender == self.logind_owner
            }
            Err(_) => false,
        }
    }
}

fn name_owner(conn: &Connection) -> Result<String, dbus::Error> {
    let proxy = conn.with_proxy("org.freedesktop.DBus", "/org/freedesktop/DBus", CALL_TIMEOUT);
    let (owner,): (String,) = proxy.method_call("org.freedesktop.DBus", "GetNameOwner", (LOGIND,))?;
    Ok(owner)
}

fn take_delay_lock(conn: &Connection) -> Result<dbus::arg::OwnedFd, dbus::Error> {
    let proxy = conn.with_proxy(LOGIND, LOGIND_PATH, CALL_TIMEOUT);
    let (fd,): (dbus::arg::OwnedFd,) = proxy.method_call(
        LOGIND_MANAGER,
        "Inhibit",
        ("sleep", "Daisy", "Close the recording segment before sleep", "delay"),
    )?;
    Ok(fd)
}
