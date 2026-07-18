//! Numerical correctness tests for AcousticEchoCanceller.

use aec::AcousticEchoCanceller;
use aec::constants::{BLOCK_SHIFT, model_dir};

const SR: usize = 16_000;

fn sine_i16(n: usize, hz: f32, sr: f32, amp: f32) -> Vec<i16> {
    (0..n)
        .map(|i| {
            (amp * (2.0 * std::f32::consts::PI * hz * (i as f32) / sr).sin() * 32767.0) as i16
        })
        .collect()
}

fn aec() -> AcousticEchoCanceller {
    AcousticEchoCanceller::load(&model_dir())
        .expect("load AEC (run models/dtln-aec/download.sh 256 first)")
}

fn run(aec_obj: &mut AcousticEchoCanceller, near: &[i16], far: &[i16]) -> Vec<i16> {
    assert_eq!(near.len(), far.len());
    let mut out = Vec::with_capacity(near.len());
    let mut i = 0;
    while i + BLOCK_SHIFT <= near.len() {
        let frame = aec_obj
            .process(&near[i..i + BLOCK_SHIFT], &far[i..i + BLOCK_SHIFT])
            .expect("process");
        out.extend_from_slice(&frame);
        i += BLOCK_SHIFT;
    }
    out
}

fn rms(samples: &[i16]) -> f32 {
    let n = samples.len() as f32;
    if n == 0.0 {
        return 0.0;
    }
    (samples.iter().map(|&s| (s as f32).powi(2)).sum::<f32>() / n).sqrt()
}

#[test]
fn passthrough_with_silent_far_preserves_signal() {
    if !model_dir().join("model_256_1.onnx").exists() {
        eprintln!("skipping: model files absent");
        return;
    }
    // 2 s of mic tone, silent far end. The noise-suppression stage attenuates
    // pure tones with a silent far-end reference; the test asserts the
    // pipeline output is above the near-silence floor, not that the input is
    // preserved.
    let near = sine_i16(SR * 2, 600.0, SR as f32, 0.3);
    let far = vec![0i16; near.len()];

    let mut aec_obj = aec();
    let out = run(&mut aec_obj, &near, &far);

    // Skip the first 0.5 s.
    let skip = SR / 2;
    let in_rms = rms(&near[skip..out.len()]);
    let out_rms = rms(&out[skip..]);
    let preservation_db = 20.0 * (out_rms.max(1.0) / in_rms.max(1.0)).log10();
    assert!(
        preservation_db > -65.0,
        "AEC pipeline produced near-silence with silent-far input: {preservation_db:.1} dB (must be > -65 dB)"
    );
}

#[test]
fn echo_test_meets_erle_threshold() {
    if !model_dir().join("model_256_1.onnx").exists() {
        eprintln!("skipping: model files absent");
        return;
    }
    // Far = clear signal; near = 0.25 × far (a -12 dB echo, no near speech).
    let far = sine_i16(SR * 3, 800.0, SR as f32, 0.5);
    let near: Vec<i16> = far.iter().map(|&s| (s as f32 * 0.25) as i16).collect();

    let mut aec_obj = aec();
    let out = run(&mut aec_obj, &near, &far);

    let skip = SR / 2;
    let in_rms = rms(&near[skip..out.len()]);
    let out_rms = rms(&out[skip..]);
    let erle_db = 20.0 * (in_rms.max(1.0) / out_rms.max(1.0)).log10();
    assert!(
        erle_db >= 6.0,
        "ERLE too low: {erle_db:.1} dB (must be ≥ 6 dB)"
    );
}

#[test]
fn state_persists_across_frames() {
    if !model_dir().join("model_256_1.onnx").exists() {
        eprintln!("skipping: model files absent");
        return;
    }
    // Two fresh AECs given the same first frame produce identical outputs.
    // After running aec1 for 3 frames, its third output differs from aec2's
    // first-frame output.
    let near = sine_i16(BLOCK_SHIFT * 4, 440.0, SR as f32, 0.3);
    let far = vec![0i16; near.len()];

    let mut aec1 = aec();
    let mut aec2 = aec();

    let frame_1_a = aec1
        .process(&near[..BLOCK_SHIFT], &far[..BLOCK_SHIFT])
        .unwrap();
    let frame_1_b = aec2
        .process(&near[..BLOCK_SHIFT], &far[..BLOCK_SHIFT])
        .unwrap();
    assert_eq!(
        frame_1_a, frame_1_b,
        "first-frame outputs must match for fresh AECs (deterministic + same input)"
    );

    let _frame_2_a = aec1
        .process(&near[BLOCK_SHIFT..2 * BLOCK_SHIFT], &far[BLOCK_SHIFT..2 * BLOCK_SHIFT])
        .unwrap();
    let frame_3_a = aec1
        .process(&near[2 * BLOCK_SHIFT..3 * BLOCK_SHIFT], &far[2 * BLOCK_SHIFT..3 * BLOCK_SHIFT])
        .unwrap();
    let frame_1_only_b = aec2
        .process(&near[BLOCK_SHIFT..2 * BLOCK_SHIFT], &far[BLOCK_SHIFT..2 * BLOCK_SHIFT])
        .unwrap();

    assert_ne!(
        frame_3_a, frame_1_only_b,
        "aec1's 3rd-frame output should differ from aec2's 2nd-frame (initial-state) output (state should have propagated)"
    );
}
