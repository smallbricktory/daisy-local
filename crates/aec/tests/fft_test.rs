use aec::constants::{BLOCK_SIZE, FFT_BINS};
use aec::fft::{ifft_real, rfft};

fn sine(n: usize, hz: f32, sr: f32) -> Vec<f32> {
    (0..n)
        .map(|i| (2.0 * std::f32::consts::PI * hz * (i as f32) / sr).sin())
        .collect()
}

#[test]
fn rfft_output_has_correct_length() {
    let frame = sine(BLOCK_SIZE, 440.0, 16_000.0);
    let spectrum = rfft(&frame);
    assert_eq!(spectrum.len(), FFT_BINS);
}

#[test]
fn round_trip_preserves_signal() {
    let frame: Vec<f32> = sine(BLOCK_SIZE, 600.0, 16_000.0);
    let spectrum = rfft(&frame);
    let reconstructed = ifft_real(&spectrum);
    assert_eq!(reconstructed.len(), BLOCK_SIZE);
    let max_err = frame
        .iter()
        .zip(reconstructed.iter())
        .map(|(a, b)| (a - b).abs())
        .fold(0f32, f32::max);
    assert!(
        max_err < 1e-4,
        "round-trip max error too large: {max_err}"
    );
}
