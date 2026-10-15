// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Incremental log-mel front end for the Nemotron encoder: 128 slaney mel
//! bins over a 512-point FFT of a 400-sample Hann window centred in the
//! frame, 160-sample hop, pre-emphasis 0.97, `ln(x + 2^-24)`, no
//! normalization. Frame `f` is centred on sample `160 f`; the stream start
//! is zero-padded. Frames leave as 65-frame encoder windows: the last 9
//! frames of the previous window (zeros before the first) and 56 new ones.

use std::collections::VecDeque;
use std::sync::Arc;

use ndarray::Array3;
use realfft::num_complex::Complex;
use realfft::{RealFftPlanner, RealToComplex};

pub const SAMPLE_RATE: usize = 16_000;
pub const HOP: usize = 160;
pub const N_MELS: usize = 128;
pub const CHUNK_FRAMES: usize = 56;
pub const CONTEXT_FRAMES: usize = 9;
pub const WINDOW_FRAMES: usize = CONTEXT_FRAMES + CHUNK_FRAMES;
pub const CHUNK_SAMPLES: usize = CHUNK_FRAMES * HOP;
pub const CHUNK_MS: i64 = (CHUNK_SAMPLES * 1000 / SAMPLE_RATE) as i64;
/// Value of a mel bin with no energy: `ln(2^-24)`.
pub const SILENT_BIN: f32 = -16.635_532;
/// Feature value of the context frames ahead of the first window.
const CONTEXT_FILL: f32 = 0.0;
const N_FFT: usize = 512;
const WIN: usize = 400;
const WIN_OFFSET: usize = (N_FFT - WIN) / 2;
const PAD: usize = N_FFT / 2;
const PREEMPH: f32 = 0.97;
const LOG_FLOOR: f32 = 5.960_464_5e-8;
/// Samples past a chunk's end that its last frame's window reaches: chunk
/// `c` of a stream is ready once `CHUNK_SAMPLES * (c + 1) +
/// CHUNK_LOOKAHEAD_SAMPLES` samples have arrived.
pub const CHUNK_LOOKAHEAD_SAMPLES: usize = WIN_OFFSET + WIN - PAD - HOP;
/// Zeros `finish` appends: two chunks.
const FLUSH_SAMPLES: usize = 2 * CHUNK_SAMPLES;

pub struct MelStream {
    fft: Arc<dyn RealToComplex<f32>>,
    window: Vec<f32>,
    /// Per mel bin: first FFT bin with weight, and the weights from there.
    filters: Vec<(usize, Vec<f32>)>,
    /// Pre-emphasized, left-padded samples; `buf[0]` is padded index `base`.
    buf: Vec<f32>,
    base: usize,
    prev: Option<f32>,
    next_frame: usize,
    pending: VecDeque<Vec<f32>>,
    context: Vec<Vec<f32>>,
    fft_in: Vec<f32>,
    fft_out: Vec<Complex<f32>>,
}

impl Default for MelStream {
    fn default() -> Self {
        Self::new()
    }
}

impl MelStream {
    pub fn new() -> Self {
        let fft = RealFftPlanner::<f32>::new().plan_fft_forward(N_FFT);
        let mut window = vec![0.0f32; N_FFT];
        for i in 0..WIN {
            let phase = 2.0 * std::f32::consts::PI * i as f32 / (WIN - 1) as f32;
            window[WIN_OFFSET + i] = 0.5 - 0.5 * phase.cos();
        }
        let fft_in = fft.make_input_vec();
        let fft_out = fft.make_output_vec();
        Self {
            fft,
            window,
            filters: mel_filters(),
            buf: vec![0.0; PAD],
            base: 0,
            prev: None,
            next_frame: 0,
            pending: VecDeque::new(),
            context: vec![vec![CONTEXT_FILL; N_MELS]; CONTEXT_FRAMES],
            fft_in,
            fft_out,
        }
    }

    /// Append 16 kHz mono samples and compute every frame whose window is
    /// now complete.
    pub fn push(&mut self, samples: &[f32]) {
        self.buf.reserve(samples.len());
        for &s in samples {
            let y = match self.prev {
                Some(p) => s - PREEMPH * p,
                None => s,
            };
            self.prev = Some(s);
            self.buf.push(y);
        }
        while self.next_frame * HOP + WIN_OFFSET + WIN <= self.base + self.buf.len() {
            let start = self.next_frame * HOP - self.base;
            let frame = self.frame(start);
            self.pending.push_back(frame);
            self.next_frame += 1;
        }
        let keep_from = self.next_frame * HOP - self.base;
        if keep_from > 0 {
            self.buf.drain(..keep_from);
            self.base += keep_from;
        }
    }

    /// End of input: zero samples push the last partial chunk out.
    pub fn finish(&mut self) {
        self.push(&vec![0.0; FLUSH_SAMPLES]);
    }

    /// The next `[1, N_MELS, WINDOW_FRAMES]` encoder window, once 56 new
    /// frames exist.
    pub fn next_chunk(&mut self) -> Option<Array3<f32>> {
        if self.pending.len() < CHUNK_FRAMES {
            return None;
        }
        let frames: Vec<Vec<f32>> = self.pending.drain(..CHUNK_FRAMES).collect();
        let mut out = Array3::<f32>::zeros((1, N_MELS, WINDOW_FRAMES));
        for (t, f) in self.context.iter().chain(frames.iter()).enumerate() {
            for (m, &v) in f.iter().enumerate() {
                out[[0, m, t]] = v;
            }
        }
        self.context = frames[CHUNK_FRAMES - CONTEXT_FRAMES..].to_vec();
        Some(out)
    }

    fn frame(&mut self, start: usize) -> Vec<f32> {
        for i in 0..N_FFT {
            self.fft_in[i] = self.buf.get(start + i).copied().unwrap_or(0.0) * self.window[i];
        }
        self.fft
            .process(&mut self.fft_in, &mut self.fft_out)
            .expect("fft buffers match the plan");
        let power: Vec<f32> = self.fft_out.iter().map(|c| c.norm_sqr()).collect();
        self.filters
            .iter()
            .map(|(first, w)| {
                let e: f32 = w.iter().zip(&power[*first..]).map(|(a, b)| a * b).sum();
                (e + LOG_FLOOR).ln()
            })
            .collect()
    }
}

/// Slaney-scale, slaney-normalized triangular filters over 0-8 kHz.
fn mel_filters() -> Vec<(usize, Vec<f32>)> {
    let log_step = 6.4f64.ln() / 27.0;
    let hz_to_mel = |hz: f64| if hz < 1000.0 { hz * 3.0 / 200.0 } else { 15.0 + (hz / 1000.0).ln() / log_step };
    let mel_to_hz = |m: f64| if m < 15.0 { m * 200.0 / 3.0 } else { 1000.0 * ((m - 15.0) * log_step).exp() };
    let top = hz_to_mel(SAMPLE_RATE as f64 / 2.0);
    let pts: Vec<f64> = (0..N_MELS + 2).map(|i| mel_to_hz(top * i as f64 / (N_MELS + 1) as f64)).collect();
    (0..N_MELS)
        .map(|m| {
            let (lo, mid, hi) = (pts[m], pts[m + 1], pts[m + 2]);
            let norm = 2.0 / (hi - lo);
            let w: Vec<f32> = (0..=N_FFT / 2)
                .map(|k| {
                    let f = k as f64 * SAMPLE_RATE as f64 / N_FFT as f64;
                    (((f - lo) / (mid - lo)).min((hi - f) / (hi - mid)).max(0.0) * norm) as f32
                })
                .collect();
            let first = w.iter().position(|&v| v > 0.0).unwrap_or(0);
            let last = w.iter().rposition(|&v| v > 0.0).map_or(first, |p| p + 1);
            (first, w[first..last].to_vec())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn two_tone(n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| {
                let t = i as f64 / SAMPLE_RATE as f64;
                (0.5 * (2.0 * std::f64::consts::PI * 440.0 * t).sin()
                    + 0.25 * (2.0 * std::f64::consts::PI * 1250.0 * t).sin()) as f32
            })
            .collect()
    }

    fn all_chunks(m: &mut MelStream) -> Vec<Array3<f32>> {
        std::iter::from_fn(|| m.next_chunk()).collect()
    }

    #[test]
    fn frames_match_the_nemo_front_end() {
        let mut m = MelStream::new();
        m.push(&two_tone(SAMPLE_RATE));
        let c = m.next_chunk().expect("one chunk from one second");
        let at = |frame: usize, bin: usize| c[[0, bin, CONTEXT_FRAMES + frame]];
        for (frame, bin, want) in [
            (0, 19, -0.4955), (0, 51, 0.1354), (0, 52, -0.0791),
            (1, 19, 0.4260), (1, 51, 1.5234), (1, 52, 0.7326),
            (50, 19, 0.4190), (50, 51, 1.5372), (50, 52, 0.7182),
        ] {
            let got = at(frame, bin);
            assert!((got - want).abs() < 0.01, "frame {frame} bin {bin}: {got} vs {want}");
        }
    }

    #[test]
    fn first_window_context_is_zero_features() {
        let mut m = MelStream::new();
        m.push(&two_tone(SAMPLE_RATE));
        let c = m.next_chunk().unwrap();
        for t in 0..CONTEXT_FRAMES {
            for b in 0..N_MELS {
                assert_eq!(c[[0, b, t]], 0.0);
            }
        }
    }

    #[test]
    fn a_chunk_is_ready_once_its_last_frame_has_its_window() {
        let x = two_tone(3 * CHUNK_SAMPLES);
        let mut m = MelStream::new();
        m.push(&x[..8_999]);
        assert!(m.next_chunk().is_none());
        m.push(&x[8_999..9_000]);
        assert!(m.next_chunk().is_some());
        m.push(&x[9_000..CHUNK_SAMPLES + 8_999]);
        assert!(m.next_chunk().is_none());
        m.push(&x[CHUNK_SAMPLES + 8_999..CHUNK_SAMPLES + 9_000]);
        assert!(m.next_chunk().is_some());
    }

    #[test]
    fn pushes_cut_at_the_readiness_boundaries_yield_one_chunk_each() {
        assert_eq!(CHUNK_LOOKAHEAD_SAMPLES, 40);
        let x = two_tone(6 * CHUNK_SAMPLES);
        let mut m = MelStream::new();
        let mut off = 0;
        for k in 0..5 {
            let cut = if k == 0 { CHUNK_SAMPLES + CHUNK_LOOKAHEAD_SAMPLES } else { CHUNK_SAMPLES };
            m.push(&x[off..off + cut - 1]);
            assert!(m.next_chunk().is_none(), "push {k}: no chunk one sample short of the cut");
            m.push(&x[off + cut - 1..off + cut]);
            assert!(m.next_chunk().is_some(), "push {k}: the cut completes a chunk");
            assert!(m.next_chunk().is_none(), "push {k}: exactly one chunk");
            off += cut;
        }
    }

    #[test]
    fn next_window_carries_the_last_nine_frames_as_context() {
        let mut m = MelStream::new();
        m.push(&two_tone(3 * CHUNK_SAMPLES));
        let a = m.next_chunk().unwrap();
        let b = m.next_chunk().unwrap();
        for k in 0..CONTEXT_FRAMES {
            for bin in 0..N_MELS {
                assert_eq!(b[[0, bin, k]], a[[0, bin, WINDOW_FRAMES - CONTEXT_FRAMES + k]]);
            }
        }
    }

    #[test]
    fn block_size_does_not_change_the_frames() {
        let x = two_tone(3 * SAMPLE_RATE);
        let mut whole = MelStream::new();
        whole.push(&x);
        let want = all_chunks(&mut whole);
        let mut pieces = MelStream::new();
        let mut got = Vec::new();
        let (mut off, mut k) = (0usize, 0usize);
        while off < x.len() {
            let n = [1usize, 37, 1_600, 8_960][k % 4].min(x.len() - off);
            pieces.push(&x[off..off + n]);
            got.extend(all_chunks(&mut pieces));
            off += n;
            k += 1;
        }
        assert_eq!(got, want);
    }

    #[test]
    fn buffer_stays_bounded_over_a_minute() {
        let second = two_tone(SAMPLE_RATE);
        let mut m = MelStream::new();
        for _ in 0..60 {
            m.push(&second);
            while m.next_chunk().is_some() {}
            assert!(m.buf.len() < 456, "buffer grew to {}", m.buf.len());
            assert!(m.pending.len() < CHUNK_FRAMES);
        }
    }

    #[test]
    fn finish_on_empty_stream_yields_one_silent_chunk() {
        let mut m = MelStream::new();
        m.finish();
        let chunks = all_chunks(&mut m);
        assert_eq!(chunks.len(), 1);
        let c = &chunks[0];
        for t in CONTEXT_FRAMES..WINDOW_FRAMES {
            for b in 0..N_MELS {
                assert!((c[[0, b, t]] - SILENT_BIN).abs() < 1e-5);
            }
        }
    }

    #[test]
    fn finish_flushes_every_real_frame() {
        let mut m = MelStream::new();
        m.push(&two_tone(CHUNK_SAMPLES + 100));
        assert_eq!(all_chunks(&mut m).len(), 1);
        m.finish();
        assert_eq!(all_chunks(&mut m).len(), 2);
    }
}
