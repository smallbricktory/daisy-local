//! End-to-end lifecycle integration test for the Recorder facade.
//!
//! Drives the full start → pause → resume → stop cycle through a real PipeWire
//! virtual sink, then asserts manifest correctness and WAV file contents.
//!
//! Run with:
//!   cargo test -p recording --test lifecycle -- --ignored --nocapture
//!
//! AEC mode is derived from detected routing, not passed by the caller; the
//! test inspects what routing the machine reports and asserts consistency.

use audio_engine::source::{list_sources, SourceKind};
use audio_engine::virtual_sink::VirtualSink;
use recording::manifest::{AecMode, SessionManifest};
use recording::{Recorder, RecorderConfig};
use std::path::Path;
use std::thread;
use std::time::Duration;
use tempfile::TempDir;

fn read_wav_samples(p: &Path) -> Vec<i16> {
    let mut r = hound::WavReader::open(p).unwrap();
    r.samples::<i16>().map(|x| x.unwrap()).collect()
}

fn pick_sources(sink_name: &str) -> (audio_engine::source::Source, audio_engine::source::Source, VirtualSink) {
    let vs = VirtualSink::create(sink_name).unwrap();

    // Give PipeWire time to register the new virtual sink globally (same
    // pattern as the audio-engine streaming_lifecycle test).
    thread::sleep(Duration::from_millis(300));

    let all = list_sources().unwrap();
    let mic = all
        .iter()
        .find(|s| s.kind == SourceKind::Mic)
        .cloned()
        .expect("no mic source available");
    let monitor_name = format!("{sink_name}.monitor");
    let monitor = all
        .iter()
        .find(|s| s.node_name == monitor_name)
        .cloned()
        .unwrap_or_else(|| panic!("virtual sink monitor {monitor_name} not found in source list"));
    (mic, monitor, vs)
}

#[test]
#[ignore = "requires PipeWire + pactl + (sometimes) ONNX models; run with --ignored"]
fn full_lifecycle_uses_routing_to_decide_aec() {
    let _ = env_logger::builder().is_test(true).try_init();
    let (mic, monitor, _vs) = pick_sources("daisy-test-rec-lc");
    let td = TempDir::new().unwrap();
    let root = td.path().join("session-lc");

    let mut rec = Recorder::start(RecorderConfig {
        session_root: root.clone(),
        mic_source_id: mic.id,
        mic_source_node_name: mic.node_name.clone(),
        mic_source_description: mic.description.clone(),
        system_source_id: monitor.id,
        system_source_node_name: monitor.node_name.clone(),
        system_source_description: monitor.description.clone(),
        sample_rate: 16_000,
        session_id: "lc".into(),
        live_mode: recording::LiveMode::Off,
        speech_env_min: None,
        flight_recorder: false,
    })
    .unwrap();

    thread::sleep(Duration::from_millis(800));
    rec.pause().unwrap();
    thread::sleep(Duration::from_millis(200));
    rec.resume().unwrap();
    thread::sleep(Duration::from_millis(800));
    let final_root = rec.stop().unwrap();
    assert_eq!(final_root, root);

    // After stop, manifest reflects observed routing.
    let m: SessionManifest =
        serde_json::from_slice(&std::fs::read(root.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(m.chunks.len(), 2, "expected 2 chunks");
    for c in &m.chunks {
        assert!(c.ended_at_unix_seconds.is_some(), "chunk {} not closed", c.index);
        let mic_abs = root.join(&c.mic_wav_relative);
        assert!(read_wav_samples(&mic_abs).len() > 4_000, "mic chunk {} too short", c.index);
    }
    assert!(
        m.finalized_at_unix_seconds.is_none(),
        "stop no longer sets finalized_at; got {:?}",
        m.finalized_at_unix_seconds
    );
    assert!(
        !root.join("heartbeat").exists(),
        "heartbeat should be removed after stop"
    );

    eprintln!("Routing-derived aec_mode after stop: {:?}", m.aec_mode);

    // finalize_orphan completes the post-stop pass (runs AEC iff aec_mode == Always).
    recording::recorder::finalize_orphan(&root, 30).unwrap();

    let m2: SessionManifest =
        serde_json::from_slice(&std::fs::read(root.join("manifest.json")).unwrap()).unwrap();
    assert!(
        m2.finalized_at_unix_seconds.is_some(),
        "finalize_orphan should set finalized_at"
    );

    // If routing said AEC is needed, every chunk has mic_aec.wav.
    if m2.aec_mode == AecMode::Always {
        eprintln!("AEC was Always — checking mic_aec.wav files");
        for c in &m2.chunks {
            let aec_rel = c
                .mic_aec_wav_relative
                .as_ref()
                .unwrap_or_else(|| panic!("AEC was Always — mic_aec_wav_relative should be set for chunk {}", c.index));
            assert!(root.join(aec_rel).is_file(), "missing {}", aec_rel.display());
        }
    } else {
        eprintln!("AEC was Disabled — confirming no mic_aec.wav files");
        for c in &m2.chunks {
            assert!(
                c.mic_aec_wav_relative.is_none(),
                "AEC was Disabled — should be no mic_aec_wav_relative for chunk {}",
                c.index
            );
        }
    }
}
