//! Shared topology types: stable device identity, change events, and the
//! id-resolution snapshot the supervisor maintains.

use crate::routing::OutputClass;
use crate::source::SourceKind;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;

/// Stable device identity that survives renumber and replug.
///
/// Linux: PipeWire `node.name`. macOS: CoreAudio device UID. Windows: the
/// IMMDevice id string. Numeric ids are ephemeral — resolve them through
/// [`Snapshot::id_of`] at the moment of use, never cache them.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DeviceKey(pub String);

impl DeviceKey {
    /// Key of the monitor source that shadows the named sink/render
    /// endpoint: `<name>.monitor`.
    pub fn monitor_of(name: &str) -> DeviceKey {
        DeviceKey(format!("{name}.monitor"))
    }
}

/// Which capture stream an event refers to. Serializes as `"Mic"`/`"System"`
/// (part of the audio-warning event payload).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
pub enum Track {
    Mic,
    System,
}

/// Why a capture stream stopped delivering.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StreamDeathReason {
    /// The stream's target node/device no longer exists.
    TargetGone,
    /// The stream reported an error state.
    Error(String),
    /// The stream is nominally alive but has delivered no frames.
    NoFrames,
}

/// A change in the audio world outside our control.
#[derive(Clone, Debug)]
pub enum TopologyEvent {
    DeviceAdded { key: DeviceKey, kind: SourceKind },
    DeviceRemoved { key: DeviceKey },
    DefaultInputChanged { key: Option<DeviceKey> },
    DefaultOutputChanged { key: Option<DeviceKey>, class: OutputClass },
    /// A sound card's active profile changed (BT A2DP↔HFP).
    ProfileChanged { card: String },
    VolumeChanged { key: DeviceKey, volume: f32, muted: bool },
    StreamDied { track: Track, reason: StreamDeathReason },
    SilenceStarted { track: Track },
    SilenceEnded { track: Track },
    /// Frames flow and carry nonzero samples, but nothing reaches audible
    /// level. The stream is alive.
    QuietStarted { track: Track },
    QuietEnded { track: Track },
    /// Frames flow and are neither silent nor quiet, yet nothing has
    /// exceeded the device's learned dead-air floor for the whole window —
    /// the stream delivers only sub-floor dither (degraded audio daemon).
    DeadAirStarted { track: Track },
    DeadAirEnded { track: Track },
    /// A new playback stream appeared (Linux: candidate for capture-sink routing).
    SinkInputAdded { stream_id: u32 },
    /// The machine slept and woke; streams need a restart.
    SystemWoke { gap: Duration },
}

/// Current device world: stable key → ephemeral numeric id. A hotplugged
/// device is present with `None` for an id until the next enumeration pass
/// resolves it — no caller can ever observe a fake sentinel id.
#[derive(Default)]
pub struct Snapshot {
    devices: HashMap<DeviceKey, (Option<u32>, SourceKind)>,
}

impl Snapshot {
    /// Insert or renumber a device (`None` = present, id not yet known).
    pub fn upsert(&mut self, key: DeviceKey, id: Option<u32>, kind: SourceKind) {
        self.devices.insert(key, (id, kind));
    }

    pub fn remove(&mut self, key: &DeviceKey) {
        self.devices.remove(key);
    }

    /// Current numeric id for a device, if present and resolved.
    pub fn id_of(&self, key: &DeviceKey) -> Option<u32> {
        self.devices.get(key).and_then(|(id, _)| *id)
    }

    pub fn contains(&self, key: &DeviceKey) -> bool {
        self.devices.contains_key(key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_upsert_and_resolve() {
        let mut s = Snapshot::default();
        let k = DeviceKey("alsa_input.pci-0000".into());
        s.upsert(k.clone(), Some(64), SourceKind::Mic);
        assert_eq!(s.id_of(&k), Some(64));
        s.upsert(k.clone(), Some(144), SourceKind::Mic);
        assert_eq!(s.id_of(&k), Some(144));
        s.remove(&k);
        assert_eq!(s.id_of(&k), None);
        assert!(!s.contains(&k));
    }

    #[test]
    fn snapshot_pending_device_is_present_without_an_id() {
        let mut s = Snapshot::default();
        let k = DeviceKey("bluez_input.AC".into());
        s.upsert(k.clone(), None, SourceKind::Mic);
        assert!(s.contains(&k));
        assert_eq!(s.id_of(&k), None);
    }

    #[test]
    fn source_key_is_stable_identity_not_numeric_id() {
        let s = crate::source::Source {
            id: 64,
            key: DeviceKey("alsa_input.pci-0000_00_1f.3".into()),
            node_name: "alsa_input.pci-0000_00_1f.3".into(),
            description: "Built-in Audio".into(),
            kind: SourceKind::Mic,
            default_sample_rate: 48_000,
            default_channels: 1,
            is_virtual: false,
        };
        assert_eq!(s.key.0, s.node_name);
    }

}

/// Handle for the per-OS watcher thread; dropping it stops the watcher.
/// The inner value is only a keep-alive — its `Drop` quits the watcher loop.
pub struct WatcherHandle(#[allow(dead_code)] WatcherImpl);

#[cfg(target_os = "linux")]
type WatcherImpl = crate::topology_linux::WatcherHandle;
#[cfg(target_os = "windows")]
type WatcherImpl = crate::wasapi::topology::WatcherHandle;
#[cfg(target_os = "macos")]
type WatcherImpl = crate::macos::topology::WatcherHandle;
#[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
type WatcherImpl = ();

/// One process-wide platform watcher, many subscribers: the app-lifetime UI
/// listener and a per-recording supervisor share a single OS registration
/// instead of each spawning their own watcher.
pub struct TopologyHub {
    subs: std::sync::Arc<std::sync::Mutex<Vec<(u64, std::sync::mpsc::Sender<TopologyEvent>)>>>,
    next_token: std::sync::atomic::AtomicU64,
    _watcher: WatcherHandle,
}

/// Keep-alive for one hub subscription; dropping it unsubscribes.
pub struct HubSubscription {
    token: u64,
    subs: std::sync::Arc<std::sync::Mutex<Vec<(u64, std::sync::mpsc::Sender<TopologyEvent>)>>>,
}

impl Drop for HubSubscription {
    fn drop(&mut self) {
        if let Ok(mut s) = self.subs.lock() {
            s.retain(|(t, _)| *t != self.token);
        }
    }
}

impl TopologyHub {
    pub fn start() -> Self {
        let (tx, rx) = std::sync::mpsc::channel::<TopologyEvent>();
        let watcher = spawn_watcher(tx);
        let subs: std::sync::Arc<std::sync::Mutex<Vec<(u64, std::sync::mpsc::Sender<TopologyEvent>)>>> =
            std::sync::Arc::default();
        let subs_fan = std::sync::Arc::clone(&subs);
        // Fan-out thread; exits when the watcher (and with it the sender)
        // drops on hub drop. Dead subscribers are pruned on send failure.
        std::thread::Builder::new()
            .name("daisy-topology-hub".into())
            .spawn(move || {
                while let Ok(ev) = rx.recv() {
                    subs_fan
                        .lock()
                        .unwrap()
                        .retain(|(_, s)| s.send(ev.clone()).is_ok());
                }
            })
            .expect("spawn topology hub thread");
        Self {
            subs,
            next_token: std::sync::atomic::AtomicU64::new(0),
            _watcher: watcher,
        }
    }

    /// Fan events into `tx` until the returned guard drops.
    pub fn subscribe(&self, tx: std::sync::mpsc::Sender<TopologyEvent>) -> HubSubscription {
        let token = self
            .next_token
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.subs.lock().unwrap().push((token, tx));
        HubSubscription {
            token,
            subs: std::sync::Arc::clone(&self.subs),
        }
    }
}

/// Start the platform topology watcher, feeding events into `tx`.
pub fn spawn_watcher(tx: std::sync::mpsc::Sender<TopologyEvent>) -> WatcherHandle {
    #[cfg(target_os = "linux")]
    {
        WatcherHandle(crate::topology_linux::spawn_watcher(tx))
    }
    #[cfg(target_os = "windows")]
    {
        WatcherHandle(crate::wasapi::topology::spawn_watcher(tx))
    }
    #[cfg(target_os = "macos")]
    {
        WatcherHandle(crate::macos::topology::spawn_watcher(tx))
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
    {
        let _ = tx;
        WatcherHandle(())
    }
}

/// True when this platform's system-audio capture is bound to the default
/// output at open and strands when the default moves (WASAPI loopback);
/// such captures need a reopen on a default-output change.
pub fn system_capture_reopens_on_default_change() -> bool {
    cfg!(target_os = "windows")
}

/// Stable key of the OS default input device right now, if resolvable.
pub fn default_input_key() -> Option<DeviceKey> {
    #[cfg(target_os = "linux")]
    {
        crate::topology_linux::default_input_key()
    }
    #[cfg(target_os = "windows")]
    {
        crate::wasapi::topology::default_input_key()
    }
    #[cfg(target_os = "macos")]
    {
        crate::macos::topology::default_input_key()
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
    {
        None
    }
}
