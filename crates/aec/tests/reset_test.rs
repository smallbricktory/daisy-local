// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! `reset()` restores a canceller to exactly its freshly-built behavior.

use aec::AcousticEchoCanceller;

const FRAME_SIZE: usize = AcousticEchoCanceller::FRAME_SIZE;

fn process_all(aec: &mut AcousticEchoCanceller, near: &[i16], far: &[i16]) -> Vec<i16> {
    let mut out = Vec::new();
    let mut i = 0;
    while i + FRAME_SIZE <= near.len() {
        out.extend(aec.process(&near[i..i + FRAME_SIZE], &far[i..i + FRAME_SIZE]).unwrap());
        i += FRAME_SIZE;
    }
    out
}

#[test]
fn reset_restores_fresh_output() {
    let n = FRAME_SIZE * 200;
    let near: Vec<i16> = (0..n).map(|i| (((i as f32) * 0.001).sin() * 1000.0) as i16).collect();
    let far: Vec<i16> = (0..n).map(|i| (((i as f32) * 0.0007).sin() * 1500.0) as i16).collect();
    // A different signal used only to dirty the reused canceller's state.
    let near2: Vec<i16> = (0..n).map(|i| (((i as f32) * 0.003).sin() * 800.0) as i16).collect();
    let far2: Vec<i16> = (0..n).map(|i| (((i as f32) * 0.002).sin() * 1200.0) as i16).collect();

    // Fresh canceller processes the signal.
    let mut fresh = AcousticEchoCanceller::new().expect("new");
    let out_fresh = process_all(&mut fresh, &near, &far);

    // Reused canceller: dirty its adaptive state with other audio, reset(),
    // then process the same signal.
    let mut reused = AcousticEchoCanceller::new().expect("new");
    let _ = process_all(&mut reused, &near2, &far2);
    reused.reset();
    let out_reused = process_all(&mut reused, &near, &far);

    assert_eq!(
        out_fresh, out_reused,
        "reset() must restore freshly-built behavior (else reuse corrupts streams)"
    );
}
