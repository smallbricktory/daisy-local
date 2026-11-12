// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Importing a file goes through the same band-limited resampler capture uses.

use recording::audio_import::decode_to_mono_16k;
use std::f32::consts::TAU;

fn write_wav(path: &std::path::Path, rate: u32, channels: u16, samples: &[i16]) {
    let spec = hound::WavSpec {
        channels,
        sample_rate: rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut w = hound::WavWriter::create(path, spec).unwrap();
    for &s in samples {
        w.write_sample(s).unwrap();
    }
    w.finalize().unwrap();
}

fn tone(rate: u32, hz: f32, secs: u32) -> Vec<i16> {
    (0..rate * secs)
        .map(|n| ((TAU * hz * n as f32 / rate as f32).sin() * 0.8 * 32_767.0) as i16)
        .collect()
}

fn rms(v: &[i16]) -> f64 {
    (v.iter().map(|&s| (s as f64).powi(2)).sum::<f64>() / v.len().max(1) as f64).sqrt()
}

#[test]
fn a_48k_import_lands_at_16k_with_its_duration_intact() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("speech.wav");
    write_wav(&path, 48_000, 1, &tone(48_000, 440.0, 2));

    let out = decode_to_mono_16k(&path).unwrap();
    // 2 s at 16 kHz, within one 320-sample frame of padding either way.
    assert!(
        (31_680..=32_320).contains(&out.pcm.len()),
        "got {} samples",
        out.pcm.len()
    );
    assert!((out.duration_secs - 2.0).abs() < 0.05, "{}", out.duration_secs);
    assert!(out.quality_ok, "clean tone flagged: {}", out.quality_note);
}

#[test]
fn content_above_the_output_nyquist_is_filtered_not_folded() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hiss.wav");
    // 12 kHz tone at 48 k, above the 16 kHz output's Nyquist.
    write_wav(&path, 48_000, 1, &tone(48_000, 12_000.0, 2));

    let out = decode_to_mono_16k(&path).unwrap();
    let body = &out.pcm[1_600..out.pcm.len() - 1_600];
    assert!(rms(body) < 1_000.0, "aliased energy at {:.0} RMS", rms(body));
}

#[test]
fn stereo_is_downmixed_to_one_channel() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("stereo.wav");
    let mono = tone(16_000, 440.0, 1);
    let mut interleaved = Vec::with_capacity(mono.len() * 2);
    for &s in &mono {
        interleaved.push(s);
        interleaved.push(0); // right channel silent → mono is half amplitude
    }
    write_wav(&path, 16_000, 2, &interleaved);

    let out = decode_to_mono_16k(&path).unwrap();
    assert!(
        (15_680..=16_320).contains(&out.pcm.len()),
        "got {} samples",
        out.pcm.len()
    );
    let expected = rms(&mono) / 2.0;
    assert!(
        (rms(&out.pcm) - expected).abs() < expected * 0.1,
        "downmix level {:.0}, expected ~{expected:.0}",
        rms(&out.pcm)
    );
}

/// Apple's encoder writes 2112 priming samples ahead of the audio and symphonia
/// decodes them; the decoder drops them so the 0.5 s burst lands at 0.5 s.
#[test]
fn aac_priming_is_dropped() {
    let path = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/burst-16k.m4a"));
    let (mono, rate) = recording::audio_import::decode_mono(path).unwrap();
    assert_eq!(rate, 16_000);
    let first = mono.iter().position(|v| v.abs() > 0.1).unwrap();
    assert!((7_950..=8_050).contains(&first), "burst starts at sample {first}");
    assert_eq!(mono.len(), 32_000, "the file's iTunSMPB tag holds 32000 real samples");
}

/// Chunks the Android app writes: a 16 kHz tone from sample 0. A closed file's
/// iTunSMPB tag gives priming 2112, 40037 real samples, and 2907 samples of
/// encoder padding; the decode keeps exactly the real ones. A file copied
/// mid-write has a tag with the priming only, and keeps every decoded sample
/// after it.
#[test]
fn aac_decode_keeps_the_real_samples_the_itunsmpb_tag_counts() {
    for (name, want) in [
        ("android-fragmented.m4a", 40_037),
        ("android-plain.m4a", 40_037),
        ("android-unclosed.m4a", 38_848),
    ] {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name);
        let out = decode_to_mono_16k(&path).unwrap();
        assert_eq!(out.pcm.len(), want, "{name}");
        assert!(out.pcm[..16].iter().any(|&s| s != 0), "{name}: tone starts at sample 0");
    }
}
