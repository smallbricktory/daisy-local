// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Linux topology watcher: a persistent PipeWire thread that turns registry,
//! metadata, and device changes into [`TopologyEvent`]s.

use crate::routing::OutputClass;
use crate::source::SourceKind;
use crate::topology::{DeviceKey, TopologyEvent};
use pipewire as pw;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::mpsc::Sender;

/// What a registry global maps to, by `media.class`.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum GlobalKind {
    Mic,
    Monitor,
    PlaybackStream,
    CaptureStream,
}

pub(crate) fn classify_global(media_class: &str) -> Option<GlobalKind> {
    match media_class {
        "Audio/Source" => Some(GlobalKind::Mic),
        "Audio/Sink" => Some(GlobalKind::Monitor),
        "Stream/Output/Audio" => Some(GlobalKind::PlaybackStream),
        "Stream/Input/Audio" => Some(GlobalKind::CaptureStream),
        _ => None,
    }
}

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

/// Capture streams seen by the watcher, by registry global id.
static MIC_CLIENTS: Mutex<Option<HashMap<u32, crate::mic_clients::MicClient>>> = Mutex::new(None);
/// Set once the watcher has a PipeWire registry.
static PIPEWIRE_UP: AtomicBool = AtomicBool::new(false);

pub(crate) fn pipewire_up() -> bool {
    PIPEWIRE_UP.load(Ordering::Relaxed)
}

/// Capture streams currently registered, our own excluded.
pub(crate) fn mic_clients() -> Vec<crate::mic_clients::MicClient> {
    let guard = MIC_CLIENTS.lock().unwrap_or_else(|e| e.into_inner());
    guard
        .as_ref()
        .map(|m| m.values().cloned().collect())
        .unwrap_or_default()
}

/// Records a `Stream/Input/Audio` global as a mic client when its props say
/// it captures a microphone from another process.
fn remember_mic_client(
    global_id: u32,
    capture_sink: Option<&str>,
    process_id: Option<&str>,
    app_name: Option<&str>,
) {
    let own = std::process::id();
    let Some(client) =
        crate::mic_clients::mic_client_from_stream_props(capture_sink, process_id, app_name, own)
    else {
        return;
    };
    let mut guard = MIC_CLIENTS.lock().unwrap_or_else(|e| e.into_inner());
    guard.get_or_insert_with(HashMap::new).insert(global_id, client);
}

fn forget_mic_client(global_id: u32) {
    let mut guard = MIC_CLIENTS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(m) = guard.as_mut() {
        m.remove(&global_id);
    }
}

/// Empties the tracked mic-client set, e.g. when the watcher (re)connects to
/// a PipeWire daemon that will not deliver `global_remove` for what the
/// previous connection saw.
fn clear_mic_clients() {
    let mut guard = MIC_CLIENTS.lock().unwrap_or_else(|e| e.into_inner());
    *guard = Some(HashMap::new());
}

/// Extract the node name from a `default` metadata value
/// (`{"name":"bluez_output.0A_1B_2C_3D_4E_5F.1"}`).
pub(crate) fn parse_default_metadata(value: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(value).ok()?;
    v.get("name")?.as_str().map(|s| s.to_string())
}

/// Name-only output classification for default-sink changes (the event
/// carries no props); the one classifier in `routing` with an empty
/// props blob, so a sink can't classify differently here than at start.
pub(crate) fn classify_sink_name(name: &str) -> OutputClass {
    crate::routing::classify_output(name, "")
}

pub(crate) struct WatcherHandle {
    quit: Option<pw::channel::Sender<()>>,
    join: Option<std::thread::JoinHandle<()>>,
    /// `pactl subscribe` child feeding VolumeChanged; killed on drop.
    volume_child: Option<std::process::Child>,
}

impl Drop for WatcherHandle {
    fn drop(&mut self) {
        if let Some(q) = self.quit.take() {
            let _ = q.send(());
        }
        if let Some(mut c) = self.volume_child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

/// Watch `pactl subscribe` for source change events and emit
/// [`TopologyEvent::VolumeChanged`] for each, with the source's current
/// volume/mute queried per event (external changes are user-interaction
/// rate; the supervisor's SelfWrites window absorbs our own writes echoed
/// back). Returns the child so the caller can kill it on drop; the reader
/// thread exits when the child's stdout closes.
fn spawn_volume_watch(tx: Sender<TopologyEvent>) -> Option<std::process::Child> {
    use std::io::BufRead;
    let mut child = std::process::Command::new("pactl")
        .arg("subscribe")
        // C locale: the subscribe lines are gettext-localized; a German
        // system would otherwise never match the English prefix.
        .env("LC_ALL", "C")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| log::warn!("volume watch: pactl subscribe unavailable: {e}"))
        .ok()?;
    let stdout = child.stdout.take()?;
    std::thread::Builder::new()
        .name("daisy-volume-watch".into())
        .spawn(move || {
            let reader = std::io::BufReader::new(stdout);
            for line in reader.lines() {
                let Ok(line) = line else { break };
                // "Event 'change' on source #57"
                let Some(rest) = line.strip_prefix("Event 'change' on source #") else {
                    continue;
                };
                let Ok(id) = rest.trim().parse::<u32>() else { continue };
                let Some((key, volume, muted)) = query_source_volume(id) else { continue };
                let _ = tx.send(TopologyEvent::VolumeChanged { key, volume, muted });
            }
        })
        .ok();
    Some(child)
}

/// `(key, volume 0..1+, muted)` for a source id via pactl, or None.
fn query_source_volume(id: u32) -> Option<(DeviceKey, f32, bool)> {
    let short = std::process::Command::new("pactl")
        .args(["list", "sources", "short"])
        .env("LC_ALL", "C")
        .output()
        .ok()?;
    let short = String::from_utf8_lossy(&short.stdout);
    let name = short.lines().find_map(|l| {
        let mut it = l.split_whitespace();
        (it.next()? == id.to_string()).then(|| it.next().map(str::to_string))?
    })?;
    let vol_out = std::process::Command::new("pactl")
        .args(["get-source-volume", &id.to_string()])
        .env("LC_ALL", "C")
        .output()
        .ok()?;
    let vol_text = String::from_utf8_lossy(&vol_out.stdout);
    // "Volume: front-left: 39322 /  60% / -13.31 dB, ..."
    let volume = vol_text
        .split('/')
        .find_map(|part| part.trim().strip_suffix('%').and_then(|p| p.trim().parse::<f32>().ok()))
        .map(|pct| pct / 100.0)?;
    let mute_out = std::process::Command::new("pactl")
        .args(["get-source-mute", &id.to_string()])
        .env("LC_ALL", "C")
        .output()
        .ok()?;
    let muted = String::from_utf8_lossy(&mute_out.stdout).contains("yes");
    Some((DeviceKey(name), volume, muted))
}

pub(crate) fn spawn_watcher(tx: Sender<TopologyEvent>) -> WatcherHandle {
    let (quit_tx, quit_rx) = pw::channel::channel::<()>();
    let volume_child = spawn_volume_watch(tx.clone());
    let join = std::thread::Builder::new()
        .name("daisy-topology-watch".into())
        .spawn(move || {
            if let Err(e) = watcher_loop(tx, quit_rx) {
                log::warn!("topology watcher exited: {e}");
            }
        })
        .expect("spawn topology watcher thread");
    WatcherHandle {
        quit: Some(quit_tx),
        join: Some(join),
        volume_child,
    }
}

fn watcher_loop(
    tx: Sender<TopologyEvent>,
    quit_rx: pw::channel::Receiver<()>,
) -> crate::error::Result<()> {
    use crate::error::Error;
    pw::init();

    let mainloop = pw::main_loop::MainLoopRc::new(None)
        .map_err(|e| Error::PipeWire(format!("MainLoop::new: {e}")))?;
    let context = pw::context::ContextRc::new(&mainloop, None)
        .map_err(|e| Error::PipeWire(format!("ContextRc::new: {e}")))?;
    let core = context
        .connect_rc(None)
        .map_err(|e| Error::PipeWire(format!("connect: {e}")))?;
    let registry = core
        .get_registry_rc()
        .map_err(|e| Error::PipeWire(format!("get_registry: {e}")))?;
    clear_mic_clients();
    PIPEWIRE_UP.store(true, Ordering::Relaxed);

    // id → key map so `global_remove` (id only) can name the lost device.
    let known: Rc<RefCell<HashMap<u32, DeviceKey>>> = Rc::new(RefCell::new(HashMap::new()));
    // Bound proxies + listeners stay alive here; dropped on global_remove.
    #[allow(clippy::type_complexity)]
    let bound: Rc<RefCell<HashMap<u32, (Box<dyn std::any::Any>, Box<dyn std::any::Any>)>>> =
        Rc::new(RefCell::new(HashMap::new()));
    // Last-seen BT card profile, per card name, for ProfileChanged detection.
    let profiles: Rc<RefCell<HashMap<String, String>>> = Rc::new(RefCell::new(HashMap::new()));

    let known_add = Rc::clone(&known);
    let bound_add = Rc::clone(&bound);
    let registry_bind = registry.clone();
    let tx_add = tx.clone();
    let tx_remove = tx.clone();
    let known_remove = Rc::clone(&known);
    let bound_remove = Rc::clone(&bound);

    let _reg_listener = registry
        .add_listener_local()
        .global(move |global| {
            let Some(props) = global.props.as_ref() else {
                return;
            };
            match global.type_ {
                pw::types::ObjectType::Node => {
                    let media_class = props.get("media.class").unwrap_or("");
                    let node_name = props.get("node.name").unwrap_or("").to_string();
                    match classify_global(media_class) {
                        Some(GlobalKind::Mic) => {
                            let key = DeviceKey(node_name);
                            known_add.borrow_mut().insert(global.id, key.clone());
                            let _ = tx_add.send(TopologyEvent::DeviceAdded {
                                key,
                                kind: SourceKind::Mic,
                            });
                        }
                        Some(GlobalKind::Monitor) => {
                            let key = DeviceKey::monitor_of(&node_name);
                            known_add.borrow_mut().insert(global.id, key.clone());
                            let _ = tx_add.send(TopologyEvent::DeviceAdded {
                                key,
                                kind: SourceKind::Monitor,
                            });
                        }
                        Some(GlobalKind::PlaybackStream) => {
                            let _ = tx_add
                                .send(TopologyEvent::SinkInputAdded { stream_id: global.id });
                        }
                        Some(GlobalKind::CaptureStream) => {
                            remember_mic_client(
                                global.id,
                                props.get("stream.capture.sink"),
                                props.get("application.process.id"),
                                props.get("application.name"),
                            );
                        }
                        None => {}
                    }
                }
                pw::types::ObjectType::Metadata => {
                    if props.get("metadata.name") != Some("default") {
                        return;
                    }
                    let Ok(metadata) = registry_bind.bind::<pw::metadata::Metadata, _>(global)
                    else {
                        return;
                    };
                    let tx_meta = tx_add.clone();
                    let listener = metadata
                        .add_listener_local()
                        .property(move |_subject, key, _type, value| {
                            let name = value.and_then(parse_default_metadata);
                            match key {
                                Some("default.audio.source") => {
                                    let _ = tx_meta.send(TopologyEvent::DefaultInputChanged {
                                        key: name.map(DeviceKey),
                                    });
                                }
                                Some("default.audio.sink") => {
                                    let class = name
                                        .as_deref()
                                        .map(classify_sink_name)
                                        .unwrap_or(OutputClass::Unknown);
                                    let _ = tx_meta.send(TopologyEvent::DefaultOutputChanged {
                                        key: name.map(DeviceKey),
                                        class,
                                    });
                                }
                                _ => {}
                            }
                            0
                        })
                        .register();
                    bound_add
                        .borrow_mut()
                        .insert(global.id, (Box::new(metadata), Box::new(listener)));
                }
                pw::types::ObjectType::Device => {
                    let name = props.get("device.name").unwrap_or("").to_string();
                    if !crate::bt_profile::is_bluetooth_card(&name) {
                        return;
                    }
                    let Ok(device) = registry_bind.bind::<pw::device::Device, _>(global) else {
                        return;
                    };
                    let tx_dev = tx_add.clone();
                    let profiles_dev = Rc::clone(&profiles);
                    let card = name.clone();
                    let listener = device
                        .add_listener_local()
                        .info(move |info| {
                            let Some(p) = info.props() else { return };
                            let Some(profile) = p.get("device.profile.name") else {
                                return;
                            };
                            let mut seen = profiles_dev.borrow_mut();
                            let prev = seen.insert(card.clone(), profile.to_string());
                            if prev.as_deref().is_some_and(|old| old != profile) {
                                let _ = tx_dev
                                    .send(TopologyEvent::ProfileChanged { card: card.clone() });
                            }
                        })
                        .register();
                    bound_add
                        .borrow_mut()
                        .insert(global.id, (Box::new(device), Box::new(listener)));
                }
                _ => {}
            }
        })
        .global_remove(move |id| {
            forget_mic_client(id);
            bound_remove.borrow_mut().remove(&id);
            if let Some(key) = known_remove.borrow_mut().remove(&id) {
                let _ = tx_remove.send(TopologyEvent::DeviceRemoved { key });
            }
        })
        .register();

    let loop_quit = mainloop.clone();
    let _quit_attached = quit_rx.attach(mainloop.loop_(), move |_| {
        loop_quit.quit();
    });

    mainloop.run();
    PIPEWIRE_UP.store(false, Ordering::Relaxed);
    clear_mic_clients();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_class_maps_to_event_kind() {
        assert_eq!(classify_global("Audio/Source"), Some(GlobalKind::Mic));
        assert_eq!(classify_global("Audio/Sink"), Some(GlobalKind::Monitor));
        assert_eq!(
            classify_global("Stream/Output/Audio"),
            Some(GlobalKind::PlaybackStream)
        );
        assert_eq!(
            classify_global("Stream/Input/Audio"),
            Some(GlobalKind::CaptureStream)
        );
        assert_eq!(classify_global("Video/Source"), None);
        assert_eq!(classify_global(""), None);
    }

    #[test]
    fn default_metadata_value_extracts_node_name() {
        assert_eq!(
            parse_default_metadata(r#"{"name":"bluez_output.0A_1B_2C_3D_4E_5F.1"}"#).as_deref(),
            Some("bluez_output.0A_1B_2C_3D_4E_5F.1")
        );
        assert_eq!(parse_default_metadata("garbage"), None);
        assert_eq!(parse_default_metadata(r#"{"other":1}"#), None);
    }

    #[test]
    fn sink_names_classify_conservatively() {
        assert_eq!(
            classify_sink_name("bluez_output.0A_1B_2C_3D_4E_5F.1"),
            OutputClass::Headphone
        );
        assert_eq!(
            classify_sink_name("alsa_output.pci-0000.analog-stereo.headphones"),
            OutputClass::Headphone
        );
        assert_eq!(
            classify_sink_name("alsa_output.usb-Speaker-00.analog-stereo"),
            OutputClass::Speaker
        );
        assert_eq!(classify_sink_name("alsa_output.pci-0000.hdmi"), OutputClass::Unknown);
    }

    #[test]
    fn the_client_set_tracks_globals_by_id() {
        clear_mic_clients();
        remember_mic_client(101, Some("false"), Some("4242"), Some("Zoom"));
        remember_mic_client(102, Some("true"), Some("4243"), Some("OBS"));
        assert_eq!(
            mic_clients(),
            vec![crate::mic_clients::MicClient { pid: 4242, name: "Zoom".into() }]
        );
        forget_mic_client(101);
        assert!(mic_clients().is_empty());
    }
}

/// Default capture source's stable key, via `pactl get-default-source`.
pub(crate) fn default_input_key() -> Option<DeviceKey> {
    let out = std::process::Command::new("pactl")
        .args(["get-default-source"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if name.is_empty() {
        None
    } else {
        Some(DeviceKey(name))
    }
}
