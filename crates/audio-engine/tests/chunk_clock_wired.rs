// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Every capture backend must record per-chunk timing.
//!
//! `ChunkClock` reports `None`/0 when nothing calls `note()`, and `None` is a
//! legal value meaning "legacy session". A backend that silently stops
//! recording timing therefore looks identical to one that never had it, and
//! the failure only surfaces as an empty manifest field after a real
//! recording on that OS. The backends are `#[cfg]`-gated, so no single build
//! compiles all three — this reads the sources instead.

use std::fs;

/// (file, what marks the frame-write site)
const BACKENDS: &[(&str, &str)] = &[
    ("src/capture.rs", "linux/pipewire"),
    ("src/macos/mod.rs", "macos"),
    ("src/windows/mod.rs", "windows"),
];

#[test]
fn every_backend_notes_frames_into_its_chunk_clock() {
    for (path, label) in BACKENDS {
        let src = fs::read_to_string(path).unwrap_or_else(|e| panic!("read {path}: {e}"));
        let notes = src.matches(".note(").count();
        assert!(
            notes >= 1,
            "{label} backend ({path}) never calls ChunkClock::note — per-chunk \
             capture timing will be silently absent on that platform",
        );
    }
}

#[test]
fn every_backend_resets_its_chunk_clock_on_open() {
    for (path, label) in BACKENDS {
        let src = fs::read_to_string(path).unwrap_or_else(|e| panic!("read {path}: {e}"));
        let resets = src.matches(".reset()").count();
        assert!(
            resets >= 1,
            "{label} backend ({path}) never calls ChunkClock::reset — timing \
             would accumulate across chunks instead of describing one",
        );
    }
}
