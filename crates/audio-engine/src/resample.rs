// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Device-rate f32 PCM → 16 kHz mono i16, chunked into 320-sample frames.
//!
//! The capture shim hands us interleaved f32 at the device's native rate and
//! channel count (e.g. 48 kHz stereo). `FrameResampler` downmixes to mono,
//! resamples to 16 kHz via `rubato::FftFixedIn`, and emits exactly-`FRAME_SAMPLES`
//! (320-sample, 20 ms) mono i16 frames — the unit `WavFrameWriter` expects.
//!
//! The FFT resampler band-limits before decimating: everything above 8 kHz
//! would otherwise fold back into the band the ASR reads. It costs ~10 ms of
//! output delay (`fft_size_out / 2`), applied equally to both tracks at the
//! same device rate.
//!
//! The device rate/channel count is unknown until the first audio callback, so
//! the engine is constructed with a guess and `reconfigure_if_needed` rebuilds
//! the resampler the moment the real format arrives.

use crate::wav::FRAME_SAMPLES;

const OUTPUT_RATE: u32 = 16_000;
/// Input samples fed to rubato per process() call.
const INPUT_CHUNK: usize = 1024;

/// Full-scale float sample → i16 (clamped).
#[inline]
fn f32_to_i16(s: f32) -> i16 {
    (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16
}

pub struct FrameResampler {
    resampler: Option<rubato::FftFixedIn<f32>>,
    input_rate: u32,
    channels: u16,
    /// True when input rate already equals 16 kHz; rubato is bypassed.
    passthrough: bool,
    /// Mono f32 input at the device rate, waiting for a full `INPUT_CHUNK`.
    input_buf: Vec<f32>,
    /// Mono i16 output at 16 kHz, waiting to be emitted as 320-sample frames.
    output_accum: Vec<i16>,
    /// Mono input samples ever pushed at the current format (input rate).
    in_total: u64,
    /// Output samples ever produced at the current format, emitted or still
    /// in `output_accum`.
    out_total: u64,
}

impl FrameResampler {
    pub fn new(input_rate: u32, channels: u16) -> Self {
        let mut s = Self {
            resampler: None,
            input_rate: 0,
            channels: 0,
            passthrough: false,
            input_buf: Vec::with_capacity(INPUT_CHUNK * 4),
            output_accum: Vec::with_capacity(FRAME_SAMPLES * 4),
            in_total: 0,
            out_total: 0,
        };
        s.configure(input_rate.max(1), channels.max(1));
        s
    }

    /// Rebuild the resampler when the device reports a new rate/channel count.
    /// Drops any half-buffered input (the format boundary is a clean cut).
    /// (macOS-only today: WASAPI fixes the format at stream init.)
    #[cfg_attr(target_os = "windows", allow(dead_code))]
    pub fn reconfigure_if_needed(&mut self, input_rate: u32, channels: u16) {
        let input_rate = input_rate.max(1);
        let channels = channels.max(1);
        if input_rate == self.input_rate && channels == self.channels {
            return;
        }
        // The format boundary is a clean cut: held output (≤20 ms) goes with
        // the held input, and the sample counters restart with the stream.
        self.input_buf.clear();
        self.output_accum.clear();
        self.in_total = 0;
        self.out_total = 0;
        self.configure(input_rate, channels);
    }

    fn configure(&mut self, input_rate: u32, channels: u16) {
        self.input_rate = input_rate;
        self.channels = channels;
        if input_rate == OUTPUT_RATE {
            self.passthrough = true;
            self.resampler = None;
            return;
        }
        self.passthrough = false;
        self.resampler = rubato::FftFixedIn::<f32>::new(
            input_rate as usize,
            OUTPUT_RATE as usize,
            INPUT_CHUNK,
            1,
            1,
        )
        .ok();
    }

    /// Push interleaved device-rate f32 PCM; return any whole 16 kHz mono
    /// 320-sample frames that became available.
    pub fn push(&mut self, interleaved: &[f32]) -> Vec<Vec<i16>> {
        let ch = self.channels.max(1) as usize;
        // Downmix to mono by averaging channels.
        let mono_before = self.input_buf.len();
        if ch == 1 {
            self.input_buf.extend_from_slice(interleaved);
        } else {
            for frame in interleaved.chunks(ch) {
                let sum: f32 = frame.iter().copied().sum();
                self.input_buf.push(sum / frame.len() as f32);
            }
        }
        self.in_total += (self.input_buf.len() - mono_before) as u64;

        if self.passthrough {
            for s in self.input_buf.drain(..) {
                self.out_total += 1;
                self.output_accum.push(f32_to_i16(s));
            }
        } else if let Some(rs) = self.resampler.as_mut() {
            use rubato::Resampler;
            while self.input_buf.len() >= INPUT_CHUNK {
                let chunk: Vec<f32> = self.input_buf.drain(..INPUT_CHUNK).collect();
                if let Ok(out) = rs.process(std::slice::from_ref(&chunk.as_slice()), None) {
                    self.out_total += out[0].len() as u64;
                    for &s in &out[0] {
                        self.output_accum.push(f32_to_i16(s));
                    }
                }
            }
        } else {
            // Resampler construction failed (unreachable for real device
            // rates): drop the input instead of buffering it forever.
            self.input_buf.clear();
        }
        self.drain_frames()
    }

    /// Like [`Self::push`], but also derives the capture time of the first
    /// emitted sample from `host_ns`, the capture time of the NEWEST input
    /// sample in `interleaved`. The emitted audio trails that newest sample
    /// by whatever is still buffered (input awaiting a full resampler chunk,
    /// output awaiting a full frame) plus the FFT filter's signal delay.
    /// Output positions are mapped back to input positions by the exact
    /// rate ratio, so rubato's internal buffering needs no modelling. The
    /// returned ns is 0 when `host_ns` is 0 or no frame was emitted.
    pub fn push_stamped(&mut self, interleaved: &[f32], host_ns: u64) -> (Vec<Vec<i16>>, u64) {
        let frames = self.push(interleaved);
        let total: u64 = frames.iter().map(|f| f.len() as u64).sum();
        if host_ns == 0 || total == 0 {
            return (frames, 0);
        }
        use rubato::Resampler;
        let d_out = self.resampler.as_ref().map_or(0, |r| r.output_delay()) as i64;
        let in_rate = self.input_rate.max(1) as i64;
        // Output index of the batch's first sample, then the input index
        // whose audio it carries (the filter delays the signal by `d_out`;
        // during warm-up that index is negative — pre-roll extrapolates on
        // the same timeline rather than clamping to the first sample).
        let first_out = (self.out_total - self.output_accum.len() as u64 - total) as i64;
        let first_in = (first_out - d_out) * in_rate / OUTPUT_RATE as i64;
        let behind = (self.in_total as i64 - 1 - first_in).max(0) as u64;
        (frames, host_ns.saturating_sub(behind * 1_000_000_000 / in_rate as u64))
    }

    /// End of stream: push the part-filled input chunk through the resampler
    /// (zero-padded to a whole chunk) and emit everything left, padding the
    /// last frame with silence. Without this the tail — up to one input chunk
    /// plus a partial frame, ~40 ms — is discarded.
    pub fn flush(&mut self) -> Vec<Vec<i16>> {
        if !self.input_buf.is_empty() {
            if self.passthrough {
                for s in self.input_buf.drain(..) {
                    self.out_total += 1;
                    self.output_accum.push(f32_to_i16(s));
                }
            } else if let Some(rs) = self.resampler.as_mut() {
                use rubato::Resampler;
                let mut chunk: Vec<f32> = self.input_buf.drain(..).collect();
                chunk.resize(INPUT_CHUNK, 0.0);
                if let Ok(out) = rs.process(std::slice::from_ref(&chunk.as_slice()), None) {
                    self.out_total += out[0].len() as u64;
                    for &s in &out[0] {
                        self.output_accum.push(f32_to_i16(s));
                    }
                }
            } else {
                self.input_buf.clear();
            }
        }
        if !self.output_accum.is_empty() {
            let pad = (FRAME_SAMPLES - self.output_accum.len() % FRAME_SAMPLES) % FRAME_SAMPLES;
            self.out_total += pad as u64;
            self.output_accum.extend(std::iter::repeat_n(0, pad));
        }
        self.drain_frames()
    }

    fn drain_frames(&mut self) -> Vec<Vec<i16>> {
        let mut frames = Vec::new();
        while self.output_accum.len() >= FRAME_SAMPLES {
            frames.push(self.output_accum.drain(..FRAME_SAMPLES).collect());
        }
        frames
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passthrough_16k_mono_emits_full_frames() {
        let mut r = FrameResampler::new(16_000, 1);
        // 3 full frames + a partial tail.
        let input = vec![0.5f32; FRAME_SAMPLES * 3 + 100];
        let frames = r.push(&input);
        assert_eq!(frames.len(), 3);
        assert!(frames.iter().all(|f| f.len() == FRAME_SAMPLES));
        // Tail held until flush, then emitted zero-padded — not discarded.
        let tail = r.flush();
        assert_eq!(tail.len(), 1);
        assert_eq!(tail[0].len(), FRAME_SAMPLES);
        assert!(r.flush().is_empty());
    }

    #[test]
    fn passthrough_converts_amplitude() {
        let mut r = FrameResampler::new(16_000, 1);
        let frames = r.push(&vec![1.0f32; FRAME_SAMPLES]);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0][0], i16::MAX);
    }

    #[test]
    fn stereo_downmix_averages_channels() {
        let mut r = FrameResampler::new(16_000, 2);
        // L=1.0, R=0.0 interleaved → mono 0.5.
        let mut input = Vec::new();
        for _ in 0..FRAME_SAMPLES {
            input.push(1.0f32);
            input.push(0.0f32);
        }
        let frames = r.push(&input);
        assert_eq!(frames.len(), 1);
        let expected = f32_to_i16(0.5);
        assert!(frames[0].iter().all(|&s| s == expected));
    }

    #[test]
    fn downsample_48k_to_16k_thirds_the_samples() {
        let mut r = FrameResampler::new(48_000, 1);
        // 48k mono: feed ~1 second; expect ~16k output ≈ 50 frames (16000/320).
        let frames = r.push(&vec![0.25f32; 48_000]);
        let tail = r.flush();
        let total: usize = frames.iter().chain(tail.iter()).map(|f| f.len()).sum();
        // ~16000 output samples; allow rubato warm-up slack.
        assert!((320 * 45..=320 * 52).contains(&total), "got {total} samples");
    }

    /// 12 kHz at 48 k is above the 8 kHz Nyquist of the output: a resampler
    /// that decimates without band-limiting folds it back to 4 kHz at nearly
    /// full amplitude, right in the band the ASR reads.
    #[test]
    fn above_nyquist_input_is_filtered_not_folded() {
        let mut r = FrameResampler::new(48_000, 1);
        let tone: Vec<f32> = (0..48_000)
            .map(|n| (std::f32::consts::TAU * 12_000.0 * n as f32 / 48_000.0).sin())
            .collect();
        let mut out: Vec<i16> = Vec::new();
        for f in r.push(&tone).into_iter().chain(r.flush()) {
            out.extend(f);
        }
        // Skip the filter's warm-up; measure the steady state.
        let body = &out[1_600..out.len() - 1_600];
        let rms = (body.iter().map(|&s| (s as f64).powi(2)).sum::<f64>() / body.len() as f64).sqrt();
        // Full-scale input RMS would be ~23_170.
        assert!(rms < 1_000.0, "aliased energy at {rms:.0} RMS");
    }

    /// The implied stream epoch (batch ns minus the batch's output-sample
    /// position) must hold still across pushes.
    fn epoch_spread_ns(rate: u32, push_len: usize, pushes: usize) -> u64 {
        let mut r = FrameResampler::new(rate, 1);
        let mut newest_ns: u64 = 1_000_000_000;
        let mut emitted: u64 = 0;
        let mut epochs: Vec<i64> = Vec::new();
        for _ in 0..pushes {
            newest_ns += push_len as u64 * 1_000_000_000 / rate as u64;
            let (frames, ns) = r.push_stamped(&vec![0.1f32; push_len], newest_ns);
            let total: u64 = frames.iter().map(|f| f.len() as u64).sum();
            if total > 0 {
                assert_ne!(ns, 0);
                epochs.push(ns as i64 - (emitted * 1_000_000_000 / 16_000) as i64);
            }
            emitted += total;
        }
        assert!(epochs.len() > 10, "too few stamped batches to judge");
        (epochs.iter().max().unwrap() - epochs.iter().min().unwrap()) as u64
    }

    #[test]
    fn stamped_pairing_stays_put_across_uneven_pushes() {
        // Two output samples of slack for the rounding of the ns arithmetic.
        let tol = 2 * 1_000_000_000 / 16_000;
        // macOS tap/AUHAL shape: 512-sample callbacks at 48 kHz.
        assert!(epoch_spread_ns(48_000, 512, 200) <= tol);
        // Callback size that never divides the resampler chunk.
        assert!(epoch_spread_ns(48_000, 480, 200) <= tol);
        // Passthrough at 16 kHz with a frame-misaligned push size.
        assert!(epoch_spread_ns(16_000, 100, 400) <= tol);
    }

    #[test]
    fn push_stamped_zero_stamp_stays_zero() {
        let mut r = FrameResampler::new(48_000, 1);
        let (frames, ns) = r.push_stamped(&vec![0.1f32; 4_800], 0);
        assert!(!frames.is_empty());
        assert_eq!(ns, 0);
    }

    #[test]
    fn reconfigure_switches_rate() {
        let mut r = FrameResampler::new(16_000, 1);
        assert!(r.passthrough);
        r.reconfigure_if_needed(48_000, 2);
        assert!(!r.passthrough);
        assert_eq!(r.channels, 2);
        // Idempotent: same params don't rebuild/clear mid-stream.
        r.push(&vec![0.1f32; 100]);
        let before = r.input_buf.len();
        r.reconfigure_if_needed(48_000, 2);
        assert_eq!(r.input_buf.len(), before);
    }
}
