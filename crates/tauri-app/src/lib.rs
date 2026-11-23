// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Tauri backend library embedded by the main.rs binary.

pub mod ai_jobs;
pub mod bootstrap;
pub mod bt_prefs;
pub mod builtin_model;
pub mod commands;
pub mod downloads;
pub mod perf;
pub mod logging;
pub mod library_events;
pub mod live_audio;
pub mod live_diarize;
pub mod live_names;
pub mod machine_id;
pub mod mcp;
pub mod media_scheme;
pub mod meeting_detect;
pub mod openblas;
#[cfg(target_os = "linux")]
pub mod playback_file;
pub mod mini_window;
pub mod error;
pub mod hardware;
pub mod hotkey;
pub mod migrations;
pub mod profile;
pub mod settings;
pub mod single_instance;
pub mod sleep_watch;
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

/// The process-wide keyring as tests share it. A test that reads or writes
/// sealed session files holds [`test_keys::unlocked`]; a test that locks,
/// unlocks or forgets keys holds [`test_keys::exclusive`].
#[cfg(test)]
pub(crate) mod test_keys {
    use std::sync::{OnceLock, RwLock, RwLockReadGuard, RwLockWriteGuard};

    static RING: RwLock<()> = RwLock::new(());

    /// The keyring unlocked (with one content key per test process when no
    /// test left another open), held so no exclusive test locks it.
    pub(crate) fn unlocked() -> RwLockReadGuard<'static, ()> {
        static SECRET: OnceLock<[u8; 32]> = OnceLock::new();
        let guard = RING.read().unwrap_or_else(|e| e.into_inner());
        if !sealfile::keyring::is_unlocked() {
            let bytes = *SECRET.get_or_init(|| *sealfile::ContentSecret::generate().0.to_bytes());
            sealfile::keyring::unlock(sealfile::ContentSecret::from_bytes(bytes));
        }
        guard
    }

    /// Sole use of the keyring.
    pub(crate) fn exclusive() -> RwLockWriteGuard<'static, ()> {
        RING.write().unwrap_or_else(|e| e.into_inner())
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
