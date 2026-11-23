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
            audio_samples: None,
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
        mixed_clusters: vec![], cluster_sample_spans: vec![], sample_spans_on_audio_timeline: false, speaker_separation: None, diarize_count: None, diarization_id: None,
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
fn chunk_audio_is_stamped_from_the_wavs_without_finalizing() {
    let td = TempDir::new().unwrap();
    let root = fixture_session(&td);
    let mut session = Session::load(&root).unwrap();
    recording::manifest_ops::stamp_chunk_audio(&mut session).unwrap();

    let m = read_manifest(&root);
    let c = &m.chunks[0];
    assert_eq!(c.audio_samples, Some(16_000));
    assert_eq!(c.duration_seconds, Some(1));
    assert_eq!(c.ended_at_unix_seconds, Some(101)); // started_at 100 + 1s WAV
    assert!(
        m.finalized_at_unix_seconds.is_none(),
        "stamping must not stamp finalization"
    );
}

#[test]
fn a_duration_within_a_second_of_the_audio_stands() {
    let td = TempDir::new().unwrap();
    let root = fixture_session(&td);
    let mut session = Session::load(&root).unwrap();
    session
        .update_manifest(|m| {
            m.chunks[0].duration_seconds = Some(2);
            m.chunks[0].ended_at_unix_seconds = Some(102);
        })
        .unwrap();
    recording::manifest_ops::stamp_chunk_audio(&mut session).unwrap();

    let m = read_manifest(&root);
    assert_eq!(m.chunks[0].duration_seconds, Some(2));
    assert_eq!(m.chunks[0].ended_at_unix_seconds, Some(102));
}

#[test]
fn a_wall_clock_duration_that_took_in_a_gap_comes_down_to_the_audio() {
    // The chunk's wall span ran 7 s; 1 s of audio reached the file.
    let td = TempDir::new().unwrap();
    let root = fixture_session(&td);
    let mut session = Session::load(&root).unwrap();
    session
        .update_manifest(|m| {
            m.chunks[0].duration_seconds = Some(7);
            m.chunks[0].ended_at_unix_seconds = Some(107);
        })
        .unwrap();
    recording::manifest_ops::stamp_chunk_audio(&mut session).unwrap();

    let m = read_manifest(&root);
    assert_eq!(m.chunks[0].duration_seconds, Some(1));
    assert_eq!(m.chunks[0].ended_at_unix_seconds, Some(107), "the wall stamp stays a wall stamp");
}

#[test]
fn the_longer_track_is_the_chunk_length() {
    let td = TempDir::new().unwrap();
    let root = fixture_session(&td);
    write_dummy_wav(&root.join("chunks/0001/system.wav"), 24_000);
    let mut session = Session::load(&root).unwrap();
    recording::manifest_ops::stamp_chunk_audio(&mut session).unwrap();
    assert_eq!(read_manifest(&root).chunks[0].audio_samples, Some(24_000));
}

#[test]
fn a_chunk_with_no_audio_on_disk_keeps_what_it_has() {
    // Its WAVs are still syncing, or were pruned: nothing measures it.
    let td = TempDir::new().unwrap();
    let root = fixture_session(&td);
    std::fs::remove_dir_all(root.join("chunks/0001")).unwrap();
    let mut session = Session::load(&root).unwrap();
    session.update_manifest(|m| m.chunks[0].duration_seconds = Some(7)).unwrap();
    assert!(!recording::manifest_ops::chunk_audio_on_disk(&root, session.manifest()));
    recording::manifest_ops::stamp_chunk_audio(&mut session).unwrap();
    let c = &read_manifest(&root).chunks[0];
    assert_eq!(c.audio_samples, None);
    assert_eq!(c.duration_seconds, Some(7));
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

fn open_segment(m: &mut SessionManifest, started: i64, first_chunk: u32) {
    m.recording_segments.push(recording::manifest::RecordingSegment {
        reason: None,
        extra: Default::default(),
        started_at_unix_seconds: started,
        stopped_at_unix_seconds: None,
        first_chunk_index: first_chunk,
        last_chunk_index: None,
    });
}

#[test]
fn close_abandoned_ends_the_open_chunk_and_segment_at_the_readable_audio() {
    let td = TempDir::new().unwrap();
    let root = fixture_session(&td);
    let mut session = Session::load(&root).unwrap();
    session.update_manifest(|m| open_segment(m, 100, 1)).unwrap();

    assert!(recording::manifest_ops::close_abandoned(&mut session, Some(5_000)).unwrap());

    let m = read_manifest(&root);
    assert_eq!(m.chunks[0].ended_at_unix_seconds, Some(101), "start 100 + 1 s of mic audio");
    assert_eq!(m.chunks[0].duration_seconds, Some(1));
    assert_eq!(m.chunks[0].audio_samples, Some(16_000), "the audio length is stamped too");
    let seg = &m.recording_segments[0];
    assert_eq!(seg.stopped_at_unix_seconds, Some(101), "the segment ends with its last chunk");
    assert_eq!(seg.last_chunk_index, Some(1));
    assert!(m.interrupted);
    assert!(!recording::manifest_ops::has_open_segment(&m));
}

#[test]
fn close_abandoned_falls_back_to_file_mtime_when_no_track_is_readable() {
    let td = TempDir::new().unwrap();
    let root = fixture_session(&td);
    // A header-less, zero-byte track: what a create that failed after the
    // file appeared leaves behind.
    std::fs::write(root.join("chunks/0001/mic.wav"), b"").unwrap();
    std::fs::write(root.join("chunks/0001/system.wav"), b"").unwrap();
    let mut session = Session::load(&root).unwrap();
    session.update_manifest(|m| open_segment(m, 100, 1)).unwrap();

    recording::manifest_ops::close_abandoned(&mut session, None).unwrap();

    let m = read_manifest(&root);
    let mtime = std::fs::metadata(root.join("chunks/0001/mic.wav"))
        .unwrap()
        .modified()
        .unwrap()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    assert_eq!(m.chunks[0].ended_at_unix_seconds, Some(mtime));
    assert_eq!(m.recording_segments[0].stopped_at_unix_seconds, Some(mtime));
}

#[test]
fn close_abandoned_stops_a_chunkless_segment_at_the_last_heartbeat() {
    let td = TempDir::new().unwrap();
    let root = fixture_session(&td);
    let mut session = Session::load(&root).unwrap();
    session
        .update_manifest(|m| {
            m.chunks.clear();
            open_segment(m, 1_790_887_989, 1);
        })
        .unwrap();

    recording::manifest_ops::close_abandoned(&mut session, Some(1_790_887_995)).unwrap();
    let m = read_manifest(&root);
    assert_eq!(m.recording_segments[0].stopped_at_unix_seconds, Some(1_790_887_995));
    assert_eq!(m.recording_segments[0].last_chunk_index, None);

    // A heartbeat older than the segment never moves the stop before its start.
    let mut session = Session::load(&root).unwrap();
    session
        .update_manifest(|m| {
            m.recording_segments.clear();
            open_segment(m, 1_790_887_989, 1);
        })
        .unwrap();
    recording::manifest_ops::close_abandoned(&mut session, Some(1)).unwrap();
    assert_eq!(read_manifest(&root).recording_segments[0].stopped_at_unix_seconds, Some(1_790_887_989));
}

#[test]
fn close_abandoned_leaves_a_closed_session_alone() {
    let td = TempDir::new().unwrap();
    let root = fixture_session(&td);
    let mut session = Session::load(&root).unwrap();
    session
        .update_manifest(|m| {
            m.chunks[0].ended_at_unix_seconds = Some(160);
            m.chunks[0].duration_seconds = Some(60);
            open_segment(m, 100, 1);
            m.recording_segments[0].stopped_at_unix_seconds = Some(160);
        })
        .unwrap();
    assert!(!recording::manifest_ops::close_abandoned(&mut session, None).unwrap());
    assert!(!read_manifest(&root).interrupted);
}

/// A start that never opened its first chunk: manifest, heartbeat, a mic
/// file holding a bare header and an empty system file.
fn empty_start(td: &TempDir) -> std::path::PathBuf {
    let root = td.path().join("empty-start");
    std::fs::create_dir_all(root.join("chunks/0001")).unwrap();
    std::fs::write(root.join("manifest.json"), b"{}").unwrap();
    std::fs::write(root.join("heartbeat"), b"1\n2\nhost\n").unwrap();
    std::fs::write(root.join("metrics.jsonl"), b"{\"event\":\"session\"}\n").unwrap();
    write_dummy_wav(&root.join("chunks/0001/mic.wav"), 0);
    std::fs::write(root.join("chunks/0001/system.wav"), b"").unwrap();
    root
}

#[test]
fn holds_no_recording_accepts_only_an_empty_start() {
    let td = TempDir::new().unwrap();
    let root = empty_start(&td);
    assert!(recording::manifest_ops::holds_no_recording(&root));

    // One second of audio is a recording.
    write_dummy_wav(&root.join("chunks/0001/system.wav"), 16_000);
    assert!(!recording::manifest_ops::holds_no_recording(&root));
}

#[test]
fn holds_no_recording_refuses_anything_it_does_not_know() {
    let cases: [(&str, fn(&std::path::Path)); 5] = [
        ("notes", |r| std::fs::write(r.join("notes.json"), b"{\"md\":\"agenda\"}").unwrap()),
        ("unparseable track", |r| std::fs::write(r.join("chunks/0001/mic.wav"), b"RIFF").unwrap()),
        ("a second level", |r| std::fs::create_dir_all(r.join("chunks/0001/deeper")).unwrap()),
        ("an unknown dir", |r| std::fs::create_dir_all(r.join("speakers")).unwrap()),
        ("a stray file in chunks", |r| std::fs::write(r.join("chunks/readme"), b"").unwrap()),
    ];
    for (what, add) in cases {
        let td = TempDir::new().unwrap();
        let root = empty_start(&td);
        add(&root);
        assert!(!recording::manifest_ops::holds_no_recording(&root), "{what}");
    }
    // An empty file of any name is no recording.
    let td = TempDir::new().unwrap();
    let root = empty_start(&td);
    std::fs::write(root.join("live_transcript.jsonl"), b"").unwrap();
    assert!(recording::manifest_ops::holds_no_recording(&root));
}

#[cfg(unix)]
#[test]
fn holds_no_recording_refuses_symlinks() {
    let td = TempDir::new().unwrap();
    let root = empty_start(&td);
    let outside = td.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::os::unix::fs::symlink(&outside, root.join("chunks/0002")).unwrap();
    assert!(!recording::manifest_ops::holds_no_recording(&root), "a linked chunk dir");

    let td = TempDir::new().unwrap();
    let root = empty_start(&td);
    let link = td.path().join("link");
    std::os::unix::fs::symlink(&root, &link).unwrap();
    assert!(!recording::manifest_ops::holds_no_recording(&link), "a linked session dir");
}
