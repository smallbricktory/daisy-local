//! Realtime-factor benchmark. Asserts AEC processes at ≥ 1.5× realtime in
//! release builds.

use aec::AcousticEchoCanceller;
use aec::constants::{BLOCK_SHIFT, model_dir};
use std::time::Instant;

#[test]
fn aec_runs_at_or_above_1_5x_realtime() {
    if !model_dir().join("model_256_1.onnx").exists() {
        eprintln!("skipping: model files absent");
        return;
    }
    let mut aec = AcousticEchoCanceller::load(&model_dir()).expect("load");
    let sr = 16_000usize;
    let secs = 30usize;
    let n = sr * secs;
    let near: Vec<i16> = (0..n).map(|i| (((i as f32) * 0.001).sin() * 1000.0) as i16).collect();
    let far: Vec<i16> = (0..n).map(|i| (((i as f32) * 0.0007).sin() * 1500.0) as i16).collect();

    // Warm-up frame.
    aec.process(&near[..BLOCK_SHIFT], &far[..BLOCK_SHIFT]).unwrap();

    let t0 = Instant::now();
    let mut i = BLOCK_SHIFT;
    while i + BLOCK_SHIFT <= n {
        aec.process(&near[i..i + BLOCK_SHIFT], &far[i..i + BLOCK_SHIFT]).unwrap();
        i += BLOCK_SHIFT;
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
