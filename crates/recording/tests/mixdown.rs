// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
use recording::compress::CompressParams;
use recording::manifest::{AecMode, ChunkManifest, SessionManifest};
use recording::mixdown::{build_meeting_audio, MEETING_AUDIO_NAME};
use std::path::PathBuf;

fn write_wav(path: &std::path::Path, samples: &[i16]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    transcript::wav_io::write_wav_mono_i16(path, samples, 16_000).unwrap();
}

/// Build an in-memory v2 manifest with `n` chunks at `chunks/NNNN/{mic,system}.wav`.
fn manifest_with_chunks(n: u32) -> SessionManifest {
    let mut chunks = Vec::new();
    for i in 1..=n {
        chunks.push(ChunkManifest {
            extra: Default::default(),
            index: i,
            started_at_unix_seconds: 0,
            ended_at_unix_seconds: Some(1),
            duration_seconds: Some(1),
            mic_wav_relative: PathBuf::from(format!("chunks/{i:04}/mic.wav")),
            system_wav_relative: PathBuf::from(format!("chunks/{i:04}/system.wav")),
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
    }
    SessionManifest {
        recording_kind: None,
        origin_device: None,
        extra: Default::default(),
        schema_version: 2,
        session_id: "sess".into(),
        created_at_unix_seconds: 0,
        sample_rate: 16_000,
        channels: 1,
        mic_source_id: 1,
        mic_source_node_name: "m".into(),
        mic_source_description: "m".into(),
        system_source_id: 2,
        system_source_node_name: "s".into(),
        system_source_description: "s".into(),
        aec_mode: AecMode::Always,
        chunks,
        finalized_at_unix_seconds: None,
        title: None,
        meeting_id: "mid".into(),
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
fn mixes_two_chunks_into_one_opus() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let m = manifest_with_chunks(2);
    for c in &m.chunks {
        write_wav(&root.join(&c.mic_wav_relative), &vec![4000i16; 8000]);
        write_wav(&root.join(&c.system_wav_relative), &vec![-3000i16; 8000]);
    }
    let n = build_meeting_audio(root, &m, &CompressParams::default()).unwrap();
    assert!(n > 0);
    let out = root.join(MEETING_AUDIO_NAME);
    assert!(out.is_file());
    assert_eq!(std::fs::metadata(&out).unwrap().len(), n);
    // Sanity: file starts with the Ogg capture pattern.
    let head = std::fs::read(&out).unwrap();
    assert!(head.starts_with(b"OggS"), "expected OggS magic");
}

#[test]
fn errors_when_no_chunk_audio_present() {
    let dir = tempfile::tempdir().unwrap();
    let m = manifest_with_chunks(2); // WAVs never written
    assert!(build_meeting_audio(dir.path(), &m, &CompressParams::default()).is_err());
}

fn tone(freq: f32, n: usize, amp: f32) -> Vec<i16> {
    (0..n)
        .map(|i| ((2.0 * std::f32::consts::PI * freq * i as f32 / 16_000.0).sin() * amp) as i16)
        .collect()
}

fn rms(s: &[i16]) -> f64 {
    (s.iter().map(|&v| f64::from(v) * f64::from(v)).sum::<f64>() / s.len().max(1) as f64).sqrt()
}

#[test]
fn mic_only_manifest_yields_mono_opus_that_round_trips_to_the_mic_track() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let mut m = manifest_with_chunks(2);
    m.recording_kind = Some("phone_room".into());
    for c in &mut m.chunks {
        c.system_wav_relative = PathBuf::new();
        write_wav(&root.join(&c.mic_wav_relative), &tone(440.0, 16_000, 6000.0));
    }
    build_meeting_audio(root, &m, &CompressParams::default()).unwrap();

    let (channels, pcm) = recording::compress::decode_opus(&root.join(MEETING_AUDIO_NAME)).unwrap();
    assert_eq!(channels, 1);
    assert!(pcm.len() >= 30_000, "both chunks present: {}", pcm.len());
    assert!(rms(&pcm) > 1000.0, "mic audio survives the round trip");
    let (mic, _) = recording::compress::deinterleave_stereo(channels, &pcm);
    assert_eq!(mic, pcm);
}

#[test]
fn session_with_a_system_track_stays_stereo_with_both_channels_carrying_audio() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let m = manifest_with_chunks(1);
    write_wav(&root.join(&m.chunks[0].mic_wav_relative), &tone(220.0, 16_000, 6000.0));
    write_wav(&root.join(&m.chunks[0].system_wav_relative), &tone(660.0, 16_000, 6000.0));
    build_meeting_audio(root, &m, &CompressParams::default()).unwrap();

    let (channels, pcm) = recording::compress::decode_opus(&root.join(MEETING_AUDIO_NAME)).unwrap();
    assert_eq!(channels, 2);
    let (l, r) = recording::compress::deinterleave_stereo(channels, &pcm);
    assert!(rms(&l) > 1000.0 && rms(&r) > 1000.0);
}

