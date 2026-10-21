// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Integration test for the loop-driven streaming capture API.
//!
//! Requires a running PipeWire daemon with the PulseAudio shim (`pactl`).
//! Run with:
//!   cargo test -p audio-engine --test streaming_lifecycle -- --ignored --nocapture

mod common;

use audio_engine::capture::{run_dual_streaming, StreamingCaptureRequest, StreamingHandle};
use common::{pulse_available, sink_rig};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use tempfile::TempDir;

fn read_wav_samples(p: &std::path::Path) -> Vec<i16> {
    let mut r = hound::WavReader::open(p).unwrap();
    r.samples::<i16>().map(|x| x.unwrap()).collect()
}

#[test]
#[ignore = "requires PipeWire + pactl; run with --ignored"]
fn run_dual_streaming_two_chunks_round_trip() {
    let _ = env_logger::builder().is_test(true).try_init();

    if !pulse_available() {
        eprintln!("skipping: pactl not available");
        return;
    }

    let td = TempDir::new().unwrap();
    let chunk1 = td.path().join("c1");
    let chunk2 = td.path().join("c2");
    std::fs::create_dir_all(&chunk1).unwrap();
    std::fs::create_dir_all(&chunk2).unwrap();

    let (mic, monitor, _vs) = sink_rig("daisy-test-1b");

    let req = StreamingCaptureRequest {
        mic_source_id: mic.id,
        system_source_id: monitor.id,
        sample_rate: 16_000,
        mic_clock: std::sync::Arc::new(audio_engine::capture::ChunkClock::default()),
        system_clock: std::sync::Arc::new(audio_engine::capture::ChunkClock::default()),
        topology_tx: None,
        allow_os_voice_processing: false,
    };

    let handle_slot: Arc<Mutex<Option<StreamingHandle>>> = Arc::new(Mutex::new(None));
    let handle_slot_for_cb = Arc::clone(&handle_slot);

    // run_dual_streaming is blocking; run it on a separate thread.
    let join = thread::spawn(move || {
        run_dual_streaming(req, move |h| {
            *handle_slot_for_cb.lock().unwrap() = Some(h);
        })
        .unwrap();
    });

    // Wait for the handle to be deposited by on_ready.
    let handle = loop {
        if let Some(h) = handle_slot.lock().unwrap().take() {
            break h;
        }
        thread::sleep(Duration::from_millis(20));
    };

    // Chunk 1
    handle
        .open_chunk(1, &chunk1.join("mic.wav"), &chunk1.join("system.wav"))
        .unwrap();
    thread::sleep(Duration::from_millis(800));
    handle.close_chunk().unwrap();

    // Brief gap — audio between chunks should be silently discarded.
    thread::sleep(Duration::from_millis(50));

    // Chunk 2
    handle
        .open_chunk(1, &chunk2.join("mic.wav"), &chunk2.join("system.wav"))
        .unwrap();
    thread::sleep(Duration::from_millis(800));
    handle.close_chunk().unwrap();

    handle.stop().unwrap();
    join.join().unwrap();

    // Both WAV files should contain meaningful samples.
    let m1 = read_wav_samples(&chunk1.join("mic.wav"));
    let m2 = read_wav_samples(&chunk2.join("mic.wav"));
    assert!(
        m1.len() > 4_000,
        "chunk 1 mic too short: {} samples",
        m1.len()
    );
    assert!(
        m2.len() > 4_000,
        "chunk 2 mic too short: {} samples",
        m2.len()
    );

    // Verify system WAV files exist too.
    let s1 = read_wav_samples(&chunk1.join("system.wav"));
    let s2 = read_wav_samples(&chunk2.join("system.wav"));
    assert!(
        s1.len() > 4_000,
        "chunk 1 system too short: {} samples",
        s1.len()
    );
    assert!(
        s2.len() > 4_000,
        "chunk 2 system too short: {} samples",
        s2.len()
    );
}

#[test]
#[ignore = "requires PipeWire + pactl; run with --ignored"]
fn open_chunk_reports_the_track_that_failed_and_keeps_the_current_chunk() {
    let _ = env_logger::builder().is_test(true).try_init();
    if !pulse_available() {
        eprintln!("skipping: pactl not available");
        return;
    }
    let td = TempDir::new().unwrap();
    let chunk1 = td.path().join("c1");
    std::fs::create_dir_all(&chunk1).unwrap();
    let missing = td.path().join("no-such-dir");
    let (mic, monitor, _vs) = sink_rig("daisy-test-open-fail");
    let req = StreamingCaptureRequest {
        mic_source_id: mic.id,
        system_source_id: monitor.id,
        sample_rate: 16_000,
        mic_clock: std::sync::Arc::new(audio_engine::capture::ChunkClock::default()),
        system_clock: std::sync::Arc::new(audio_engine::capture::ChunkClock::default()),
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

    // A first chunk whose system file can't be created fails, naming it.
    let err = handle
        .open_chunk(1, &chunk1.join("mic.wav"), &missing.join("system.wav"))
        .unwrap_err();
    assert!(
        matches!(err, audio_engine::error::Error::TrackFileOpen { track: "system", .. }),
        "{err}"
    );
    // Same for the mic file.
    let err = handle
        .open_chunk(1, &missing.join("mic.wav"), &chunk1.join("system.wav"))
        .unwrap_err();
    assert!(matches!(err, audio_engine::error::Error::TrackFileOpen { track: "mic", .. }), "{err}");

    // A rotation that can't open keeps capture writing the open chunk.
    handle
        .open_chunk(1, &chunk1.join("mic.wav"), &chunk1.join("system.wav"))
        .unwrap();
    thread::sleep(Duration::from_millis(400));
    assert!(handle
        .open_chunk(2, &missing.join("mic.wav"), &missing.join("system.wav"))
        .is_err());
    thread::sleep(Duration::from_millis(600));
    handle.close_chunk().unwrap();
    handle.stop().unwrap();
    join.join().unwrap();
    let m1 = read_wav_samples(&chunk1.join("mic.wav"));
    assert!(m1.len() > 12_000, "chunk 1 kept recording through the failed rotation: {} samples", m1.len());
}
