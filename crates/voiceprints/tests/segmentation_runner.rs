// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! The in-tree segmentation runner runs the bundled model window by window.

use std::path::PathBuf;

use voiceprints::segmentation::{segmentation_model_path, SegmentationModel};

/// Synthetic 16 kHz "speech": voiced harmonics with syllable-rate gating, a
/// pitch change and a pause, plus a little deterministic noise. Fictional by
/// construction.
fn clip(seconds: f32) -> Vec<f32> {
    let n = (seconds * 16_000.0) as usize;
    let mut seed = 0x2545_f491u32;
    (0..n)
        .map(|i| {
            let t = i as f32 / 16_000.0;
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let noise = (seed >> 8) as f32 / (1u32 << 24) as f32 - 0.5;
            let f0 = if (t / 4.0) as u32 % 2 == 0 { 120.0 } else { 210.0 };
            let gate = (0.5 + 0.5 * (t * 2.0 * std::f32::consts::PI * 3.0).sin()).powi(2);
            let voiced: f32 =
                (1..6).map(|h| (t * f0 * h as f32 * 2.0 * std::f32::consts::PI).sin() / h as f32).sum();
            let on = if t % 9.0 < 7.0 { 1.0 } else { 0.0 };
            0.3 * gate * on * voiced + 0.01 * noise
        })
        .collect()
}

#[test]
fn the_runner_streams_windows_and_reports_a_missing_model() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../models/voiceprints");
    if dir.join("segmentation-3.0.onnx").is_file() {
        std::env::set_var("DAISY_VOICEPRINT_DIR", &dir);
        assert_eq!(segmentation_model_path(), dir.join("segmentation-3.0.onnx"));

        let audio = clip(14.5);
        let mut ours = SegmentationModel::load_background().unwrap();
        let mut got = 0;
        let n = ours
            .run_logits(&audio, 1.0, |i, logits| {
                assert_eq!(i, got);
                assert!(!logits.is_empty() && logits.iter().all(|v| v.is_finite()));
                got += 1;
            })
            .unwrap();
        // 14.5 s at a 1 s step: at least the windows starting at 0 .. 4 s.
        assert_eq!(n, got);
        assert!(n >= 5, "{n} windows");

        // Under one window: a single padded window.
        let mut windows = 0;
        let n = ours
            .run_windows(&clip(4.0), 1.0, |_, p| {
                windows += 1;
                assert!(!p.is_empty());
            })
            .unwrap();
        assert_eq!((n, windows), (1, 1));
    }

    std::env::set_var("DAISY_VOICEPRINT_DIR", std::env::temp_dir().join("daisy-no-such-voiceprint-dir"));
    match SegmentationModel::load_background() {
        Err(voiceprints::VpError::ModelMissing(m)) => assert!(m.contains("segmentation-3.0.onnx"), "{m}"),
        other => panic!("expected ModelMissing, got {:?}", other.err()),
    }
}
