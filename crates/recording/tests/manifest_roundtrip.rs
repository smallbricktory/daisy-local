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
