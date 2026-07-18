//! Device-rate f32 PCM → 16 kHz mono i16, chunked into 320-sample frames.
//!
//! The capture shim hands us interleaved f32 at the device's native rate and
//! channel count (e.g. 48 kHz stereo). `FrameResampler` downmixes to mono,
//! resamples to 16 kHz via `rubato::FastFixedIn`, and emits exactly-`FRAME_SAMPLES`
//! (320-sample, 20 ms) mono i16 frames — the unit `WavFrameWriter` expects.
//!
//! The device rate/channel count is unknown until the first audio callback, so
//! the engine is constructed with a guess and `reconfigure_if_needed` rebuilds
//! the resampler the moment the real format arrives.

use crate::wav::FRAME_SAMPLES;

const OUTPUT_RATE: u32 = 16_000;
/// Input samples fed to rubato per process() call.
const INPUT_CHUNK: usize = 1024;

pub struct FrameResampler {
    resampler: Option<rubato::FastFixedIn<f32>>,
    input_rate: u32,
    channels: u16,
    /// True when input rate already equals 16 kHz; rubato is bypassed.
    passthrough: bool,
    /// Mono f32 input at the device rate, waiting for a full `INPUT_CHUNK`.
    input_buf: Vec<f32>,
    /// Mono i16 output at 16 kHz, waiting to be emitted as 320-sample frames.
    output_accum: Vec<i16>,
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
        };
        s.configure(input_rate.max(1), channels.max(1));
        s
    }

    /// Rebuild the resampler when the device reports a new rate/channel count.
    /// Drops any half-buffered input (the format boundary is a clean cut).
    pub fn reconfigure_if_needed(&mut self, input_rate: u32, channels: u16) {
        let input_rate = input_rate.max(1);
        let channels = channels.max(1);
        if input_rate == self.input_rate && channels == self.channels {
            return;
        }
        self.input_buf.clear();
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
        self.resampler = rubato::FastFixedIn::<f32>::new(
            OUTPUT_RATE as f64 / input_rate as f64,
            1.0,
            rubato::PolynomialDegree::Linear,
            INPUT_CHUNK,
            1,
        )
        .ok();
    }

    /// Push interleaved device-rate f32 PCM; return any whole 16 kHz mono
    /// 320-sample frames that became available.
    pub fn push(&mut self, interleaved: &[f32]) -> Vec<Vec<i16>> {
        let ch = self.channels.max(1) as usize;
        // Downmix to mono by averaging channels.
        if ch == 1 {
            self.input_buf.extend_from_slice(interleaved);
        } else {
            for frame in interleaved.chunks(ch) {
                let sum: f32 = frame.iter().copied().sum();
                self.input_buf.push(sum / frame.len() as f32);
            }
        }

        if self.passthrough {
            for s in self.input_buf.drain(..) {
                self.output_accum.push(f32_to_i16(s));
            }
        } else if let Some(rs) = self.resampler.as_mut() {
            use rubato::Resampler;
            while self.input_buf.len() >= INPUT_CHUNK {
                let chunk: Vec<f32> = self.input_buf.drain(..INPUT_CHUNK).collect();
                if let Ok(out) = rs.process(std::slice::from_ref(&chunk.as_slice()), None) {
                    for &s in &out[0] {
                        self.output_accum.push(f32_to_i16(s));
                    }
                }
            }
        }
        self.drain_frames()
    }

    /// Emit any whole frames still held. Sub-frame tail samples are dropped.
    pub fn flush(&mut self) -> Vec<Vec<i16>> {
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

#[inline]
fn f32_to_i16(s: f32) -> i16 {
    (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16
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
        // Tail held until flush; flush drops sub-frame remainder.
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
        assert!(total >= 320 * 45 && total <= 320 * 52, "got {total} samples");
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
