// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Parakeet decode-core contract test. Model-gated: skips unless
//! `DAISY_PARAKEET_TEST_MODEL_DIR` points at a split-file model dir. With
//! `DAISY_PARAKEET_WAV` also set (16 kHz mono speech), asserts words come out.
//!
//!   DAISY_PARAKEET_TEST_MODEL_DIR=…/parakeet-tdt-0.6b-v2 \
//!   DAISY_PARAKEET_WAV=…/mic.wav \
//!   cargo test -p providers-local --test parakeet_contract -- --nocapture

use providers_http::Transcriber;
use providers_local::parakeet::batch::ParakeetLocalTranscriber;
use providers_local::parakeet::realtime::ParakeetTune;
use providers_local::parakeet::{ParakeetASR, ParakeetOpts};
use std::path::PathBuf;

#[test]
fn decode_contract_tone_is_silent_speech_is_not() {
    let Ok(model_dir) = std::env::var("DAISY_PARAKEET_TEST_MODEL_DIR") else {
        eprintln!("skip: DAISY_PARAKEET_TEST_MODEL_DIR not set");
        return;
    };
    let mut asr =
        ParakeetASR::new(&PathBuf::from(&model_dir), &ParakeetOpts::default()).expect("load model");

    // 2 s of 440 Hz tone: no speech → no words.
    let tone: Vec<f32> = (0..32_000)
        .map(|i| 0.3 * (2.0 * std::f32::consts::PI * 440.0 * i as f32 / 16_000.0).sin())
        .collect();
    let words = asr.decode(&tone).expect("tone decode");
    assert!(words.is_empty(), "pure tone must decode to no words, got {words:?}");

    let Ok(wav) = std::env::var("DAISY_PARAKEET_WAV") else {
        eprintln!("skip speech half: DAISY_PARAKEET_WAV not set");
        return;
    };
    let mut reader = hound::WavReader::open(&wav).expect("open wav");
    assert_eq!(reader.spec().sample_rate, 16_000, "expect 16k mono wav");
    let samples: Vec<f32> = reader
        .samples::<i16>()
        .map(|s| s.unwrap() as f32 / 32768.0)
        .collect();
    let take = samples.len().min(30 * 16_000);
    let words = asr.decode(&samples[..take]).expect("speech decode");
    println!(
        "speech words: {}",
        words.iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" ")
    );
    assert!(!words.is_empty(), "speech wav must produce words");
    // Spans are window-relative, ordered, and non-degenerate.
    for pair in words.windows(2) {
        assert!(pair[1].start_ms >= pair[0].start_ms, "word starts must be ordered");
    }
    assert!(words.iter().all(|w| w.end_ms >= w.start_ms));
}

#[test]
fn batch_transcriber_produces_segments() {
    let (Ok(model_dir), Ok(wav)) = (
        std::env::var("DAISY_PARAKEET_TEST_MODEL_DIR"),
        std::env::var("DAISY_PARAKEET_WAV"),
    ) else {
        eprintln!("skip: DAISY_PARAKEET_TEST_MODEL_DIR / DAISY_PARAKEET_WAV not set");
        return;
    };
    let t = ParakeetLocalTranscriber::new(
        PathBuf::from(model_dir),
        ParakeetTune::default(),
        "parakeet batch test",
    );
    let segs = t
        .transcribe(std::path::Path::new(&wav), Some("en"))
        .expect("batch transcribe");
    assert!(!segs.is_empty(), "speech wav must yield segments");
    let words: usize = segs.iter().map(|s| s.text.split_whitespace().count()).sum();
    println!("batch transcriber: {} segment(s), {words} words", segs.len());
    assert!(words >= 10, "expected sentence-scale output, got {words} words");
    for pair in segs.windows(2) {
        assert!(pair[1].start_ms >= pair[0].start_ms, "segments ordered");
    }
}
