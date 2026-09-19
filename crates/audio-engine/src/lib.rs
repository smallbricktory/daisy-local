// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Daisy audio engine: source enumeration, dual-stream capture, virtual sink.

pub mod autogain;
pub mod bt_profile;
pub mod topology;
#[cfg(target_os = "linux")]
mod topology_linux;
pub mod capture;
pub mod correlation;
pub mod error;
pub mod gain;
pub(crate) mod manifest;
pub mod mic_clients;
pub mod resample;
pub mod routing;
pub mod source;
#[cfg(any(target_os = "macos", test))]
pub(crate) mod sysgain;
pub mod virtual_sink;
pub mod wav;

// The internal module is aliased `wasapi`, apart from the `windows` crate
// name (the WASAPI bindings). Files live in `src/windows/`.
#[cfg(target_os = "windows")]
#[path = "windows/mod.rs"]
mod wasapi;

// macOS capture backend (Core Audio tap + AVAudioEngine). Standard module name —
// `crate::macos::*` dispatch calls in capture.rs/source.rs resolve to this.
#[cfg(target_os = "macos")]
mod macos;

// Root re-exports: only what external crates actually reach via
// `audio_engine::X`; everything else is used through its module path.
pub use capture::{capture_dual, capture_one, run_dual_streaming, DualCaptureRequest};
pub use source::{list_sources, SourceKind};
pub use virtual_sink::VirtualSink;

