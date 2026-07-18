//! Frame-by-frame WAV writer.
//!
//! 16 kHz mono int16 PCM. Each `write_frame` call appends one 20 ms
//! (320-sample) frame and flushes the underlying file; a sudden process kill
//! leaves a recoverable file.

use crate::error::{Error, Result};
use hound::{SampleFormat, WavSpec, WavWriter};
use std::fs::File;
use std::io::BufWriter;
use std::path::Path;
use std::time::Instant;

/// Frame size in samples for the 20 ms convention at 16 kHz.
pub const FRAME_SAMPLES: usize = 320;

/// Safety cap on leading-silence padding; a gap beyond this skips the pad.
const MAX_LEAD_PAD_SECS: u64 = 60 * 30;

pub struct WavFrameWriter {
    inner: Option<WavWriter<BufWriter<File>>>,
    sample_rate: u32,
    /// Set at create(); the amount of silence preceding the first real frame
    /// is computed from it. Every track shares a common t=0 (chunk-open
    /// time).
    created: Instant,
    started: bool,
}

impl WavFrameWriter {
    pub fn create(path: &Path, sample_rate: u32) -> Result<Self> {
        if sample_rate != 16_000 {
            return Err(Error::InvalidFrame(format!(
                "WavFrameWriter only supports 16 kHz; got {sample_rate}"
            )));
        }
        let spec = WavSpec {
            channels: 1,
            sample_rate,
            bits_per_sample: 16,
            sample_format: SampleFormat::Int,
        };
        let writer = WavWriter::create(path, spec)?;
        Ok(Self {
            inner: Some(writer),
            sample_rate,
            created: Instant::now(),
            started: false,
        })
    }

    pub fn write_frame(&mut self, frame: &[i16]) -> Result<()> {
        if frame.len() != FRAME_SAMPLES {
            return Err(Error::InvalidFrame(format!(
                "frame must be {FRAME_SAMPLES} samples; got {}",
                frame.len()
            )));
        }
        let sample_rate = self.sample_rate;
        let writer = self
            .inner
            .as_mut()
            .ok_or_else(|| Error::InvalidFrame("writer closed".into()))?;

        // Leading-silence alignment: every downstream consumer (AEC frame
        // pairing, the stereo mixdown, transcript timestamps) assumes both
        // tracks start at chunk-open. The head is padded once, on the first
        // real frame, with the silence that elapsed between chunk-open and
        // now.
        if !self.started {
            self.started = true;
            let elapsed = self.created.elapsed().as_secs_f64();
            if elapsed > 0.0 && elapsed <= MAX_LEAD_PAD_SECS as f64 {
                let pad = (elapsed * sample_rate as f64) as usize;
                if pad > 0 {
                    log::debug!("wav: padding {pad} samples ({elapsed:.2}s) of leading silence to align track start");
                    for _ in 0..pad {
                        writer.write_sample(0i16)?;
                    }
                }
            } else if elapsed > MAX_LEAD_PAD_SECS as f64 {
                log::warn!("wav: first frame {elapsed:.0}s after open exceeds {MAX_LEAD_PAD_SECS}s cap — not padding (clock glitch?)");
            }
        }

        for &s in frame {
            writer.write_sample(s)?;
        }
        writer.flush()?;
        Ok(())
    }

    pub fn close(mut self) -> Result<()> {
        if let Some(writer) = self.inner.take() {
            writer.finalize()?;
        }
        Ok(())
    }
}

impl Drop for WavFrameWriter {
    fn drop(&mut self) {
        if let Some(writer) = self.inner.take() {
            let _ = writer.finalize();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_len(path: &Path) -> u32 {
        hound::WavReader::open(path).unwrap().duration()
    }

    #[test]
    fn first_frame_pads_leading_silence_for_late_start() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("late.wav");
        let mut w = WavFrameWriter::create(&p, 16_000).unwrap();
        // Simulate a source that delivers its first frame ~200 ms after open
        // (a silent loopback tap that only starts once audio plays).
        std::thread::sleep(std::time::Duration::from_millis(200));
        w.write_frame(&[0i16; FRAME_SAMPLES]).unwrap();
        w.write_frame(&[0i16; FRAME_SAMPLES]).unwrap();
        w.close().unwrap();
        // ~200 ms @ 16 kHz ≈ 3200 samples of pad + 2 frames. Allow slack for
        // scheduling jitter, but it must be well above the un-padded 640.
        let len = read_len(&p);
        assert!(len > 2_000, "expected leading-silence pad, got {len} samples");
    }

    #[test]
    fn prompt_start_pads_negligibly() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("prompt.wav");
        let mut w = WavFrameWriter::create(&p, 16_000).unwrap();
        w.write_frame(&[0i16; FRAME_SAMPLES]).unwrap();
        w.close().unwrap();
        // First frame essentially immediate → pad is a handful of samples, not
        // a whole extra frame's worth on top.
        let len = read_len(&p);
        assert!(len < FRAME_SAMPLES as u32 + 1_600, "unexpected large pad: {len}");
    }
}
