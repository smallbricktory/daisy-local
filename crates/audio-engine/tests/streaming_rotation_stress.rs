// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Chunk rotation against a live capture, at the rate a long meeting reaches
//! only after hours.
//!
//! Rotation runs on the loop thread while the stream's `process` callback runs
//! on PipeWire's realtime thread. Both reach the same writer slot, so a slot
//! that is not safe to share across threads aborts the process here rather
//! than one rotation in several hundred.
//!
//! Requires a running PipeWire daemon with the PulseAudio shim (`pactl`).
//! Run with:
//!   cargo test -p audio-engine --test streaming_rotation_stress -- --ignored --nocapture

mod common;

use audio_engine::capture::{run_dual_streaming, StreamingCaptureRequest, StreamingHandle};
use common::{pulse_available, sink_rig};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use tempfile::TempDir;

/// How long to keep rotating.
const RUN: Duration = Duration::from_secs(20);
/// Gap between rotations. Short enough that a rotation lands inside a frame
/// write; the capture delivers a buffer roughly every 10-20 ms.
const ROTATE_EVERY: Duration = Duration::from_millis(7);

#[test]
#[ignore = "requires PipeWire + pactl; run with --ignored"]
fn rotating_chunks_against_a_live_capture_does_not_abort() {
    let _ = env_logger::builder().is_test(true).try_init();

    if !pulse_available() {
        eprintln!("skipping: pactl not available");
        return;
    }

    let td = TempDir::new().unwrap();
    let (mic, monitor, _vs) = sink_rig("daisy-rotation-stress");

    let req = StreamingCaptureRequest {
        mic_source_id: mic.id,
        system_source_id: monitor.id,
        sample_rate: 16_000,
        mic_clock: Arc::new(audio_engine::capture::ChunkClock::default()),
        system_clock: Arc::new(audio_engine::capture::ChunkClock::default()),
        topology_tx: None,
        allow_os_voice_processing: false,
    };

    let handle_slot: Arc<Mutex<Option<StreamingHandle>>> = Arc::new(Mutex::new(None));
    let handle_slot_for_cb = Arc::clone(&handle_slot);
    let join = thread::spawn(move || {
        run_dual_streaming(req, move |h| {
            *handle_slot_for_cb.lock().unwrap() = Some(h);
        })
        .unwrap();
    });

    let handle = loop {
        if let Some(h) = handle_slot.lock().unwrap().take() {
            break h;
        }
        thread::sleep(Duration::from_millis(20));
    };

    let started = Instant::now();
    let mut rotations: u32 = 0;
    while started.elapsed() < RUN {
        let dir = td.path().join(format!("c{rotations}"));
        std::fs::create_dir_all(&dir).unwrap();
        handle
            .open_chunk(rotations, &dir.join("mic.wav"), &dir.join("system.wav"))
            .unwrap();
        rotations += 1;
        thread::sleep(ROTATE_EVERY);
    }
    handle.close_chunk().unwrap();
    handle.stop().unwrap();
    join.join().unwrap();

    // Reaching here at all is the assertion: the race aborts the process, so a
    // failing run never returns to report one.
    eprintln!("survived {rotations} rotations in {:?}", started.elapsed());
    assert!(rotations > 100, "too few rotations to be a stress: {rotations}");
}
