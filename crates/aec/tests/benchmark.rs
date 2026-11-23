// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Realtime-factor benchmark. Asserts AEC processes at ≥ 1.5× realtime in
//! release builds.

use aec::AcousticEchoCanceller;
use std::time::Instant;

const FRAME_SIZE: usize = AcousticEchoCanceller::FRAME_SIZE;

#[test]
fn aec_runs_at_or_above_1_5x_realtime() {
    let mut aec = AcousticEchoCanceller::new().expect("new");
    let sr = 16_000usize;
    let secs = 30usize;
    let n = sr * secs;
    let near: Vec<i16> = (0..n).map(|i| (((i as f32) * 0.001).sin() * 1000.0) as i16).collect();
    let far: Vec<i16> = (0..n).map(|i| (((i as f32) * 0.0007).sin() * 1500.0) as i16).collect();

    // Warm-up frame.
    aec.process(&near[..FRAME_SIZE], &far[..FRAME_SIZE]).unwrap();

    let t0 = Instant::now();
    let mut i = FRAME_SIZE;
    while i + FRAME_SIZE <= n {
        aec.process(&near[i..i + FRAME_SIZE], &far[i..i + FRAME_SIZE]).unwrap();
        i += FRAME_SIZE;
    }
    let elapsed = t0.elapsed().as_secs_f32();
    let realtime_factor = secs as f32 / elapsed;
    eprintln!("processed {secs} s of audio in {elapsed:.2} s ({realtime_factor:.2}× realtime)");

    // Debug builds accept any positive factor; release builds must hit ≥ 1.5×.
    let target = if cfg!(debug_assertions) { 0.05 } else { 1.5 };
    assert!(
        realtime_factor >= target,
        "realtime factor too low: {realtime_factor:.2} (must be ≥ {target})"
    );
}
