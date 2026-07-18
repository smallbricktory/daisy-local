//! Kaldi-compatible log-Mel filterbank front-end for the WeSpeaker ResNet34
//! ONNX model. The model's `feats` input takes 80-dim Fbank features
//! (torchaudio's `compliance.kaldi.fbank` defaults), followed by
//! utterance-level cepstral mean normalization (CMN, mean-only) — not raw
//! audio.
//!
//! Per-frame pipeline (matches kaldi `FeatureWindowFunction` + `MelBanks`):
//!   1. slice a 25 ms window (snip_edges), 10 ms hop
//!   2. remove DC offset (subtract the frame mean)
//!   3. pre-emphasis (coeff 0.97, first sample replicated)
//!   4. Povey window
//!   5. real FFT, power spectrum (|X|^2) over n_fft = next_pow2(frame_len)
//!   6. triangular Mel filterbank (80 bins, 20 Hz .. Nyquist, kaldi mel)
//!   7. log with a small floor
//! Then subtract the per-dim mean across all frames (CMN, no variance norm).
//!
//! Dither and energy are not applied. Output: an `Array2<f32>` of shape
//! `[num_frames, NUM_MEL_BINS]`.

use ndarray::Array2;
use num_complex::Complex32;
use std::sync::Arc;

pub const NUM_MEL_BINS: usize = 80;
const SAMPLE_RATE: f32 = 16_000.0;
const FRAME_LENGTH_MS: f32 = 25.0;
const FRAME_SHIFT_MS: f32 = 10.0;
const PREEMPH_COEFF: f32 = 0.97;
const LOW_FREQ: f32 = 20.0;
const DITHER: f32 = 0.0;
const EPSILON: f32 = f32::EPSILON;

fn frame_length() -> usize {
    (SAMPLE_RATE * FRAME_LENGTH_MS / 1000.0).round() as usize // 400
}
fn frame_shift() -> usize {
    (SAMPLE_RATE * FRAME_SHIFT_MS / 1000.0).round() as usize // 160
}
fn n_fft(frame_len: usize) -> usize {
    // round_to_power_of_two = true
    let mut n = 1usize;
    while n < frame_len {
        n <<= 1;
    }
    n // 512 for a 400-sample frame
}

/// Hz -> kaldi Mel.
fn hz_to_mel(hz: f32) -> f32 {
    1127.0 * (1.0 + hz / 700.0).ln()
}

/// Povey window: (0.5 - 0.5*cos(2*pi*n/(N-1)))^0.85.
fn povey_window(n: usize) -> Vec<f32> {
    let nm1 = (n - 1) as f32;
    (0..n)
        .map(|i| {
            let a = 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / nm1).cos();
            a.powf(0.85)
        })
        .collect()
}

/// Triangular Mel filterbank as kaldi builds it: `num_bins` filters spanning
/// `low_freq .. Nyquist` in the Mel domain, each defined over the
/// `n_fft/2 + 1` power-spectrum bins. Returns dense weights per filter.
fn mel_banks(num_bins: usize, n_fft: usize) -> Vec<Vec<f32>> {
    let num_fft_bins = n_fft / 2 + 1;
    let nyquist = SAMPLE_RATE / 2.0;
    let mel_low = hz_to_mel(LOW_FREQ);
    let mel_high = hz_to_mel(nyquist);
    let mel_delta = (mel_high - mel_low) / (num_bins + 1) as f32;
    let fft_bin_width = SAMPLE_RATE / n_fft as f32;

    let mut banks = Vec::with_capacity(num_bins);
    for bin in 0..num_bins {
        let left_mel = mel_low + bin as f32 * mel_delta;
        let center_mel = mel_low + (bin + 1) as f32 * mel_delta;
        let right_mel = mel_low + (bin + 2) as f32 * mel_delta;

        let mut weights = vec![0f32; num_fft_bins];
        for (fft_bin, w) in weights.iter_mut().enumerate() {
            let mel = hz_to_mel(fft_bin_width * fft_bin as f32);
            if mel > left_mel && mel < right_mel {
                *w = if mel <= center_mel {
                    (mel - left_mel) / (center_mel - left_mel)
                } else {
                    (right_mel - mel) / (right_mel - center_mel)
                };
            }
        }
        banks.push(weights);
    }
    banks
}

/// Compute the kaldi-style log-Mel filterbank for a 16 kHz mono signal and
/// apply utterance CMN (mean subtraction). Output shape `[num_frames, 80]`.
pub fn compute(samples: &[f32]) -> Array2<f32> {
    let flen = frame_length();
    let fshift = frame_shift();
    if samples.len() < flen {
        return Array2::<f32>::zeros((0, NUM_MEL_BINS));
    }
    let nfft = n_fft(flen);
    let window = povey_window(flen);
    let banks = mel_banks(NUM_MEL_BINS, nfft);

    let mut planner = rustfft::FftPlanner::<f32>::new();
    let fft: Arc<dyn rustfft::Fft<f32>> = planner.plan_fft_forward(nfft);

    // snip_edges = true: num_frames = 1 + (n - frame_len) / frame_shift
    let num_frames = 1 + (samples.len() - flen) / fshift;
    let mut feats = Array2::<f32>::zeros((num_frames, NUM_MEL_BINS));

    let mut buf: Vec<Complex32> = vec![Complex32::new(0.0, 0.0); nfft];
    for f in 0..num_frames {
        let off = f * fshift;
        let frame = &samples[off..off + flen];

        // 1. remove DC offset (subtract mean).
        let mean: f32 = frame.iter().sum::<f32>() / flen as f32;
        let mut w: Vec<f32> = frame.iter().map(|x| x - mean).collect();

        // 2. pre-emphasis (replicate first sample), in place, high->low.
        for i in (1..flen).rev() {
            w[i] -= PREEMPH_COEFF * w[i - 1];
        }
        w[0] -= PREEMPH_COEFF * w[0];

        // 3. window.
        for i in 0..flen {
            w[i] *= window[i];
        }

        // 4. FFT (zero-padded to nfft).
        for (i, slot) in buf.iter_mut().enumerate() {
            *slot = Complex32::new(if i < flen { w[i] } else { 0.0 }, 0.0);
        }
        fft.process(&mut buf);

        // 5. power spectrum over the first nfft/2 + 1 bins.
        let num_fft_bins = nfft / 2 + 1;
        let mut power = vec![0f32; num_fft_bins];
        for (i, p) in power.iter_mut().enumerate() {
            *p = buf[i].norm_sqr();
        }

        // 6. mel filterbank + log.
        for (m, weights) in banks.iter().enumerate() {
            let mut e = 0f32;
            for (idx, wt) in weights.iter().enumerate() {
                e += wt * power[idx];
            }
            feats[(f, m)] = e.max(EPSILON).ln();
        }
    }

    // 7. CMN: subtract per-dim mean across frames.
    if num_frames > 0 {
        for m in 0..NUM_MEL_BINS {
            let mut sum = 0f32;
            for f in 0..num_frames {
                sum += feats[(f, m)];
            }
            let mean = sum / num_frames as f32;
            for f in 0..num_frames {
                feats[(f, m)] -= mean;
            }
        }
    }

    let _ = DITHER; // inference: no dither
    feats
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_geometry() {
        assert_eq!(frame_length(), 400);
        assert_eq!(frame_shift(), 160);
        assert_eq!(n_fft(400), 512);
    }

    #[test]
    fn shape_is_frames_by_80() {
        // 1 second of 16 kHz audio → (16000-400)/160 + 1 = 98 frames.
        let samples = vec![0.01f32; 16_000];
        let feats = compute(&samples);
        assert_eq!(feats.shape(), &[98, NUM_MEL_BINS]);
    }

    #[test]
    fn too_short_returns_empty() {
        let feats = compute(&vec![0.0f32; 100]);
        assert_eq!(feats.shape(), &[0, NUM_MEL_BINS]);
    }
}
