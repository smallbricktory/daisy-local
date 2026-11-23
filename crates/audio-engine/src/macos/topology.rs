// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! macOS topology watcher: CoreAudio property listeners for device-list and
//! default-device changes.

use super::source::{
    fourcc, AudioObjectGetPropertyData, AudioObjectPropertyAddress, K_AUDIO_OBJECT_SYSTEM_OBJECT,
};
use crate::routing::OutputClass;
use crate::source::SourceKind;
use crate::topology::{DeviceKey, TopologyEvent};
use std::os::raw::c_void;
use std::sync::mpsc::Sender;
use std::sync::{Mutex, OnceLock};

/// Diff two device-key lists into (added, removed).
pub(crate) fn diff_devices(
    old: &[DeviceKey],
    new: &[DeviceKey],
) -> (Vec<DeviceKey>, Vec<DeviceKey>) {
    let added = new.iter().filter(|k| !old.contains(k)).cloned().collect();
    let removed = old.iter().filter(|k| !new.contains(k)).cloned().collect();
    (added, removed)
}

type ListenerProc = extern "C" fn(
    id: u32,
    n_addresses: u32,
    addresses: *const AudioObjectPropertyAddress,
    ctx: *mut c_void,
) -> i32;

#[link(name = "CoreAudio", kind = "framework")]
extern "C" {
    fn AudioObjectAddPropertyListener(
        id: u32,
        addr: *const AudioObjectPropertyAddress,
        proc_: ListenerProc,
        ctx: *mut c_void,
    ) -> i32;
}

/// The three watched selectors: device list, default input, default output.
const WATCHED: [&[u8; 4]; 3] = [b"dev#", b"dIn ", b"dOut"];

/// Watcher state. The CoreAudio listeners are registered once for the
/// process (on the first subscriber) and stay registered; a refresh with no
/// subscribers just updates the cached device world. Concurrent watchers
/// (the app-lifetime UI watcher plus a per-recording supervisor watcher)
/// each receive every event and drop independently.
struct WatchState {
    subs: Vec<(u64, Sender<TopologyEvent>)>,
    next_token: u64,
    registered: bool,
    devices: Vec<DeviceKey>,
    default_input: Option<DeviceKey>,
    default_output: Option<DeviceKey>,
    /// Input-device ids carrying volume/mute listeners. Registered once per
    /// device while it is present; a vanished device's listener is inert
    /// (never unregistered), but its id is pruned from this set so an
    /// OS-recycled id re-registers.
    volume_watched: std::collections::HashSet<u32>,
    /// Last (volume, muted) emitted per device id. Listeners sit on the
    /// master and both channel elements, so one change fires `on_volume`
    /// 2–3×; only actual state changes are fanned out.
    volume_emitted: std::collections::HashMap<u32, (f32, bool)>,
}

/// Shared with the C callbacks; a single refresh pass serializes access.
static STATE: OnceLock<Mutex<WatchState>> = OnceLock::new();

fn state_cell() -> &'static Mutex<WatchState> {
    STATE.get_or_init(|| {
        Mutex::new(WatchState {
            subs: Vec::new(),
            next_token: 0,
            registered: false,
            devices: Vec::new(),
            default_input: None,
            default_output: None,
            volume_watched: std::collections::HashSet::new(),
            volume_emitted: std::collections::HashMap::new(),
        })
    })
}

/// Current input devices as `(CoreAudio id, key)` pairs.
fn current_mic_ids() -> Vec<(u32, DeviceKey)> {
    super::source::list_sources_blocking()
        .map(|v| {
            v.into_iter()
                .filter(|s| s.kind == SourceKind::Mic)
                .map(|s| (s.id, s.key))
                .collect()
        })
        .unwrap_or_default()
}

/// Watched elements: master plus the first two channels — input devices
/// commonly expose volume/mute per channel, not on the master element.
const VOLUME_ELEMENTS: [u32; 3] = [0, 1, 2];

/// Read a plain scalar property, trying the master element then the first
/// two channel elements; the first readable element wins.
fn read_prop<T: Copy + Default>(id: u32, selector: &[u8; 4], scope: &[u8; 4]) -> Option<T> {
    for element in VOLUME_ELEMENTS {
        let addr = AudioObjectPropertyAddress {
            selector: fourcc(selector),
            scope: fourcc(scope),
            element,
        };
        let mut v = T::default();
        let mut size = std::mem::size_of::<T>() as u32;
        let rc = unsafe {
            AudioObjectGetPropertyData(id, &addr, 0, std::ptr::null(), &mut size, &mut v as *mut T as *mut c_void)
        };
        if rc == 0 {
            return Some(v);
        }
    }
    None
}

/// Volume/mute listener on one input device: query the new state and fan a
/// VolumeChanged out to every subscriber. When NEITHER property is readable
/// nothing is emitted — a fabricated "unmuted, full volume" would defeat
/// the external-mute policy.
extern "C" fn on_volume(
    id: u32,
    _n: u32,
    _addresses: *const AudioObjectPropertyAddress,
    _ctx: *mut c_void,
) -> i32 {
    let Some(key) = super::source::device_key_of(id) else { return 0 };
    let volume = read_prop::<f32>(id, b"volm", b"inpt");
    let muted = read_prop::<u32>(id, b"mute", b"inpt").map(|v| v != 0);
    if volume.is_none() && muted.is_none() {
        return 0;
    }
    let volume = volume.unwrap_or(1.0);
    let muted = muted.unwrap_or(false);
    if let Ok(mut st) = state_cell().lock() {
        if st.volume_emitted.insert(id, (volume, muted)) == Some((volume, muted)) {
            return 0;
        }
        for (_, tx) in &st.subs {
            let _ = tx.send(TopologyEvent::VolumeChanged { key: key.clone(), volume, muted });
        }
    }
    0
}

/// Add volume/mute listeners for any input device not yet watched, on the
/// master and per-channel elements. Registrations are best-effort — devices
/// without a settable volume simply fail and are recorded as watched.
/// Departed ids are pruned from the watched set first, so an id the OS
/// recycles for a new device registers again.
fn register_volume_listeners(st: &mut WatchState) {
    let current = current_mic_ids();
    st.volume_watched
        .retain(|id| current.iter().any(|(cur, _)| cur == id));
    st.volume_emitted
        .retain(|id, _| current.iter().any(|(cur, _)| cur == id));
    for (id, _key) in current {
        if !st.volume_watched.insert(id) {
            continue;
        }
        for sel in [b"volm", b"mute"] {
            for element in VOLUME_ELEMENTS {
                let addr = AudioObjectPropertyAddress {
                    selector: fourcc(sel),
                    scope: fourcc(b"inpt"),
                    element,
                };
                unsafe {
                    let _ =
                        AudioObjectAddPropertyListener(id, &addr, on_volume, std::ptr::null_mut());
                }
            }
        }
    }
}

fn default_device_key(selector: &[u8; 4]) -> Option<DeviceKey> {
    let addr = AudioObjectPropertyAddress {
        selector: fourcc(selector),
        scope: fourcc(b"glob"),
        element: 0,
    };
    let mut id: u32 = 0;
    let mut size = std::mem::size_of::<u32>() as u32;
    let rc = unsafe {
        AudioObjectGetPropertyData(
            K_AUDIO_OBJECT_SYSTEM_OBJECT,
            &addr,
            0,
            std::ptr::null(),
            &mut size,
            &mut id as *mut u32 as *mut c_void,
        )
    };
    if rc != 0 || id == 0 {
        return None;
    }
    super::source::device_key_of(id)
}

fn current_mic_keys() -> Vec<DeviceKey> {
    super::source::list_sources_blocking()
        .map(|v| {
            v.into_iter()
                .filter(|s| s.kind == SourceKind::Mic)
                .map(|s| s.key)
                .collect()
        })
        .unwrap_or_default()
}

/// Re-reads the sections whose selectors fired, diffs against the cached
/// state, fans the resulting events out to every subscriber. BT
/// renegotiation fires default-device selectors in bursts of 5+; branching
/// spares those bursts the full per-device property walk.
fn refresh(devices: bool, din_sel: bool, dout_sel: bool) {
    let mut st = match state_cell().lock() {
        Ok(g) => g,
        Err(_) => return,
    };

    let mut events: Vec<TopologyEvent> = Vec::new();
    if devices {
        let new_devices = current_mic_keys();
        let (added, removed) = diff_devices(&st.devices, &new_devices);
        for key in added {
            events.push(TopologyEvent::DeviceAdded {
                key,
                kind: SourceKind::Mic,
            });
        }
        for key in removed {
            events.push(TopologyEvent::DeviceRemoved { key });
        }
        st.devices = new_devices;
        register_volume_listeners(&mut st);
    }

    if din_sel {
        let din = default_device_key(b"dIn ");
        if din != st.default_input {
            st.default_input = din.clone();
            events.push(TopologyEvent::DefaultInputChanged { key: din });
        }
    }

    if dout_sel {
        let dout = default_device_key(b"dOut");
        if dout != st.default_output {
            st.default_output = dout.clone();
            let class = match super::default_output_class() {
                c if c >= 0 => crate::routing::classify_macos_transport(c),
                _ => OutputClass::Unknown,
            };
            events.push(TopologyEvent::DefaultOutputChanged { key: dout, class });
        }
    }

    for ev in events {
        for (_, tx) in &st.subs {
            let _ = tx.send(ev.clone());
        }
    }
}

extern "C" fn on_property(
    _id: u32,
    n: u32,
    addresses: *const AudioObjectPropertyAddress,
    _ctx: *mut c_void,
) -> i32 {
    let (mut devices, mut din, mut dout) = (false, false, false);
    if addresses.is_null() || n == 0 {
        // No address info: refresh everything (never miss a change).
        (devices, din, dout) = (true, true, true);
    } else {
        for i in 0..n as usize {
            let sel = unsafe { (*addresses.add(i)).selector };
            if sel == fourcc(b"dev#") {
                devices = true;
            } else if sel == fourcc(b"dIn ") {
                din = true;
            } else if sel == fourcc(b"dOut") {
                dout = true;
            } else {
                (devices, din, dout) = (true, true, true);
            }
        }
    }
    refresh(devices, din, dout);
    0
}

pub(crate) struct WatcherHandle {
    token: u64,
}

impl Drop for WatcherHandle {
    fn drop(&mut self) {
        if let Ok(mut st) = state_cell().lock() {
            st.subs.retain(|(t, _)| *t != self.token);
        }
    }
}

pub(crate) fn spawn_watcher(tx: Sender<TopologyEvent>) -> WatcherHandle {
    let mut st = state_cell().lock().expect("topology state lock");
    if !st.registered {
        st.devices = current_mic_keys();
        st.default_input = default_device_key(b"dIn ");
        st.default_output = default_device_key(b"dOut");
        register_volume_listeners(&mut st);
        for sel in WATCHED {
            let addr = AudioObjectPropertyAddress {
                selector: fourcc(sel),
                scope: fourcc(b"glob"),
                element: 0,
            };
            let rc = unsafe {
                AudioObjectAddPropertyListener(
                    K_AUDIO_OBJECT_SYSTEM_OBJECT,
                    &addr,
                    on_property,
                    std::ptr::null_mut(),
                )
            };
            if rc != 0 {
                log::warn!(
                    "topology watcher: add listener {:?} failed (rc={rc})",
                    std::str::from_utf8(sel).unwrap_or("?")
                );
            }
        }
        st.registered = true;
    }
    let token = st.next_token;
    st.next_token += 1;
    st.subs.push((token, tx));
    WatcherHandle { token }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_list_diff_emits_adds_and_removes() {
        let old = vec![DeviceKey("uid-mac-mic".into())];
        let new = vec![
            DeviceKey("uid-mac-mic".into()),
            DeviceKey("uid-airpods".into()),
        ];
        let (added, removed) = diff_devices(&old, &new);
        assert_eq!(added, vec![DeviceKey("uid-airpods".into())]);
        assert!(removed.is_empty());
        let (added, removed) = diff_devices(&new, &old);
        assert!(added.is_empty());
        assert_eq!(removed, vec![DeviceKey("uid-airpods".into())]);
    }

    #[test]
    fn identical_lists_diff_to_nothing() {
        let a = vec![DeviceKey("x".into()), DeviceKey("y".into())];
        let (added, removed) = diff_devices(&a, &a.clone());
        assert!(added.is_empty() && removed.is_empty());
    }
}

/// Default input device's stable key (its CoreAudio UID).
pub(crate) fn default_input_key() -> Option<DeviceKey> {
    default_device_key(b"dIn ")
}
