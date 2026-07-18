use recording::manifest::{AecMode, RecordingSegment, SessionManifest};

fn open_manifest(start: i64) -> SessionManifest {
    SessionManifest {
        schema_version: 2,
        session_id: "s".into(),
        created_at_unix_seconds: start,
        sample_rate: 16000,
        channels: 1,
        mic_source_id: 1,
        mic_source_node_name: "m".into(),
        mic_source_description: "m".into(),
        system_source_id: 2,
        system_source_node_name: "s".into(),
        system_source_description: "s".into(),
        aec_mode: AecMode::Disabled,
        chunks: vec![],
        finalized_at_unix_seconds: None,
        title: None,
        meeting_id: "id".into(),
        tag_ids: vec![],
        notes_md_relative: None,
        attendees: vec![],
        calendar: None,
        recording_segments: vec![RecordingSegment {
            started_at_unix_seconds: start,
            stopped_at_unix_seconds: None,
            first_chunk_index: 1,
            last_chunk_index: None,
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
    }
}

#[test]
fn close_segment_sets_stop_and_last_chunk() {
    let mut m = open_manifest(1000);
    recording::manifest_ops::close_active_segment(&mut m, 2000, 6);
    let seg = &m.recording_segments[0];
    assert_eq!(seg.stopped_at_unix_seconds, Some(2000));
    assert_eq!(seg.last_chunk_index, Some(6));
}

#[test]
fn resume_opens_new_segment_continuing_chunk_index() {
    let mut m = open_manifest(1000);
    recording::manifest_ops::close_active_segment(&mut m, 2000, 6);
    recording::manifest_ops::open_segment(&mut m, 5000);
    assert_eq!(m.recording_segments.len(), 2);
    let seg = &m.recording_segments[1];
    assert_eq!(seg.started_at_unix_seconds, 5000);
    assert_eq!(seg.first_chunk_index, 7);
    assert_eq!(seg.last_chunk_index, None);
    assert_eq!(seg.stopped_at_unix_seconds, None);
}

#[test]
fn open_segment_on_empty_starts_at_one() {
    let mut m = open_manifest(1000);
    m.recording_segments.clear();
    recording::manifest_ops::open_segment(&mut m, 1000);
    assert_eq!(m.recording_segments[0].first_chunk_index, 1);
}
