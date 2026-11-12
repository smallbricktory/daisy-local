// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
use recording::manifest::{AecMode, SessionManifest};
use recording::session::Session;
use tempfile::TempDir;

fn fresh_manifest(id: &str) -> SessionManifest {
    SessionManifest {
        recording_kind: None,
        origin_device: None,
        extra: Default::default(),
        schema_version: SessionManifest::SCHEMA,
        session_id: id.into(),
        created_at_unix_seconds: 1_700_000_000,
        sample_rate: 16_000,
        channels: 1,
        mic_source_id: 1,
        mic_source_node_name: "mic".into(),
        mic_source_description: "Mic".into(),
        system_source_id: 2,
        system_source_node_name: "sys".into(),
        system_source_description: "Sys".into(),
        aec_mode: AecMode::Disabled,
        chunks: vec![],
        finalized_at_unix_seconds: None,
        title: None,
        meeting_id: id.into(),
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
        mixed_clusters: vec![], cluster_sample_spans: vec![], sample_spans_on_audio_timeline: false, speaker_separation: None, diarize_count: None, diarization_id: None,
        interrupted: false,
        denoise_applied: None,
        dismissed_suggested_tag_ids: vec![],
    }
}

#[test]
fn create_then_load_roundtrip() {
    let td = TempDir::new().unwrap();
    let root = td.path().join("session-a");

    let s = Session::create(&root, fresh_manifest("a")).unwrap();
    assert!(root.exists());
    assert!(root.join("manifest.json").exists());
    assert!(root.join("chunks").is_dir());
    drop(s);

    let s2 = Session::load(&root).unwrap();
    assert_eq!(s2.manifest().session_id, "a");
}

#[test]
fn create_refuses_existing_dir() {
    let td = TempDir::new().unwrap();
    let root = td.path().join("session-b");
    Session::create(&root, fresh_manifest("b")).unwrap();
    let err = Session::create(&root, fresh_manifest("b")).unwrap_err();
    assert!(matches!(err, recording::RecordingError::SessionExists(_)));
}

#[test]
fn write_manifest_is_atomic_via_rename() {
    let td = TempDir::new().unwrap();
    let root = td.path().join("session-c");
    let mut s = Session::create(&root, fresh_manifest("c")).unwrap();

    s.update_manifest(|m| m.finalized_at_unix_seconds = Some(42))
        .unwrap();

    // Reload from disk — must reflect change.
    let s2 = Session::load(&root).unwrap();
    assert_eq!(s2.manifest().finalized_at_unix_seconds, Some(42));

    // No tmp leftovers.
    let tmp = root.join("manifest.json.tmp");
    assert!(!tmp.exists());
}

/// The command layer patches `manifest.json` directly to set
/// `title`/`tag_ids`/`meeting_id`/`notes_md_relative` right after
/// `Recorder::start`, then mutates `recording_segments` on pause/resume. The
/// recorder's later persists (chunk open/close, aec_mode at stop) must not
/// clobber those fields; `update_manifest` re-reads disk first.
#[test]
fn update_manifest_preserves_command_layer_patches() {
    let td = TempDir::new().unwrap();
    let root = td.path().join("session-e");
    let mut s = Session::create(&root, fresh_manifest("e")).unwrap();

    // Simulate the command layer patching the on-disk manifest directly,
    // out of band from the in-memory `Session.manifest`.
    {
        let mp = root.join("manifest.json");
        let mut m: SessionManifest =
            serde_json::from_slice(&std::fs::read(&mp).unwrap()).unwrap();
        m.title = Some("Weekly sync".into());
        m.tag_ids = vec!["tag-abc".into(), "tag-def".into()];
        m.meeting_id = "11111111-2222-3333-4444-555555555555".into();
        m.notes_md_relative = Some(std::path::PathBuf::from("notes.md"));
        m.recording_segments.push(recording::manifest::RecordingSegment {
            reason: None,
            extra: Default::default(),
            started_at_unix_seconds: 1_700_000_001,
            stopped_at_unix_seconds: None,
            first_chunk_index: 1,
            last_chunk_index: None,
        });
        std::fs::write(&mp, serde_json::to_vec_pretty(&m).unwrap()).unwrap();
    }

    // Now the recorder persists a field it owns (a new chunk, then aec_mode).
    s.update_manifest(|m| {
        m.chunks.push(recording::manifest::ChunkManifest {
            extra: Default::default(),
            index: 1,
            started_at_unix_seconds: 1_700_000_001,
            ended_at_unix_seconds: None,
            duration_seconds: None,
            mic_wav_relative: std::path::PathBuf::from("chunks/0001/mic.wav"),
            system_wav_relative: std::path::PathBuf::from("chunks/0001/system.wav"),
            mic_aec_wav_relative: None,
            mic_dn_wav_relative: None,
            mic_host_ns: None,
            system_host_ns: None,
            mic_samples: None,
            system_samples: None,
            mic_voice_processed: false,
            mic_zero_samples: None,
            mic_longest_zero_run: None,
            audio_samples: None,
        });
    })
    .unwrap();
    s.update_manifest(|m| m.aec_mode = AecMode::Always).unwrap();

    // Reload from disk: command-layer patches must have survived.
    let reloaded = Session::load(&root).unwrap();
    let m = reloaded.manifest();
    assert_eq!(m.title.as_deref(), Some("Weekly sync"));
    assert_eq!(m.tag_ids, vec!["tag-abc".to_string(), "tag-def".to_string()]);
    assert_eq!(m.meeting_id, "11111111-2222-3333-4444-555555555555");
    assert_eq!(
        m.notes_md_relative,
        Some(std::path::PathBuf::from("notes.md"))
    );
    assert_eq!(m.recording_segments.len(), 1);
    // And the recorder's own fields landed too.
    assert_eq!(m.chunks.len(), 1);
    assert_eq!(m.aec_mode, AecMode::Always);
}

#[test]
fn allocate_chunk_dir_returns_monotonic_paths() {
    let td = TempDir::new().unwrap();
    let root = td.path().join("session-d");
    let mut s = Session::create(&root, fresh_manifest("d")).unwrap();

    let (idx1, path1) = s.allocate_chunk_dir().unwrap();
    // Simulate adding a chunk to the manifest (as the Recorder does).
    s.update_manifest(|m| {
        m.chunks.push(recording::manifest::ChunkManifest {
            extra: Default::default(),
            index: idx1,
            started_at_unix_seconds: 1700000000,
            ended_at_unix_seconds: None,
            duration_seconds: None,
            mic_wav_relative: std::path::PathBuf::from("chunks/0001/mic.wav"),
            system_wav_relative: std::path::PathBuf::from("chunks/0001/system.wav"),
            mic_aec_wav_relative: None,
            mic_dn_wav_relative: None,
            mic_host_ns: None,
            system_host_ns: None,
            mic_samples: None,
            system_samples: None,
            mic_voice_processed: false,
            mic_zero_samples: None,
            mic_longest_zero_run: None,
            audio_samples: None,
        })
    })
    .unwrap();

    let (idx2, path2) = s.allocate_chunk_dir().unwrap();

    assert_eq!(idx1, 1);
    assert_eq!(idx2, 2);
    assert!(path1.ends_with("chunks/0001"));
    assert!(path2.ends_with("chunks/0002"));
    assert!(path1.is_dir());
    assert!(path2.is_dir());
}

#[test]
fn the_next_chunk_follows_the_highest_index() {
    // A manifest whose last row is not its highest index: the next chunk
    // takes a fresh index, never one already in use.
    let td = TempDir::new().unwrap();
    let root = td.path().join("session-e");
    let mut s = Session::create(&root, fresh_manifest("e")).unwrap();
    let chunk = |index: u32| recording::manifest::ChunkManifest {
        extra: Default::default(),
        index,
        started_at_unix_seconds: 1700000000,
        ended_at_unix_seconds: Some(1700000001),
        duration_seconds: Some(1),
        mic_wav_relative: format!("chunks/{index:04}/mic.wav").into(),
        system_wav_relative: format!("chunks/{index:04}/system.wav").into(),
        mic_aec_wav_relative: None,
        mic_dn_wav_relative: None,
        mic_host_ns: None,
        system_host_ns: None,
        mic_samples: None,
        system_samples: None,
        mic_voice_processed: false,
        mic_zero_samples: None,
        mic_longest_zero_run: None,
        audio_samples: None,
    };
    s.update_manifest(|m| m.chunks = vec![chunk(3), chunk(1)]).unwrap();
    let (idx, path) = s.allocate_chunk_dir().unwrap();
    assert_eq!(idx, 4);
    assert!(path.ends_with("chunks/0004"));
    let reloaded = Session::load(&root).unwrap();
    let on_disk: Vec<u32> = reloaded.manifest().chunks.iter().map(|c| c.index).collect();
    assert_eq!(on_disk, vec![1, 3], "written in index order");
}
