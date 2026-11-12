// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! `wav_io` writes session audio as sealed PCM streams and reads both sealed
//! streams and plaintext WAV files.

use sealfile::{keyring, ContentSecret};
use transcript::wav_io::{read_wav_mono_f32, read_wav_mono_i16, wav_spec, write_wav_mono_i16};

#[test]
fn wav_io_reads_sealed_and_plain() {
    keyring::unlock(ContentSecret::generate().0);
    let td = tempfile::tempdir().unwrap();
    let chunk = td.path().join("sessions").join("s-1").join("chunks").join("0001");
    std::fs::create_dir_all(&chunk).unwrap();
    let samples: Vec<i16> = (0..30_000).map(|i| ((i * 7) % 4_000) as i16 - 2_000).collect();

    let sealed = chunk.join("mic.wav");
    write_wav_mono_i16(&sealed, &samples, 16_000).unwrap();
    assert_eq!(&std::fs::read(&sealed).unwrap()[..4], b"DSS1");
    assert_eq!(read_wav_mono_i16(&sealed).unwrap(), (samples.clone(), 16_000));
    assert_eq!(read_wav_mono_f32(&sealed).unwrap().0.len(), samples.len());
    assert_eq!(wav_spec(&sealed).unwrap(), (16_000, 1));

    // A plaintext WAV an older build wrote in the same session reads as before.
    let plain = chunk.join("system.wav");
    let spec = hound::WavSpec { channels: 1, sample_rate: 16_000, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
    let mut w = hound::WavWriter::create(&plain, spec).unwrap();
    for &s in &samples {
        w.write_sample(s).unwrap();
    }
    w.finalize().unwrap();
    assert_eq!(read_wav_mono_i16(&plain).unwrap(), (samples.clone(), 16_000));

    // Outside a session the writer makes a plain WAV.
    let probe = td.path().join("probe.wav");
    write_wav_mono_i16(&probe, &samples, 16_000).unwrap();
    assert_eq!(hound::WavReader::open(&probe).unwrap().duration() as usize, samples.len());
}

#[test]
fn a_rewrite_replaces_the_file_whole_and_leaves_no_temp() {
    let td = tempfile::tempdir().unwrap();
    let path = td.path().join("mic.wav");
    let first: Vec<i16> = (0..20_000).map(|i| (i % 3_000) as i16).collect();
    write_wav_mono_i16(&path, &first, 16_000).unwrap();
    let before = std::fs::read(&path).unwrap();
    // A second name for the file as it was: a rewrite in place would change it.
    let kept = td.path().join("kept.wav");
    std::fs::hard_link(&path, &kept).unwrap();

    write_wav_mono_i16(&path, &first[..5_000], 16_000).unwrap();
    assert_eq!(std::fs::read(&kept).unwrap(), before, "the old file was never written to");
    assert_eq!(read_wav_mono_i16(&path).unwrap(), (first[..5_000].to_vec(), 16_000));
    let names: Vec<_> = std::fs::read_dir(td.path()).unwrap().flatten().map(|e| e.file_name()).collect();
    assert_eq!(names.len(), 2, "{names:?}");
}
