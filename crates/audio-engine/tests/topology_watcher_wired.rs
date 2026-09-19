// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Source-level wiring guards for the per-OS topology watchers.
//!
//! The watchers are `#[cfg]`-gated, so no single build compiles all of them
//! and a lost registration would pass every runtime test on the wrong OS.
//! These assertions pin the load-bearing API calls in the source text.

use std::path::Path;

fn read(rel: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

#[test]
fn linux_watcher_watches_registry_defaults_and_streams() {
    let src = read("src/topology_linux.rs");
    for needle in [
        "global_remove",
        "default.audio.source",
        "default.audio.sink",
        "Stream/Output/Audio",
        "ProfileChanged",
    ] {
        assert!(
            src.contains(needle),
            "topology_linux.rs no longer contains `{needle}`"
        );
    }
}

#[test]
fn linux_capture_streams_report_death() {
    let src = read("src/capture.rs");
    for needle in ["state_changed", "stream_death", "StreamDied"] {
        assert!(
            src.contains(needle),
            "capture.rs no longer contains `{needle}`"
        );
    }
}

/// The field-log checker script pattern-matches these exact log strings;
/// a reword silently breaks field validation. Change them ONLY together
/// with the checker.
#[test]
fn trace_check_log_contracts_are_intact() {
    let sup = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../recording/src/supervisor.rs"),
    )
    .expect("read supervisor.rs");
    for needle in [
        "mic recovery ({why}) attempt",
        "recovery budget exhausted",
        "system woke after",
        "default output moved — system reopen",
        "System died ({reason:?}) — reconnect attempt",
        "supervisor: {ev:?}",
    ] {
        assert!(sup.contains(needle), "supervisor.rs lost trace-check contract `{needle}`");
    }
    let fmt = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../tauri-app/src/logging.rs"),
    )
    .expect("read logging.rs");
    assert!(
        fmt.contains("%Y-%m-%dT%H:%M:%S%.3f"),
        "logging.rs file-sink timestamp format is a trace-check contract"
    );
}

#[test]
fn every_os_emits_volume_changed() {
    // The VolumeChanged policy is OS-neutral; each watcher must feed it.
    for (file, needle) in [
        ("src/topology_linux.rs", "pactl"),
        ("src/topology_linux.rs", "VolumeChanged"),
        ("src/windows/topology.rs", "RegisterControlChangeNotify"),
        ("src/windows/topology.rs", "VolumeChanged"),
        ("src/macos/topology.rs", "volm"),
        ("src/macos/topology.rs", "VolumeChanged"),
    ] {
        let src = read(file);
        assert!(src.contains(needle), "{file} lost its VolumeChanged emitter (`{needle}`)");
    }
}

#[test]
fn windows_watcher_registers_mmdevice_notifications() {
    let src = read("src/windows/topology.rs");
    for needle in [
        "RegisterEndpointNotificationCallback",
        "OnDefaultDeviceChanged",
        "OnDeviceStateChanged",
    ] {
        assert!(
            src.contains(needle),
            "windows/topology.rs no longer contains `{needle}`"
        );
    }
}

#[test]
fn macos_watcher_listens_and_shim_signals_config_change() {
    let topo = read("src/macos/topology.rs");
    for needle in ["AudioObjectAddPropertyListener", "dev#", "dIn ", "dOut"] {
        assert!(
            topo.contains(needle),
            "macos/topology.rs no longer contains `{needle}`"
        );
    }
    let shim = read("src/macos/shim.swift");
    for needle in [
        "kAudioUnitSubType_HALOutput",
        "daisy_mic_restart",
        "daisy_tap_restart",
        "AudioUnitRender",
    ] {
        assert!(
            shim.contains(needle),
            "shim.swift no longer contains `{needle}`"
        );
    }
    assert!(
        !shim.contains("AVAudioEngine()"),
        "shim.swift regressed to AVAudioEngine capture"
    );
}

#[test]
fn sleep_inhibition_covers_all_three_oses() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../recording/src/inhibit.rs");
    let src = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read inhibit.rs: {e}"));
    for needle in [
        "systemd-inhibit",
        "IOPMAssertionCreateWithName",
        "SetThreadExecutionState",
    ] {
        assert!(src.contains(needle), "inhibit.rs no longer contains `{needle}`");
    }
}

#[test]
fn supervisor_handles_every_topology_event_variant() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../recording/src/supervisor.rs");
    let src = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read supervisor.rs: {e}"));
    for variant in [
        "TopologyEvent::DeviceAdded",
        "TopologyEvent::DeviceRemoved",
        "TopologyEvent::DefaultInputChanged",
        "TopologyEvent::DefaultOutputChanged",
        "TopologyEvent::ProfileChanged",
        "TopologyEvent::VolumeChanged",
        "TopologyEvent::StreamDied",
        "TopologyEvent::SilenceStarted",
        "TopologyEvent::SilenceEnded",
        "TopologyEvent::QuietStarted",
        "TopologyEvent::QuietEnded",
        "TopologyEvent::SinkInputAdded",
        "TopologyEvent::SystemWoke",
    ] {
        assert!(
            src.contains(variant),
            "supervisor.rs no longer matches `{variant}`"
        );
    }
    let handler = src
        .split("fn handle_event")
        .nth(1)
        .expect("handle_event present");
    let end = handler.find("\n}").unwrap_or(handler.len());
    assert!(
        !handler[..end].contains("_ =>"),
        "handle_event grew a wildcard arm — new TopologyEvent variants must be matched explicitly"
    );
}

#[test]
fn commanded_stream_teardown_sets_the_stopping_flags() {
    let src = read("src/capture.rs");
    for needle in [
        "parts.stopping.set(true);",
        "stopping_mic_for_cmd.set(true);",
        "stopping_sys_for_cmd.set(true);",
    ] {
        assert!(
            src.contains(needle),
            "capture.rs lost a commanded-teardown guard: `{needle}`"
        );
    }
}
