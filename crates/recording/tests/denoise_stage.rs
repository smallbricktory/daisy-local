use recording::manifest::{AecMode, ChunkManifest, SessionManifest};
use recording::session::Session;
use std::f32::consts::PI;
use std::path::PathBuf;
use tempfile::TempDir;

/// 2 s of 300 Hz tone + white noise at 16 kHz — something for DFN3 to clean.
fn write_noisy_wav(path: &std::path::Path, samples: usize) {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 16_000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut w = hound::WavWriter::create(path, spec).unwrap();
    let mut seed: u32 = 0x9e37_79b9;
    for i in 0..samples {
        seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
        let noise = ((seed >> 16) as f32 / 32768.0 - 1.0) * 0.25;
        let tone = (2.0 * PI * 300.0 * i as f32 / 16000.0).sin() * 0.4;
        w.write_sample(((tone + noise).clamp(-1.0, 1.0) * 32767.0) as i16)
            .unwrap();
    }
    w.finalize().unwrap();
}

fn fixture_session(td: &TempDir) -> std::path::PathBuf {
    let root = td.path().join("sess");
    let manifest = SessionManifest {
        schema_version: SessionManifest::SCHEMA,
        session_id: "sess".into(),
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
            ended_at_unix_seconds: Some(102),
            duration_seconds: Some(2),
            mic_wav_relative: PathBuf::from("chunks/0001/mic.wav"),
            system_wav_relative: PathBuf::from("chunks/0001/system.wav"),
            mic_aec_wav_relative: None,
            mic_dn_wav_relative: None,
        }],
        finalized_at_unix_seconds: None,
        title: None,
        meeting_id: "m-denoise".into(),
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
    write_noisy_wav(&chunk_dir.join("mic.wav"), 16_000 * 2);
    write_noisy_wav(&chunk_dir.join("system.wav"), 16_000 * 2);
    root
}

#[test]
fn apply_denoise_writes_sidecar_and_stamps_manifest() {
    let td = TempDir::new().unwrap();
    let root = fixture_session(&td);

    assert!(recording::recorder::apply_denoise(&root).unwrap());

    let m = Session::load(&root).unwrap().manifest().clone();
    assert_eq!(m.denoise_applied, Some(true));
    let rel = m.chunks[0]
        .mic_dn_wav_relative
        .as_ref()
        .expect("mic_dn recorded in manifest");
    let dn = root.join(rel);
    assert!(dn.is_file());

    // Output parity: same rate, same length as the input chunk.
    let mut r = hound::WavReader::open(&dn).unwrap();
    assert_eq!(r.spec().sample_rate, 16_000);
    assert_eq!(r.samples::<i16>().count(), 16_000 * 2);

    // Idempotent: second run no-ops without error and keeps the same file.
    let mtime = std::fs::metadata(&dn).unwrap().modified().unwrap();
    assert!(recording::recorder::apply_denoise(&root).unwrap());
    assert_eq!(std::fs::metadata(&dn).unwrap().modified().unwrap(), mtime);
}
