//! Integration test for the audio sample tees. Mirrors streaming_lifecycle
//! but adds a tee subscriber on each stream and asserts samples arrive.

use audio_engine::capture::{run_dual_streaming, StreamingCaptureRequest};
use audio_engine::source::list_sources;
use audio_engine::virtual_sink::VirtualSink;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use tempfile::TempDir;

#[test]
#[ignore = "requires PipeWire + pactl; run with --ignored"]
fn tees_deliver_samples_during_capture() {
    let _ = env_logger::builder().is_test(true).try_init();

    let _vs = VirtualSink::create("daisy-tee-test").unwrap();
    thread::sleep(Duration::from_millis(300));
    let sources = list_sources().unwrap();
    let mic = sources
        .iter()
        .find(|s| s.kind == audio_engine::source::SourceKind::Mic)
        .unwrap();
    let monitor = sources
        .iter()
        .find(|s| s.node_name == "daisy-tee-test.monitor")
        .unwrap();

    let req = StreamingCaptureRequest {
        mic_source_id: mic.id,
        system_source_id: monitor.id,
        sample_rate: 16_000,
    };

    let td = TempDir::new().unwrap();
    let chunk1 = td.path().join("c1");
    std::fs::create_dir_all(&chunk1).unwrap();

    let handle_slot: Arc<Mutex<Option<audio_engine::capture::StreamingHandle>>> =
        Arc::new(Mutex::new(None));
    let handle_slot_for_thread = Arc::clone(&handle_slot);

    let join = thread::spawn(move || {
        run_dual_streaming(req, move |h| {
            *handle_slot_for_thread.lock().unwrap() = Some(h);
        })
        .unwrap();
    });

    let handle = loop {
        if let Some(h) = handle_slot.lock().unwrap().take() {
            break h;
        }
        thread::sleep(Duration::from_millis(20));
    };

    // Tokio runtime for the mpsc receivers (the audio engine's tees use
    // tokio::sync::mpsc). A current-thread runtime suffices for this
    // synchronous test.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let _g = rt.enter();

    let (mic_tx, mut mic_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<i16>>();
    let (sys_tx, mut sys_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<i16>>();

    handle.set_mic_tee(Some(mic_tx)).unwrap();
    handle.set_system_tee(Some(sys_tx)).unwrap();

    handle
        .open_chunk(&chunk1.join("mic.wav"), &chunk1.join("system.wav"))
        .unwrap();
    thread::sleep(Duration::from_millis(800));
    handle.close_chunk().unwrap();
    handle.set_mic_tee(None).unwrap();
    handle.set_system_tee(None).unwrap();
    handle.stop().unwrap();
    join.join().unwrap();

    // Drain whatever arrived on each tee.
    let mut mic_frames = 0usize;
    while let Ok(_frame) = mic_rx.try_recv() {
        mic_frames += 1;
    }
    let mut sys_frames = 0usize;
    while let Ok(_frame) = sys_rx.try_recv() {
        sys_frames += 1;
    }

    assert!(mic_frames > 0, "expected at least 1 mic frame, got 0");
    assert!(sys_frames > 0, "expected at least 1 system frame, got 0");
    eprintln!(
        "tee delivery: {mic_frames} mic frames, {sys_frames} system frames"
    );
}
