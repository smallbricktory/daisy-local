use recording::manifest::{AecMode, ChunkManifest, SessionManifest};

#[test]
fn roundtrip_minimal_manifest() {
    let m = SessionManifest {
        schema_version: SessionManifest::SCHEMA,
        session_id: "test-session-1".into(),
        created_at_unix_seconds: 1_700_000_000,
        sample_rate: 16_000,
        channels: 1,
        mic_source_id: 77,
        mic_source_node_name: "alsa_input.pci-test".into(),
        mic_source_description: "Test Mic".into(),
        system_source_id: 180,
        system_source_node_name: "daisy-capture.monitor".into(),
        system_source_description: "Monitor of Daisy_Capture".into(),
        aec_mode: AecMode::Disabled,
        chunks: vec![],
        finalized_at_unix_seconds: None,
        title: None,
        meeting_id: "meeting-1".into(),
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
    let json = serde_json::to_string_pretty(&m).unwrap();
    let back: SessionManifest = serde_json::from_str(&json).unwrap();
    assert_eq!(m, back);
}

#[test]
fn roundtrip_with_chunks_and_aec() {
    let m = SessionManifest {
        schema_version: SessionManifest::SCHEMA,
        session_id: "x".into(),
        created_at_unix_seconds: 1,
        sample_rate: 16_000,
        channels: 1,
        mic_source_id: 1,
        mic_source_node_name: "n".into(),
        mic_source_description: "d".into(),
        system_source_id: 2,
        system_source_node_name: "n2".into(),
        system_source_description: "d2".into(),
        aec_mode: AecMode::Always,
        chunks: vec![
            ChunkManifest {
                index: 1,
                started_at_unix_seconds: 100,
                ended_at_unix_seconds: Some(110),
                duration_seconds: Some(10),
                mic_wav_relative: "chunks/0001/mic.wav".into(),
                system_wav_relative: "chunks/0001/system.wav".into(),
                mic_aec_wav_relative: Some("chunks/0001/mic_aec.wav".into()),
                mic_dn_wav_relative: None,
                mic_host_ns: None,
                system_host_ns: None,
                mic_samples: None,
                system_samples: None,
                mic_voice_processed: false,
                mic_zero_samples: None,
                mic_longest_zero_run: None,
            },
            ChunkManifest {
                index: 2,
                started_at_unix_seconds: 120,
                ended_at_unix_seconds: None,
                duration_seconds: None,
                mic_wav_relative: "chunks/0002/mic.wav".into(),
                system_wav_relative: "chunks/0002/system.wav".into(),
                mic_aec_wav_relative: None,
                mic_dn_wav_relative: None,
                mic_host_ns: None,
                system_host_ns: None,
                mic_samples: None,
                system_samples: None,
                mic_voice_processed: false,
                mic_zero_samples: None,
                mic_longest_zero_run: None,
            },
        ],
        finalized_at_unix_seconds: Some(200),
        title: None,
        meeting_id: "meeting-2".into(),
        tag_ids: vec!["tag-a".into()],
        notes_md_relative: Some("notes.md".into()),
        attendees: vec![],
        calendar: None,
        recording_segments: vec![recording::manifest::RecordingSegment {
            started_at_unix_seconds: 100,
            stopped_at_unix_seconds: Some(150),
            first_chunk_index: 1,
            last_chunk_index: Some(2),
        }],
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
    let json = serde_json::to_string_pretty(&m).unwrap();
    let back: SessionManifest = serde_json::from_str(&json).unwrap();
    assert_eq!(m, back);
}

#[test]
fn rejects_wrong_schema_version() {
    let bad = r#"{"schema_version":99,"session_id":"x","created_at_unix_seconds":0,"sample_rate":16000,"channels":1,"mic_source_id":0,"mic_source_node_name":"","mic_source_description":"","system_source_id":0,"system_source_node_name":"","system_source_description":"","aec_mode":"disabled","chunks":[],"finalized_at_unix_seconds":null}"#;
    let m: SessionManifest = serde_json::from_str(bad).unwrap();
    assert!(!m.schema_is_supported());
}

#[test]
fn v3_chunk_timing_round_trips() {
    let json = r#"{
      "schema_version": 3, "session_id": "s", "created_at_unix_seconds": 0,
      "sample_rate": 16000, "channels": 1,
      "mic_source_id": 1, "mic_source_node_name": "m", "mic_source_description": "m",
      "system_source_id": 2, "system_source_node_name": "s", "system_source_description": "s",
      "aec_mode": "disabled",
      "chunks": [{
        "index": 1, "started_at_unix_seconds": 0, "ended_at_unix_seconds": 10,
        "duration_seconds": 10,
        "mic_wav_relative": "chunks/0001/mic.wav",
        "system_wav_relative": "chunks/0001/system.wav",
        "mic_aec_wav_relative": null,
        "mic_host_ns": 1000, "system_host_ns": 1200,
        "mic_samples": 160000, "system_samples": 160000
      }], "finalized_at_unix_seconds": null, "meeting_id": "m"
    }"#;
    let m: SessionManifest = serde_json::from_str(json).unwrap();
    let c = &m.chunks[0];
    assert_eq!(c.mic_host_ns, Some(1000));
    assert_eq!(c.system_host_ns, Some(1200));
    assert_eq!(c.mic_samples, Some(160_000));
    assert_eq!(c.system_samples, Some(160_000));

    let back: SessionManifest = serde_json::from_slice(&serde_json::to_vec(&m).unwrap()).unwrap();
    assert_eq!(back.chunks[0].mic_host_ns, Some(1000));
    assert_eq!(back.chunks[0].system_samples, Some(160_000));
}

#[test]
fn a_v2_manifest_reads_with_no_timing() {
    let json = r#"{
      "schema_version": 2, "session_id": "s", "created_at_unix_seconds": 0,
      "sample_rate": 16000, "channels": 1,
      "mic_source_id": 1, "mic_source_node_name": "m", "mic_source_description": "m",
      "system_source_id": 2, "system_source_node_name": "s", "system_source_description": "s",
      "aec_mode": "disabled",
      "chunks": [{
        "index": 1, "started_at_unix_seconds": 0, "ended_at_unix_seconds": 10,
        "duration_seconds": 10,
        "mic_wav_relative": "chunks/0001/mic.wav",
        "system_wav_relative": "chunks/0001/system.wav",
        "mic_aec_wav_relative": null
      }], "finalized_at_unix_seconds": null, "meeting_id": "m"
    }"#;
    let m: SessionManifest = serde_json::from_str(json).unwrap();
    assert_eq!(m.chunks[0].mic_host_ns, None);
    assert_eq!(m.chunks[0].system_host_ns, None);
    assert_eq!(m.chunks[0].mic_samples, None);
    assert_eq!(m.chunks[0].system_samples, None);
}

#[test]
fn chunk_written_before_the_field_existed_loads_as_not_voice_processed() {
    // Pre-migration manifests carry no backend marker; they must read as
    // "software AEC still owes this chunk a pass".
    let json = r#"{
        "index": 0,
        "started_at_unix_seconds": 1700000000,
        "ended_at_unix_seconds": 1700000060,
        "duration_seconds": 60,
        "mic_wav_relative": "chunks/0000/mic.wav",
        "system_wav_relative": "chunks/0000/system.wav",
        "mic_aec_wav_relative": null
    }"#;
    let chunk: ChunkManifest = serde_json::from_str(json).unwrap();
    assert!(!chunk.mic_voice_processed);
    // Same for the zero measurement: absent means unmeasured, not zero
    // zeros — a reader must not report a pre-measurement chunk as healthy.
    assert_eq!(chunk.mic_zero_samples, None);
    assert_eq!(chunk.mic_longest_zero_run, None);
}
