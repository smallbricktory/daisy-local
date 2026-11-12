// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Session text files under `sessions/<id>/` are sealed on disk, and the
//! helpers that read them see plaintext.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use recording::manifest::{AecMode, ChunkManifest, RecordingSegment, SessionManifest};
use recording::session::Session;
use sealfile::{keyring, ContentSecret};
use tempfile::TempDir;

/// The keyring is process-wide; these tests take turns.
fn alone() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

fn manifest(id: &str) -> SessionManifest {
    SessionManifest {
        recording_kind: None,
        origin_device: None,
        extra: Default::default(),
        schema_version: SessionManifest::SCHEMA,
        session_id: id.into(),
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
            ended_at_unix_seconds: None,
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
            audio_samples: None,
        }],
        finalized_at_unix_seconds: None,
        title: None,
        meeting_id: "fictional-meeting".into(),
        tag_ids: vec![],
        notes_md_relative: None,
        attendees: vec![],
        calendar: None,
        recording_segments: vec![RecordingSegment {
            started_at_unix_seconds: 100,
            stopped_at_unix_seconds: None,
            first_chunk_index: 1,
            last_chunk_index: None,
            reason: None,
            extra: Default::default(),
        }],
        speaker_map: vec![],
        language: None,
        diarization_unavailable: false,
        expected_speakers: None,
        sent_integration_ids: vec![],
        single_local_speaker: true,
        cluster_sides: vec![],
        mixed_clusters: vec![],
        cluster_sample_spans: vec![],
        sample_spans_on_audio_timeline: false,
        speaker_separation: None,
        diarize_count: None,
        diarization_id: None,
        interrupted: false,
        denoise_applied: None,
        dismissed_suggested_tag_ids: vec![],
    }
}

fn on_disk(path: &Path) -> Vec<u8> {
    std::fs::read(path).unwrap()
}

fn mtime(path: &Path) -> std::time::SystemTime {
    std::fs::metadata(path).unwrap().modified().unwrap()
}

#[test]
fn session_manifest_is_sealed_on_disk() {
    let _g = alone();
    keyring::unlock(ContentSecret::generate().0);
    let td = TempDir::new().unwrap();
    let root = td.path().join("sessions").join("s-1");
    Session::create(&root, manifest("s-1")).unwrap();
    assert_eq!(&on_disk(&root.join("manifest.json"))[..4], b"DSS1");
    assert_eq!(Session::load(&root).unwrap().manifest().session_id, "s-1");
}

#[test]
fn update_manifest_skips_unchanged_plaintext() {
    let _g = alone();
    keyring::unlock(ContentSecret::generate().0);
    let td = TempDir::new().unwrap();
    let root = td.path().join("sessions").join("s-2");
    let mut session = Session::create(&root, manifest("s-2")).unwrap();
    let path = root.join("manifest.json");
    let before = (on_disk(&path), mtime(&path));
    std::thread::sleep(std::time::Duration::from_millis(20));

    session.update_manifest(|m| m.title = None).unwrap();
    assert_eq!((on_disk(&path), mtime(&path)), before, "an unchanged manifest is not rewritten");

    session.update_manifest(|m| m.title = Some("Planning".into())).unwrap();
    assert_ne!(on_disk(&path), before.0);
    assert_eq!(Session::load(&root).unwrap().manifest().title.as_deref(), Some("Planning"));
}

#[test]
fn abandoned_locked_recording_finalizes_after_unlock() {
    let _g = alone();
    let (secret, _) = ContentSecret::generate();
    let secret_bytes = secret.to_bytes();
    keyring::unlock(secret);
    keyring::lock();
    assert!(!keyring::is_unlocked());

    // A recording starts while the vault is locked.
    let td = TempDir::new().unwrap();
    let root = td.path().join("sessions").join("s-3");
    let pin = keyring::pin(&root);
    Session::create(&root, manifest("s-3")).unwrap();
    std::fs::create_dir_all(root.join("chunks/0001")).unwrap();
    let pcm = vec![0i16; 16_000];
    transcript::wav_io::write_wav_mono_i16(&root.join("chunks/0001/mic.wav"), &pcm, 16_000).unwrap();
    assert_eq!(&on_disk(&root.join("manifest.json"))[..4], b"DSS1");
    assert!(root.join(keyring::KEY_FILE).is_file());

    // The process ends before an unlock: the pinned key is gone.
    drop(pin);
    assert!(Session::load(&root).is_err(), "a locked vault does not open the session");

    // The next unlock finishes the session from disk.
    keyring::unlock(ContentSecret::from_bytes(*secret_bytes));
    let mut session = Session::load(&root).unwrap();
    assert!(recording::manifest_ops::close_abandoned(&mut session, None).unwrap());
    let m = Session::load(&root).unwrap().manifest().clone();
    assert!(m.interrupted);
    assert_eq!(m.chunks[0].ended_at_unix_seconds, Some(101));
    assert_eq!(m.recording_segments[0].stopped_at_unix_seconds, Some(101));
}

#[test]
fn holds_no_recording_reads_a_sealed_empty_file_as_empty() {
    let _g = alone();
    keyring::unlock(ContentSecret::generate().0);
    let td = TempDir::new().unwrap();
    let root = td.path().join("sessions").join("s-4");
    std::fs::create_dir_all(root.join("chunks/0001")).unwrap();
    sealfile::fs::write(root.join("manifest.json"), b"{}").unwrap();
    sealfile::fs::write(root.join("chunks/0001/notes.json"), b"").unwrap();
    assert!(recording::manifest_ops::holds_no_recording(&root), "key.json and an empty sealed file are no recording");

    sealfile::fs::write(root.join("notes.json"), b"{\"md\":\"agenda\"}").unwrap();
    assert!(!recording::manifest_ops::holds_no_recording(&root));
}

#[test]
fn a_torn_sealed_chunk_is_closed_and_finalized_at_its_last_full_segment() {
    use audio_engine::wav::{WavFrameWriter, FRAME_SAMPLES};
    let _g = alone();
    keyring::unlock(ContentSecret::generate().0);
    let td = TempDir::new().unwrap();
    let root = td.path().join("sessions").join("s-5");
    Session::create(&root, manifest("s-5")).unwrap();
    std::fs::create_dir_all(root.join("chunks/0001")).unwrap();
    let mut w = WavFrameWriter::create(&root.join("chunks/0001/mic.wav"), 16_000).unwrap();
    for _ in 0..85 {
        w.write_frame_stamped(&[100; FRAME_SAMPLES], 0).unwrap();
    }
    // The process is killed: no close, no destructor.
    std::mem::forget(w);

    let mut session = Session::load(&root).unwrap();
    assert!(recording::manifest_ops::close_abandoned(&mut session, None).unwrap());
    recording::manifest_ops::stamp_finalized(&mut session, true).unwrap();
    let m = Session::load(&root).unwrap().manifest().clone();
    // Three full segments (24,576 samples, 1.536 s) survive: two seconds
    // to the nearest.
    assert_eq!(m.chunks[0].ended_at_unix_seconds, Some(102));
    assert_eq!(m.recording_segments[0].stopped_at_unix_seconds, Some(102));
    assert!(m.finalized_at_unix_seconds.is_some());
    assert!(m.interrupted);
    assert_eq!(recording::manifest_ops::chunk_wav_samples(&root, &m.chunks[0]), Some(24_576));
}
