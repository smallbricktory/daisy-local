use recording::heartbeat::Heartbeat;
use recording::manifest::{AecMode, ChunkManifest, SessionManifest};
use recording::session::Session;
use std::path::PathBuf;
use tempfile::TempDir;

fn write_dummy_wav(path: &std::path::Path, samples: usize) {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 16_000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut w = hound::WavWriter::create(path, spec).unwrap();
    for i in 0..samples {
        w.write_sample((i % 1000) as i16).unwrap();
    }
    w.finalize().unwrap();
}

fn fixture_session(td: &TempDir) -> std::path::PathBuf {
    let root = td.path().join("orphan");
    let manifest = SessionManifest {
        schema_version: SessionManifest::SCHEMA,
        session_id: "orphan".into(),
        created_at_unix_seconds: 1,
        sample_rate: 16_000,
        channels: 1,
        mic_source_id: 1,
        mic_source_node_name: "m".into(),
        mic_source_description: "M".into(),
        system_source_id: 2,
        system_source_node_name: "s".into(),
        system_source_description: "S".into(),
        aec_mode: AecMode::Disabled,
        chunks: vec![ChunkManifest {
            index: 1,
            started_at_unix_seconds: 100,
            ended_at_unix_seconds: None, // simulate crash mid-chunk
            duration_seconds: None,
            mic_wav_relative: PathBuf::from("chunks/0001/mic.wav"),
            system_wav_relative: PathBuf::from("chunks/0001/system.wav"),
            mic_aec_wav_relative: None,
            mic_dn_wav_relative: None,
        }],
        finalized_at_unix_seconds: None,
        title: None,
        meeting_id: "orphan-meeting".into(),
        tag_ids: vec![],
        notes_md_relative: None,
        attendees: vec![],
        calendar: None,
        recording_segments: vec![],
        speaker_map: vec![],
        language: None,
        diarization_unavailable: false,
        expected_speakers: None,
        sent_integration_ids: vec![],
        single_local_speaker: true,
        cluster_sides: vec![],
        interrupted: false,
        denoise_applied: None,
    };
    let _ = Session::create(&root, manifest).unwrap();
    let chunk_dir = root.join("chunks/0001");
    std::fs::create_dir_all(&chunk_dir).unwrap();
    write_dummy_wav(&chunk_dir.join("mic.wav"), 16_000); // 1s
    write_dummy_wav(&chunk_dir.join("system.wav"), 16_000);
    root
}

#[test]
fn finalize_orphan_fills_chunk_durations_and_stamps_session() {
    let td = TempDir::new().unwrap();
    let root = fixture_session(&td);
    // No heartbeat → orphan.
    recording::recorder::finalize_orphan(&root, /*heartbeat_max_age_secs=*/ 30).unwrap();

    let m: SessionManifest =
        serde_json::from_slice(&std::fs::read(root.join("manifest.json")).unwrap()).unwrap();
    assert!(m.finalized_at_unix_seconds.is_some());
    let c = &m.chunks[0];
    assert!(c.ended_at_unix_seconds.is_some());
    assert_eq!(c.duration_seconds, Some(1));
}

#[test]
fn finalize_refuses_live_session() {
    let td = TempDir::new().unwrap();
    let root = fixture_session(&td);
    let _hb = Heartbeat::create(&root.join("heartbeat")).unwrap();
    let err = recording::recorder::finalize_orphan(&root, 30).unwrap_err();
    match err {
        recording::RecordingError::SessionStillLive { .. } => {}
        other => panic!("expected SessionStillLive, got {other:?}"),
    }
}
