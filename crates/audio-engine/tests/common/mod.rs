//! Shared PipeWire test rig for the virtual-sink integration tests. Also
//! included by the recording and tauri-app integration tests via `#[path]`.
#![allow(dead_code)]

use audio_engine::source::{list_sources, Source, SourceKind};
use audio_engine::virtual_sink::VirtualSink;

/// True when a PulseAudio-compatible server answers `pactl info`.
pub fn pulse_available() -> bool {
    std::process::Command::new("pactl")
        .arg("info")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Create a virtual sink named `sink_name`, wait for PipeWire to register
/// it, and return (a mic source, the sink's monitor source, the sink guard).
/// The sink is torn down when the guard drops.
pub fn sink_rig(sink_name: &str) -> (Source, Source, VirtualSink) {
    let vs = VirtualSink::create(sink_name).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(300));
    let all = list_sources().unwrap();
    let mic = all
        .iter()
        .find(|s| s.kind == SourceKind::Mic)
        .cloned()
        .expect("no mic source available");
    let monitor_name = format!("{sink_name}.monitor");
    let monitor = all
        .iter()
        .find(|s| s.node_name == monitor_name)
        .cloned()
        .unwrap_or_else(|| panic!("virtual sink monitor {monitor_name} not found in source list"));
    (mic, monitor, vs)
}
