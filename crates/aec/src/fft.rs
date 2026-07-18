//! Real-valued FFT helpers wrapping `rustfft`. The forward/inverse round-trip
//! is the identity (within floating-point error).

use crate::constants::{BLOCK_SIZE, FFT_BINS};
use num_complex::Complex32;
use rustfft::{num_complex::Complex as RustfftComplex, Fft, FftPlanner};
use std::sync::{Arc, OnceLock};

// The forward/inverse FFT plans are created once and shared across threads;
// `rustfft` plans are immutable and `process(&self, …)` is thread-safe.
fn fwd_fft() -> &'static Arc<dyn Fft<f32>> {
    static FWD: OnceLock<Arc<dyn Fft<f32>>> = OnceLock::new();
    FWD.get_or_init(|| FftPlanner::<f32>::new().plan_fft_forward(BLOCK_SIZE))
}
fn inv_fft() -> &'static Arc<dyn Fft<f32>> {
    static INV: OnceLock<Arc<dyn Fft<f32>>> = OnceLock::new();
    INV.get_or_init(|| FftPlanner::<f32>::new().plan_fft_inverse(BLOCK_SIZE))
}

/// Forward real FFT of a `BLOCK_SIZE`-sample frame.
/// Returns `FFT_BINS` complex bins (DC, positive frequencies, Nyquist).
pub fn rfft(frame: &[f32]) -> Vec<Complex32> {
    debug_assert_eq!(frame.len(), BLOCK_SIZE);
    let fft = fwd_fft();
    let mut buf: Vec<RustfftComplex<f32>> = frame
        .iter()
        .map(|&s| RustfftComplex { re: s, im: 0.0 })
        .collect();
    fft.process(&mut buf);
    // Take the non-redundant half (DC + positive frequencies + Nyquist)
    buf.into_iter()
        .take(FFT_BINS)
        .map(|c| Complex32::new(c.re, c.im))
        .collect()
}

/// Inverse real FFT: `FFT_BINS` complex bins back to `BLOCK_SIZE` real samples.
/// Normalized by 1/BLOCK_SIZE; inverts `rfft` exactly.
pub fn ifft_real(spectrum: &[Complex32]) -> Vec<f32> {
    debug_assert_eq!(spectrum.len(), FFT_BINS);
    let fft = inv_fft();

    // Reconstruct the full conjugate-symmetric spectrum.
    let mut buf: Vec<RustfftComplex<f32>> = vec![RustfftComplex::new(0.0, 0.0); BLOCK_SIZE];
    for (i, &c) in spectrum.iter().enumerate() {
        buf[i] = RustfftComplex { re: c.re, im: c.im };
    }
    // Mirror the positive-frequency bins to negative-frequency conjugates.
    // Indices 0 (DC) and FFT_BINS-1 (Nyquist) are real and are not mirrored.
    for (i, &c) in spectrum
        .iter()
        .enumerate()
        .skip(1)
        .take(FFT_BINS - 2)
    {
        let mirror = BLOCK_SIZE - i;
        buf[mirror] = RustfftComplex {
            re: c.re,
            im: -c.im,
        };
    }
    fft.process(&mut buf);
    let scale = 1.0_f32 / BLOCK_SIZE as f32;
    buf.into_iter().map(|c| c.re * scale).collect()
}
