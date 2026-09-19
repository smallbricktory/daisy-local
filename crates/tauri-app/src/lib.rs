// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Tauri backend library embedded by the main.rs binary.

pub mod ai_jobs;
pub mod bootstrap;
pub mod bt_prefs;
pub mod commands;
pub mod downloads;
pub mod perf;
pub mod logging;
pub mod library_events;
pub mod machine_id;
pub mod mcp;
pub mod meeting_detect;
pub mod openblas;
pub mod mini_window;
pub mod error;
pub mod hardware;
pub mod hotkey;
pub mod migrate_v3;
pub mod migrations;
pub mod profile;
pub mod settings;
pub mod single_instance;
pub mod state;
pub mod sync;
pub mod telemetry;

/// Synthetic-audio helpers shared by test modules that build session WAVs.
#[cfg(test)]
pub(crate) mod test_audio {
    /// Square-ish wave: both peak and RMS ≈ `amp`, 16 kHz mono.
    pub(crate) fn tone(secs: usize, amp: f32) -> Vec<i16> {
        let v = (amp * i16::MAX as f32) as i16;
        (0..16_000 * secs).map(|i| if i % 2 == 0 { v } else { -v }).collect()
    }

    pub(crate) fn write_wav(path: &std::path::Path, samples: &[i16]) {
        crate::commands::write_wav_mono_16k(path, samples).unwrap();
    }
}

/// Seconds since the Unix epoch; returns 0 if the system clock is set
/// before 1970.
pub fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
