// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Every TCC-gated framework the app links needs its usage string in
//! Info.plist: macOS kills the process outright the first time it touches the
//! service without one.
use std::path::{Path, PathBuf};

/// Every framework a Swift shim imports, and the Info.plist key its TCC
/// service demands; `None` for a framework that gates nothing. An import
/// missing from this table fails the test, so a new framework is checked for
/// a usage string before it ships.
const FRAMEWORKS: &[(&str, Option<&str>)] = &[
    ("AVFAudio", Some("NSMicrophoneUsageDescription")),
    ("AVFoundation", Some("NSMicrophoneUsageDescription")),
    ("AudioToolbox", None),
    ("CoreAudio", Some("NSAudioCaptureUsageDescription")),
    ("CoreBluetooth", Some("NSBluetoothAlwaysUsageDescription")),
    ("Darwin", None),
    ("Foundation", None),
    // The shim transcribes through SpeechAnalyzer, which runs on device.
    // SFSpeechRecognizer would want NSSpeechRecognitionUsageDescription.
    ("Speech", None),
];

/// A usage string and the hardened-runtime entitlement that has to travel
/// with it: a signed build is refused the service without both.
const ENTITLED: &[(&str, &str)] = &[
    ("NSMicrophoneUsageDescription", "com.apple.security.device.audio-input"),
    ("NSAudioCaptureUsageDescription", "com.apple.security.device.audio-input"),
    ("NSBluetoothAlwaysUsageDescription", "com.apple.security.device.bluetooth"),
];

fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().and_then(Path::parent).unwrap().to_path_buf()
}

fn read(rel: &str) -> String {
    std::fs::read_to_string(workspace().join(rel)).unwrap()
}

/// The Swift shims compiled into the app binary, as one text. Probes under
/// `tests/` are separate executables and carry their own bundles.
fn shim_sources() -> String {
    let mut all = String::new();
    for crate_dir in std::fs::read_dir(workspace().join("crates")).unwrap().flatten() {
        for entry in walkdir::WalkDir::new(crate_dir.path().join("src")).into_iter().flatten() {
            if entry.path().extension().is_some_and(|e| e == "swift") {
                all.push_str(&std::fs::read_to_string(entry.path()).unwrap());
            }
        }
    }
    assert!(!all.is_empty(), "no Swift shims found; the scan is looking in the wrong place");
    all
}

#[test]
fn info_plist_declares_a_usage_string_for_every_linked_tcc_framework() {
    let plist = read("crates/tauri-app/Info.plist");
    let sources = shim_sources();
    for (framework, key) in FRAMEWORKS {
        let Some(key) = key else { continue };
        assert!(
            !sources.contains(&format!("import {framework}")) || plist.contains(key),
            "a shim imports {framework} but Info.plist has no {key}"
        );
    }
}

#[test]
fn every_framework_a_shim_imports_is_accounted_for() {
    for line in shim_sources().lines() {
        let Some(framework) = line.trim().strip_prefix("import ") else { continue };
        let framework = framework.trim();
        assert!(
            FRAMEWORKS.iter().any(|(f, _)| f == &framework),
            "a shim imports {framework}: add it to FRAMEWORKS with the usage string its TCC service demands, or None"
        );
    }
}

#[test]
fn every_usage_string_has_its_entitlement() {
    let plist = read("crates/tauri-app/Info.plist");
    let entitlements = read("crates/tauri-app/Daisy.entitlements");
    for (key, entitlement) in ENTITLED {
        assert!(
            !plist.contains(key) || entitlements.contains(entitlement),
            "Info.plist declares {key} but Daisy.entitlements has no {entitlement}"
        );
    }
}
