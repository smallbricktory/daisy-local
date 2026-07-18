//! Integration test for the loop-driven streaming capture API.
//!
//! Requires a running PipeWire daemon with the PulseAudio shim (`pactl`).
//! Run with:
//!   cargo test -p audio-engine --test streaming_lifecycle -- --ignored --nocapture

use audio_engine::capture::{run_dual_streaming, StreamingCaptureRequest, StreamingHandle};
use audio_engine::source::{list_sources, SourceKind};
use audio_engine::virtual_sink::VirtualSink;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use tempfile::TempDir;

fn read_wav_samples(p: &std::path::Path) -> Vec<i16> {
    let mut r = hound::WavReader::open(p).unwrap();
    r.samples::<i16>().map(|x| x.unwrap()).collect()
}

fn pulse_available() -> bool {
    std::process::Command::new("pactl")
        .arg("info")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
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

    let _vs = VirtualSink::create("daisy-test-1b").unwrap();

    // Give PipeWire time to register the new virtual sink.
    thread::sleep(Duration::from_millis(300));

    let sources = list_sources().unwrap();
    let mic = sources
        .iter()
        .find(|s| s.kind == SourceKind::Mic)
        .expect("no mic source found");
    let monitor = sources
        .iter()
        .find(|s| s.node_name == "daisy-test-1b.monitor")
        .expect("virtual sink monitor not found in source list");

    let req = StreamingCaptureRequest {
        mic_source_id: mic.id,
        system_source_id: monitor.id,
        sample_rate: 16_000,
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
        .open_chunk(&chunk1.join("mic.wav"), &chunk1.join("system.wav"))
        .unwrap();
    thread::sleep(Duration::from_millis(800));
    handle.close_chunk().unwrap();

    // Brief gap — audio between chunks should be silently discarded.
    thread::sleep(Duration::from_millis(50));

    // Chunk 2
    handle
        .open_chunk(&chunk2.join("mic.wav"), &chunk2.join("system.wav"))
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
