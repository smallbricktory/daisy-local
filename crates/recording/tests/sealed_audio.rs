// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Session audio (capture chunks, the live echo-cancelled mic, the meeting
//! archive) is written as sealed streams and reads back through
//! `sealfile::fs`.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use audio_engine::wav::{WavFrameWriter, FRAME_SAMPLES};
use recording::live_aec_wav::LiveAecWav;
use recording::manifest::ChunkManifest;
use sealfile::{keyring, ContentSecret};
use tempfile::TempDir;

/// The keyring is process-wide; these tests take turns.
fn alone() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// A session folder with chunk folders 1..=3, keyring unlocked.
fn session(td: &TempDir, id: &str) -> PathBuf {
    keyring::unlock(ContentSecret::generate().0);
    let root = td.path().join("sessions").join(id);
    for i in 1..=3 {
        std::fs::create_dir_all(root.join(format!("chunks/{i:04}"))).unwrap();
    }
    root
}

fn samples(path: &Path) -> Vec<i16> {
    hound::WavReader::new(sealfile::fs::open_wav(path).unwrap()).unwrap().samples::<i16>().map(|s| s.unwrap()).collect()
}

fn sealed(path: &Path) -> bool {
    std::fs::read(path).unwrap().starts_with(b"DSS1")
}

fn chunk(index: u32) -> ChunkManifest {
    serde_json::from_value(serde_json::json!({
        "index": index, "started_at_unix_seconds": 1_000,
        "mic_wav_relative": format!("chunks/{index:04}/mic.wav"),
        "system_wav_relative": format!("chunks/{index:04}/system.wav"),
    }))
    .unwrap()
}

#[test]
fn frame_writer_crash_keeps_full_segments() {
    let _g = alone();
    let td = TempDir::new().unwrap();
    let root = session(&td, "s-1");
    let path = root.join("chunks/0001/mic.wav");
    let mut w = WavFrameWriter::create(&path, 16_000).unwrap();
    // 1.7 s of frames, unstamped and written at once (no leading pad).
    for _ in 0..85 {
        w.write_frame_stamped(&[100; FRAME_SAMPLES], 0).unwrap();
    }
    assert_eq!(w.samples_written(), 27_200);
    // The process is killed: no close, no destructor.
    std::mem::forget(w);

    // Three full 16 KiB segments (24,576 samples, 1.536 s) survive.
    assert!(sealed(&path));
    assert_eq!(samples(&path).len(), 24_576);
    assert_eq!(recording::manifest_ops::chunk_wav_samples(&root, &chunk(1)), Some(24_576));
}

#[test]
fn live_aec_overlap_kept_and_gap_padded_sealed() {
    let _g = alone();
    let td = TempDir::new().unwrap();
    let root = session(&td, "s-2");
    let mut w = LiveAecWav::new(root.clone());
    // A pad of 40 samples precedes the first frame; an overlap is kept.
    w.write(1, 40, &[7; 20]).unwrap();
    w.write(1, 50, &[8; 20]).unwrap();
    w.close();
    let path = root.join("chunks/0001/mic_live_aec.wav");
    assert!(sealed(&path));
    let got = samples(&path);
    assert_eq!(got.len(), 70);
    assert_eq!(&got[..40], &[0; 40]);
    assert_eq!(&got[40..60], &[7; 20]);
    assert_eq!(&got[60..], &[8; 10]);
}

#[test]
fn live_aec_reopen_appends_sealed() {
    let _g = alone();
    let td = TempDir::new().unwrap();
    let root = session(&td, "s-3");
    let mut w = LiveAecWav::new(root.clone());
    let first: Vec<i16> = (0..10_000).map(|i| i as i16).collect();
    w.write(1, 0, &first).unwrap();
    w.write(2, 0, &[2; 50]).unwrap();
    // Closed (a pause), then the same chunk's stragglers arrive.
    w.close();
    w.write(1, 10_000, &[3; 10]).unwrap();
    w.close();
    let one = root.join("chunks/0001/mic_live_aec.wav");
    assert!(sealed(&one));
    let got = samples(&one);
    assert_eq!(got.len(), 10_010);
    assert_eq!(&got[..10_000], &first[..]);
    assert_eq!(&got[10_000..], &[3; 10]);
    assert_eq!(samples(&root.join("chunks/0002/mic_live_aec.wav")), vec![2; 50]);
}

#[test]
fn opus_round_trip_sealed() {
    let _g = alone();
    let td = TempDir::new().unwrap();
    let root = session(&td, "s-4");
    let out = root.join("meeting.opus");
    let pcm: Vec<i16> = (0..32_000).map(|i| ((i as f32 * 0.05).sin() * 8_000.0) as i16).collect();
    recording::compress::encode_mono_pcm16(&pcm, 16_000, &Default::default(), &out).unwrap();
    assert!(sealed(&out));
    let (channels, decoded) = recording::compress::decode_opus(&out).unwrap();
    assert_eq!(channels, 1);
    assert!(decoded.len() >= pcm.len());
    assert!(recording::compress::decode_opus_to_ulaw_wav_bytes(&out).unwrap().starts_with(b"RIFF"));
}
