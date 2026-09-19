// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
use recording::heartbeat::Heartbeat;
use recording::manifest::{AecMode, ChunkManifest, SessionManifest};
use recording::session::Session;
use std::path::PathBuf;
use tempfile::TempDir;

fn write_dummy_wav(path: &std::path::Path, samples: usize) {
    let pcm: Vec<i16> = (0..samples).map(|i| (i % 1000) as i16).collect();
    transcript::wav_io::write_wav_mono_i16(path, &pcm, 16_000).unwrap();
}

fn fixture_session(td: &TempDir) -> std::path::PathBuf {
    let root = td.path().join("orphan");
    let manifest = SessionManifest {
        recording_kind: None,
        origin_device: None,
        extra: Default::default(),
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
            extra: Default::default(),
            index: 1,
            started_at_unix_seconds: 100,
            ended_at_unix_seconds: None, // simulate crash mid-chunk
            duration_seconds: None,
            mic_wav_relative: PathBuf::from("chunks/0001/mic.wav"),
            system_wav_relative: PathBuf::from("chunks/0001/system.wav"),
            mic_aec_wav_relative: None,
            mic_dn_wav_relative: None,
            mic_host_ns: None,
            system_host_ns: None,
            mic_samples: None,
            system_samples: None,
            mic_voice_processed: false,
            mic_zero_samples: None,
            mic_longest_zero_run: None,
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
        mixed_clusters: vec![], cluster_sample_spans: vec![], speaker_separation: None,
        interrupted: false,
        denoise_applied: None,
        dismissed_suggested_tag_ids: vec![],
    };
    let _ = Session::create(&root, manifest).unwrap();
    let chunk_dir = root.join("chunks/0001");
    std::fs::create_dir_all(&chunk_dir).unwrap();
    write_dummy_wav(&chunk_dir.join("mic.wav"), 16_000); // 1s
    write_dummy_wav(&chunk_dir.join("system.wav"), 16_000);
    root
}

fn read_manifest(root: &std::path::Path) -> SessionManifest {
    serde_json::from_slice(&std::fs::read(root.join("manifest.json")).unwrap()).unwrap()
}

#[test]
fn backfill_fills_chunk_durations_from_wav_without_stamping() {
    let td = TempDir::new().unwrap();
    let root = fixture_session(&td);
    let mut session = Session::load(&root).unwrap();
    recording::manifest_ops::backfill_chunk_durations(&mut session).unwrap();

    let m = read_manifest(&root);
    let c = &m.chunks[0];
    assert_eq!(c.duration_seconds, Some(1));
    assert_eq!(c.ended_at_unix_seconds, Some(101)); // started_at 100 + 1s WAV
    assert!(
        m.finalized_at_unix_seconds.is_none(),
        "backfill must not stamp finalization"
    );
}

#[test]
fn backfill_leaves_known_durations_alone() {
    let td = TempDir::new().unwrap();
    let root = fixture_session(&td);
    let mut session = Session::load(&root).unwrap();
    session
        .update_manifest(|m| {
            m.chunks[0].duration_seconds = Some(7);
            m.chunks[0].ended_at_unix_seconds = Some(107);
        })
        .unwrap();
    recording::manifest_ops::backfill_chunk_durations(&mut session).unwrap();

    let m = read_manifest(&root);
    assert_eq!(m.chunks[0].duration_seconds, Some(7));
    assert_eq!(m.chunks[0].ended_at_unix_seconds, Some(107));
}

#[test]
fn stamp_finalized_stamps_closes_chunks_and_clears_interrupted() {
    let td = TempDir::new().unwrap();
    let root = fixture_session(&td);
    let mut session = Session::load(&root).unwrap();
    // Orphan detection marks interrupted; a successful finalize clears it.
    session.update_manifest(|m| m.interrupted = true).unwrap();
    recording::manifest_ops::stamp_finalized(&mut session, false).unwrap();

    let m = read_manifest(&root);
    assert!(m.finalized_at_unix_seconds.is_some());
    assert!(!m.interrupted, "successful finalize must clear interrupted");
    assert!(m.chunks[0].ended_at_unix_seconds.is_some(), "open chunk closed");
}

#[test]
fn stamp_finalized_keeps_existing_stamp() {
    let td = TempDir::new().unwrap();
    let root = fixture_session(&td);
    let mut session = Session::load(&root).unwrap();
    session
        .update_manifest(|m| m.finalized_at_unix_seconds = Some(4242))
        .unwrap();
    recording::manifest_ops::stamp_finalized(&mut session, false).unwrap();

    let m = read_manifest(&root);
    assert_eq!(m.finalized_at_unix_seconds, Some(4242));
}

#[test]
fn fresh_heartbeat_reads_alive_and_stale_one_does_not() {
    let td = TempDir::new().unwrap();
    let root = fixture_session(&td);
    let hb_path = root.join("heartbeat");
    let _hb = Heartbeat::create(&hb_path).unwrap();
    assert!(Heartbeat::is_alive(&hb_path, 30), "fresh heartbeat is alive");

    // A stale heartbeat (last touch an hour ago) reads dead.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    std::fs::write(&hb_path, format!("{}\n{}\n", std::process::id(), now - 3600)).unwrap();
    assert!(!Heartbeat::is_alive(&hb_path, 30), "stale heartbeat is dead");
}
