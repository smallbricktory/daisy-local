//! Numerical correctness tests for AcousticEchoCanceller.

use aec::AcousticEchoCanceller;

const FRAME_SIZE: usize = AcousticEchoCanceller::FRAME_SIZE;

const SR: usize = 16_000;

fn sine_i16(n: usize, hz: f32, sr: f32, amp: f32) -> Vec<i16> {
    (0..n)
        .map(|i| {
            (amp * (2.0 * std::f32::consts::PI * hz * (i as f32) / sr).sin() * 32767.0) as i16
        })
        .collect()
}

fn run(aec_obj: &mut AcousticEchoCanceller, near: &[i16], far: &[i16]) -> Vec<i16> {
    assert_eq!(near.len(), far.len());
    let mut out = Vec::with_capacity(near.len());
    let mut i = 0;
    while i + FRAME_SIZE <= near.len() {
        let frame = aec_obj
            .process(&near[i..i + FRAME_SIZE], &far[i..i + FRAME_SIZE])
            .expect("process");
        out.extend_from_slice(&frame);
        i += FRAME_SIZE;
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
    // 2 s of mic tone, silent far end: nothing to cancel, so the canceller
    // must be near-transparent (only the high-pass filter touches the path).
    let near = sine_i16(SR * 2, 600.0, SR as f32, 0.3);
    let far = vec![0i16; near.len()];

    let mut aec_obj = AcousticEchoCanceller::new().expect("new");
    let out = run(&mut aec_obj, &near, &far);

    // Skip the first 0.5 s.
    let skip = SR / 2;
    let in_rms = rms(&near[skip..out.len()]);
    let out_rms = rms(&out[skip..]);
    let preservation_db = 20.0 * (out_rms.max(1.0) / in_rms.max(1.0)).log10();
    assert!(
        preservation_db > -3.0,
        "canceller damaged a signal it had no reason to touch: {preservation_db:.1} dB (must be > -3 dB)"
    );
}

#[test]
fn echo_test_meets_erle_threshold() {
    // Far = clear signal; near = 0.25 × far (a -12 dB echo, no near speech).
    let far = sine_i16(SR * 3, 800.0, SR as f32, 0.5);
    let near: Vec<i16> = far.iter().map(|&s| (s as f32 * 0.25) as i16).collect();

    let mut aec_obj = AcousticEchoCanceller::new().expect("new");
    let out = run(&mut aec_obj, &near, &far);

    // Skip the first second: AEC3 needs time to estimate the echo path.
    let skip = SR;
    let in_rms = rms(&near[skip..out.len()]);
    let out_rms = rms(&out[skip..]);
    let erle_db = 20.0 * (in_rms.max(1.0) / out_rms.max(1.0)).log10();
    assert!(erle_db >= 15.0, "ERLE too low: {erle_db:.1} dB (must be ≥ 15 dB)");
}

/// Deterministic broadband pseudo-noise (xorshift). Periodic stimuli (sines)
/// cannot test delay handling: a linear filter cancels any phase shift of a
/// sine, so only broadband content forces the estimator to find the true lag.
fn noise_i16(n: usize, seed: u64, amp: f32) -> Vec<i16> {
    let mut x = seed | 1;
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            ((x as i32) as f32 / i32::MAX as f32 * amp * 32767.0) as i16
        })
        .collect()
}

/// Mean-square power of the single-frequency component (Goertzel bin): a
/// sine of amplitude A measures A²/2, directly comparable to full-band
/// mean-square power.
fn tone_power(samples: &[i16], hz: f32, sr: f32) -> f64 {
    let w = 2.0 * std::f64::consts::PI * (hz as f64) / (sr as f64);
    let coeff = 2.0 * w.cos();
    let (mut s1, mut s2) = (0.0f64, 0.0f64);
    for &x in samples {
        let s0 = x as f64 / 32768.0 + coeff * s1 - s2;
        s2 = s1;
        s1 = s0;
    }
    let mag_sq = s1 * s1 + s2 * s2 - coeff * s1 * s2;
    let n = samples.len() as f64;
    2.0 * mag_sq / (n * n)
}

#[test]
fn internal_delay_estimator_handles_a_misaligned_reference() {
    // Design condition: the canceller aligns the streams itself, so callers
    // only need them roughly paired. Echo arrives 100 ms after the
    // reference, with NO pre-alignment here.
    const DELAY: usize = 1_600; // 100 ms
    let far = noise_i16(SR * 4, 7, 0.4);
    let near: Vec<i16> = (0..far.len())
        .map(|i| if i >= DELAY { (far[i - DELAY] as f32 * 0.25) as i16 } else { 0 })
        .collect();

    let mut aec_obj = AcousticEchoCanceller::new().expect("new");
    let out = run(&mut aec_obj, &near, &far);

    let skip = SR * 3 / 2;
    let erle_db =
        20.0 * (rms(&near[skip..out.len()]).max(1.0) / rms(&out[skip..]).max(1.0)).log10();
    assert!(
        erle_db >= 10.0,
        "canceller must find a 100 ms echo path on its own: ERLE {erle_db:.1} dB (must be ≥ 10)"
    );

    // The absolute value is the estimator's internal figure — it counts
    // from its own buffer alignment, not the acoustic lag — so assert only
    // that the estimator engaged; the cancellation assert above is the real
    // condition.
    assert!(aec_obj.stats().delay_ms.is_some(), "estimator must report a delay after 4 s of echo");
}

#[test]
fn double_talk_keeps_the_near_voice_while_removing_echo() {
    // Far = broadband noise; near = its echo (-12 dB, 50 ms path). The
    // canceller converges on 2 s of echo alone, then a 300 Hz "voice"
    // joins. Over the double-talk span the tone must survive and the echo
    // must not.
    const DELAY: usize = 800; // 50 ms
    const VOICE_AT: usize = SR * 2;
    let far = noise_i16(SR * 5, 21, 0.4);
    let near: Vec<i16> = (0..far.len())
        .map(|i| {
            let echo = if i >= DELAY { (far[i - DELAY] as f32 * 0.25) as i16 } else { 0 };
            let v = if i >= VOICE_AT {
                (0.15 * (2.0 * std::f32::consts::PI * 300.0 * (i as f32) / SR as f32).sin()
                    * 32767.0) as i16
            } else {
                0
            };
            echo.saturating_add(v)
        })
        .collect();

    let mut aec_obj = AcousticEchoCanceller::new().expect("new");
    let out = run(&mut aec_obj, &near, &far);

    // Skip past convergence plus half a second of the suppressor reacting to
    // the voice onset.
    let skip = VOICE_AT + SR / 2;
    let (near_w, out_w) = (&near[skip..out.len()], &out[skip..]);

    // The voice: compare the 300 Hz line in the output against the input.
    let voice_in = tone_power(near_w, 300.0, SR as f32);
    let voice_out = tone_power(out_w, 300.0, SR as f32);
    let voice_loss_db = 10.0 * (voice_in / voice_out.max(1e-12)).log10();
    assert!(
        voice_loss_db < 3.0,
        "double-talk ate the near voice: {voice_loss_db:.1} dB lost (must stay < 3 dB)"
    );

    // The echo: total output energy beyond the surviving tone must sit well
    // below the echo energy that came in.
    let mean_sq = |s: &[i16]| {
        s.iter().map(|&x| (x as f64 / 32768.0).powi(2)).sum::<f64>() / s.len() as f64
    };
    let residual_pow = (mean_sq(out_w) - voice_out).max(1e-12);
    let echo_in_pow = (mean_sq(near_w) - voice_in).max(1e-12);
    let echo_removed_db = 10.0 * (echo_in_pow / residual_pow).log10();
    assert!(
        echo_removed_db >= 6.0,
        "echo must still be cancelled during double-talk: {echo_removed_db:.1} dB removed (must be ≥ 6)"
    );
}

#[test]
fn identical_inputs_produce_identical_outputs() {
    // The canceller is deterministic: two fresh instances fed the same audio
    // emit bit-identical output (chunk-independent finalize depends on this
    // via reset()).
    let far = sine_i16(SR, 800.0, SR as f32, 0.5);
    let near: Vec<i16> = far.iter().map(|&s| (s as f32 * 0.25) as i16).collect();

    let mut a = AcousticEchoCanceller::new().expect("new");
    let mut b = AcousticEchoCanceller::new().expect("new");
    assert_eq!(run(&mut a, &near, &far), run(&mut b, &near, &far));
}

#[test]
fn wrong_frame_size_is_rejected() {
    let mut aec_obj = AcousticEchoCanceller::new().expect("new");
    let short = vec![0i16; FRAME_SIZE - 1];
    let full = vec![0i16; FRAME_SIZE];
    assert!(aec_obj.process(&short, &full).is_err());
    assert!(aec_obj.process(&full, &short).is_err());
}
