//! `reset()` restores a canceller to exactly its freshly-loaded behavior.

use aec::constants::{model_dir, BLOCK_SHIFT};
use aec::AcousticEchoCanceller;

fn process_all(aec: &mut AcousticEchoCanceller, near: &[i16], far: &[i16]) -> Vec<i16> {
    let mut out = Vec::new();
    let mut i = 0;
    while i + BLOCK_SHIFT <= near.len() {
        out.extend(
            aec.process(&near[i..i + BLOCK_SHIFT], &far[i..i + BLOCK_SHIFT])
                .unwrap(),
        );
        i += BLOCK_SHIFT;
    }
    out
}

#[test]
fn reset_restores_fresh_load_output() {
    if !model_dir().join("model_256_1.onnx").exists() {
        eprintln!("skipping: model files absent");
        return;
    }
    let n = BLOCK_SHIFT * 200;
    let near: Vec<i16> = (0..n).map(|i| (((i as f32) * 0.001).sin() * 1000.0) as i16).collect();
    let far: Vec<i16> = (0..n).map(|i| (((i as f32) * 0.0007).sin() * 1500.0) as i16).collect();
    // A different signal used only to dirty the reused canceller's state.
    let near2: Vec<i16> = (0..n).map(|i| (((i as f32) * 0.003).sin() * 800.0) as i16).collect();
    let far2: Vec<i16> = (0..n).map(|i| (((i as f32) * 0.002).sin() * 1200.0) as i16).collect();

    // Fresh canceller processes the signal.
    let mut fresh = AcousticEchoCanceller::load(&model_dir()).expect("load");
    let out_fresh = process_all(&mut fresh, &near, &far);

    // Reused canceller: dirty its LSTM state + rolling buffers with other
    // audio, reset(), then process the same signal.
    let mut reused = AcousticEchoCanceller::load(&model_dir()).expect("load");
    let _ = process_all(&mut reused, &near2, &far2);
    reused.reset();
    let out_reused = process_all(&mut reused, &near, &far);

    assert_eq!(
        out_fresh, out_reused,
        "reset() must restore freshly-loaded behavior (else reuse corrupts streams)"
    );
}
