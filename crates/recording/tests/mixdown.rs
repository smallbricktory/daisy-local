use recording::compress::CompressParams;
use recording::manifest::{AecMode, ChunkManifest, SessionManifest};
use recording::mixdown::{build_meeting_audio, MEETING_AUDIO_NAME};
use std::path::PathBuf;

fn write_wav(path: &std::path::Path, samples: &[i16]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 16_000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut w = hound::WavWriter::create(path, spec).unwrap();
    for &s in samples {
        w.write_sample(s).unwrap();
    }
    w.finalize().unwrap();
}

/// Build an in-memory v2 manifest with `n` chunks at `chunks/NNNN/{mic,system}.wav`.
fn manifest_with_chunks(n: u32) -> SessionManifest {
    let mut chunks = Vec::new();
    for i in 1..=n {
        chunks.push(ChunkManifest {
            index: i,
            started_at_unix_seconds: 0,
            ended_at_unix_seconds: Some(1),
            duration_seconds: Some(1),
            mic_wav_relative: PathBuf::from(format!("chunks/{i:04}/mic.wav")),
            system_wav_relative: PathBuf::from(format!("chunks/{i:04}/system.wav")),
            mic_aec_wav_relative: None,
            mic_dn_wav_relative: None,
        });
    }
    SessionManifest {
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
        interrupted: false,
        denoise_applied: None,
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
