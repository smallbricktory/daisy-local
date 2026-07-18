//! Detect the user's current audio routing.
//!
//! "Speaker-class" output = built-in laptop speakers, external speakers, or
//! the 3.5mm jack with port=Speaker (NOT port=Headphones). Headphones,
//! headsets, AirPods, and Bluetooth devices are not speaker-class — they
//! produce no acoustic bleed back into the mic.
//!
//! Rule: a session is treated as bleed-prone when the default output is
//! speaker-class.

#[cfg(target_os = "linux")]
use crate::error::Error;
use crate::error::Result;
#[cfg(target_os = "linux")]
use std::process::Command;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputClass {
    /// Built-in or external speakers. Bleeds into the mic.
    Speaker,
    /// Headphones, headset, AirPods, USB headphones, or 3.5mm jack with
    /// the Headphones port active. Does NOT bleed.
    Headphone,
    /// Undetermined; treated as Speaker (AEC runs).
    Unknown,
}

#[derive(Debug, Clone)]
pub struct RoutingSnapshot {
    pub default_sink_name: String,
    pub default_sink_description: String,
    pub output_class: OutputClass,
}

impl RoutingSnapshot {
    pub fn needs_aec(&self) -> bool {
        matches!(self.output_class, OutputClass::Speaker | OutputClass::Unknown)
    }
}

// ── Platform output-device classifiers (pure; unit-tested on any OS) ─────────
// The macOS/Windows `detect_routing()` branches gather a small integer code
// from the native API and map it here. Classification: on-the-ear devices
// (BT, headphones, headset) → Headphone (no bleed, AEC skipped);
// speaker-class or display speakers → Speaker; anything ambiguous → Unknown
// (AEC stays on).

/// macOS CoreAudio transport classes, as emitted by the Swift shim's
/// `daisy_default_output_class()`. Keep in sync with shim.swift.
pub mod macos_transport {
    pub const UNKNOWN: i32 = 0;
    pub const BUILTIN_SPEAKER: i32 = 1;
    pub const BUILTIN_HEADPHONES: i32 = 2; // 3.5mm jack, Headphones data source
    pub const BLUETOOTH: i32 = 3;
    pub const USB: i32 = 4;
    pub const DISPLAY: i32 = 5; // HDMI / DisplayPort → external display speakers
    pub const VIRTUAL: i32 = 6; // aggregate / virtual device
}

/// Map a macOS transport code to an [`OutputClass`].
#[cfg_attr(target_os = "linux", allow(dead_code))]
pub fn classify_macos_transport(code: i32) -> OutputClass {
    use macos_transport::*;
    match code {
        BLUETOOTH | BUILTIN_HEADPHONES => OutputClass::Headphone,
        BUILTIN_SPEAKER | DISPLAY => OutputClass::Speaker,
        // USB (headset DAC or USB speakers) and virtual/unknown are
        // ambiguous → Unknown (AEC on).
        _ => OutputClass::Unknown,
    }
}

/// Windows `EndpointFormFactor` enum values (mmdeviceapi.h). Used to classify
/// the default render endpoint when it is NOT a Bluetooth device (BT is detected
/// separately via the enumerator name, which is more reliable than form factor).
pub mod windows_form_factor {
    pub const SPEAKERS: u32 = 1;
    pub const HEADPHONES: u32 = 3;
    pub const HEADSET: u32 = 5;
    pub const DIGITAL_AUDIO_DISPLAY_DEVICE: u32 = 9; // HDMI/DP display speakers
}

/// True when the given input (mic) device is
/// headphone/headset/Bluetooth-class. macOS queries CoreAudio; other
/// platforms return false.
pub fn mic_is_headphone(mic_device_id: u32) -> bool {
    #[cfg(target_os = "macos")]
    {
        classify_macos_transport(crate::macos::input_class(mic_device_id)) == OutputClass::Headphone
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = mic_device_id;
        false
    }
}

/// Map a Windows `EndpointFormFactor` to an [`OutputClass`].
#[cfg_attr(target_os = "linux", allow(dead_code))]
pub fn classify_windows_form_factor(form_factor: u32) -> OutputClass {
    use windows_form_factor::*;
    match form_factor {
        HEADPHONES | HEADSET => OutputClass::Headphone,
        SPEAKERS | DIGITAL_AUDIO_DISPLAY_DEVICE => OutputClass::Speaker,
        _ => OutputClass::Unknown,
    }
}

#[cfg(test)]
mod classifier_tests {
    use super::*;

    #[test]
    fn macos_bluetooth_and_headphones_skip_aec() {
        assert_eq!(classify_macos_transport(macos_transport::BLUETOOTH), OutputClass::Headphone);
        assert_eq!(classify_macos_transport(macos_transport::BUILTIN_HEADPHONES), OutputClass::Headphone);
        assert!(!RoutingSnapshot { default_sink_name: String::new(), default_sink_description: String::new(), output_class: classify_macos_transport(macos_transport::BLUETOOTH) }.needs_aec());
    }

    #[test]
    fn macos_speakers_and_display_need_aec() {
        assert_eq!(classify_macos_transport(macos_transport::BUILTIN_SPEAKER), OutputClass::Speaker);
        assert_eq!(classify_macos_transport(macos_transport::DISPLAY), OutputClass::Speaker);
    }

    #[test]
    fn macos_usb_and_unknown_are_conservative() {
        assert_eq!(classify_macos_transport(macos_transport::USB), OutputClass::Unknown);
        assert_eq!(classify_macos_transport(macos_transport::VIRTUAL), OutputClass::Unknown);
        assert_eq!(classify_macos_transport(999), OutputClass::Unknown);
    }

    #[test]
    fn windows_headphones_headset_skip_aec() {
        assert_eq!(classify_windows_form_factor(windows_form_factor::HEADPHONES), OutputClass::Headphone);
        assert_eq!(classify_windows_form_factor(windows_form_factor::HEADSET), OutputClass::Headphone);
    }

    #[test]
    fn windows_speakers_and_hdmi_need_aec() {
        assert_eq!(classify_windows_form_factor(windows_form_factor::SPEAKERS), OutputClass::Speaker);
        assert_eq!(classify_windows_form_factor(windows_form_factor::DIGITAL_AUDIO_DISPLAY_DEVICE), OutputClass::Speaker);
    }

    #[test]
    fn windows_unknown_form_factor_conservative() {
        assert_eq!(classify_windows_form_factor(0), OutputClass::Unknown); // RemoteNetworkDevice
        assert_eq!(classify_windows_form_factor(10), OutputClass::Unknown); // UnknownFormFactor
    }

    /// Manual probe: prints the live default-output classification. Run on a
    /// real device to confirm the platform query works and to check a given
    /// output (e.g. AirPods → Headphone → needs_aec=false).
    #[test]
    #[ignore = "hits the live OS audio API; run with --ignored on a device"]
    fn print_live_output_class() {
        let snap = detect_routing().expect("detect_routing");
        eprintln!(
            "live routing: class={:?} needs_aec={} desc={:?} default_mic_is_headphone={}",
            snap.output_class,
            snap.needs_aec(),
            snap.default_sink_description,
            mic_is_headphone(0)
        );
    }
}

/// Probe `pactl` for the default sink and classify it.
pub fn detect_routing() -> Result<RoutingSnapshot> {
    #[cfg(target_os = "macos")]
    {
        // CoreAudio: classify the default output device's transport type.
        let code = crate::macos::default_output_class();
        return Ok(RoutingSnapshot {
            default_sink_name: "default-output".into(),
            default_sink_description: format!("macos transport code {code}"),
            output_class: classify_macos_transport(code),
        });
    }
    #[cfg(target_os = "windows")]
    {
        // WASAPI: Bluetooth enumerator (most reliable for BT headsets) →
        // Headphone; otherwise the endpoint form factor.
        let class = crate::wasapi::default_output_class().unwrap_or(OutputClass::Unknown);
        return Ok(RoutingSnapshot {
            default_sink_name: "default-render".into(),
            default_sink_description: String::new(),
            output_class: class,
        });
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        // No routing probe on other targets; returns Unknown (AEC on).
        return Ok(RoutingSnapshot {
            default_sink_name: String::new(),
            default_sink_description: String::new(),
            output_class: OutputClass::Unknown,
        });
    }
    #[cfg(target_os = "linux")]
    {
        let default_sink = run_pactl(&["get-default-sink"])?;
        let default_sink = default_sink.trim().to_string();
        if default_sink.is_empty() {
            return Ok(RoutingSnapshot {
                default_sink_name: String::new(),
                default_sink_description: String::new(),
                output_class: OutputClass::Unknown,
            });
        }
        let verbose = run_pactl(&["list", "sinks"])?;
        let (description, props) = sink_block(&verbose, &default_sink);
        let output_class = classify_output(&default_sink, &props);
        Ok(RoutingSnapshot {
            default_sink_name: default_sink,
            default_sink_description: description,
            output_class,
        })
    }
}

#[cfg(target_os = "linux")]
fn run_pactl(args: &[&str]) -> Result<String> {
    let output = Command::new("pactl").args(args).output().map_err(Error::Io)?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::Subprocess(format!("pactl {args:?}: {stderr}")));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Find the block in `pactl list sinks` for the given Name and return
/// (description, properties_text).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn sink_block(verbose: &str, target_name: &str) -> (String, String) {
    let mut in_block = false;
    let mut description = String::new();
    let mut props = String::new();
    let target_line = format!("Name: {target_name}");
    for line in verbose.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("Sink #") {
            // Reset state at each new sink boundary.
            if in_block {
                break;
            }
            in_block = false;
            continue;
        }
        if !in_block && trimmed == target_line {
            in_block = true;
            continue;
        }
        if !in_block {
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("Description: ") {
            description = rest.to_string();
        }
        // Capture every line in the block; classification reads it as one blob.
        props.push_str(line);
        props.push('\n');
    }
    (description, props)
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn classify_output(name: &str, props: &str) -> OutputClass {
    let lower_name = name.to_ascii_lowercase();
    let lower_props = props.to_ascii_lowercase();

    // Strongest signals first: Bluetooth.
    if lower_name.starts_with("bluez") || lower_props.contains("device.bus = \"bluetooth\"") {
        return OutputClass::Headphone;
    }
    // device.form_factor is the canonical signal.
    if lower_props.contains("device.form_factor = \"headphone\"")
        || lower_props.contains("device.form_factor = \"headset\"")
        || lower_props.contains("device.form_factor = \"hands-free\"")
    {
        return OutputClass::Headphone;
    }
    // 3.5mm jack: PipeWire names sinks with the active port. Look for the
    // Headphones port being active.
    if lower_name.contains("headphones") {
        return OutputClass::Headphone;
    }
    // Explicit speaker form factor or speaker-port name.
    if lower_props.contains("device.form_factor = \"speaker\"")
        || lower_name.contains("speaker")
    {
        return OutputClass::Speaker;
    }
    // Built-in laptop output that isn't otherwise classified — treat as speaker.
    if lower_props.contains("device.api = \"alsa\"") {
        return OutputClass::Speaker;
    }
    OutputClass::Unknown
}
