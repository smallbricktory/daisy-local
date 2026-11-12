// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
use audio_engine::correlation::{best_correlation, cross_correlation_at_lag};

fn sine(n: usize, hz: f32, sr: f32) -> Vec<f32> {
    (0..n)
        .map(|i| (2.0 * std::f32::consts::PI * hz * (i as f32) / sr).sin())
        .collect()
}

#[test]
fn identical_signals_correlate_at_one() {
    let s = sine(16_000, 440.0, 16_000.0);
    let c = cross_correlation_at_lag(&s, &s, 0);
    assert!((c - 1.0).abs() < 1e-3, "expected ~1.0, got {c}");
}

#[test]
fn delayed_signal_peaks_at_correct_lag() {
    let s = sine(16_000, 440.0, 16_000.0);
    let mut delayed = vec![0.0; 100];
    delayed.extend_from_slice(&s[..(s.len() - 100)]);
    let (lag, peak) = best_correlation(&s, &delayed, 200);
    assert_eq!(lag, -100, "expected lag -100 (delayed signal trails), got {lag}");
    assert!(peak > 0.95, "expected peak > 0.95, got {peak}");
}

#[test]
fn unrelated_signals_have_low_correlation() {
    let a = sine(16_000, 440.0, 16_000.0);
    let b = sine(16_000, 1037.0, 16_000.0);
    let (_, peak) = best_correlation(&a, &b, 200);
    assert!(peak.abs() < 0.3, "expected |peak| < 0.3, got {peak}");
}

#[test]
fn lag_search_matches_the_per_lag_coefficient() {
    // Deterministic noise plus a delayed copy, unequal lengths: the search's
    // peak equals the brute-force maximum over `cross_correlation_at_lag`.
    let mut x = 0x2545_f491_u32;
    let mut noise = || {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        (x as f32 / u32::MAX as f32) * 2.0 - 1.0
    };
    let a: Vec<f32> = (0..3_000).map(|_| noise()).collect();
    let mut b: Vec<f32> = vec![0.0; 37];
    b.extend(a.iter().map(|v| 0.6 * v + 0.4 * noise()));
    b.truncate(2_900);
    for search in [0, 5, 60, 3_500] {
        let (lag, peak) = best_correlation(&a, &b, search);
        let mut want = (0i32, 0f32);
        for l in -search..=search {
            let c = cross_correlation_at_lag(&a, &b, l);
            if c.abs() > want.1.abs() {
                want = (-l, c);
            }
        }
        assert_eq!(lag, want.0, "search {search}");
        assert!((peak - want.1).abs() < 1e-4, "search {search}: {peak} vs {}", want.1);
    }
}
