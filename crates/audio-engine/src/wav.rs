// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
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

/// Minimum mid-stream stamp deficit (samples) that inserts a silence fill.
/// Below this, stamp jitter and device-clock drift are left alone; a real
/// capture stall (seconds) clears it by orders of magnitude.
const GAP_FILL_MIN_SAMPLES: u64 = 3_200;

/// Now, in the same clock domain the capture backends stamp frames with:
/// CLOCK_MONOTONIC on Linux (PipeWire's `pw_time`), mach host time on macOS
/// (the shim's `mHostTime` through `mach_timebase_info`), QPC on Windows
/// (WASAPI's `QPCPosition`). Returns 0 when unavailable; stamp-driven
/// padding is then disabled and the arrival-time fallback applies.
fn host_now_ns() -> u64 {
    #[cfg(target_os = "linux")]
    {
        let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
        if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) } != 0 {
            return 0;
        }
        ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
    }
    #[cfg(target_os = "macos")]
    {
        let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
        if unsafe { libc::clock_gettime(libc::CLOCK_UPTIME_RAW, &mut ts) } != 0 {
            return 0;
        }
        ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
    }
    #[cfg(target_os = "windows")]
    {
        use windows::Win32::System::Performance::{
            QueryPerformanceCounter, QueryPerformanceFrequency,
        };
        let mut counter: i64 = 0;
        let mut freq: i64 = 0;
        unsafe {
            let _ = QueryPerformanceFrequency(&mut freq);
            let _ = QueryPerformanceCounter(&mut counter);
        }
        if freq <= 0 || counter <= 0 {
            return 0;
        }
        (counter as u128 * 1_000_000_000 / freq as u128) as u64
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        0
    }
}

pub struct WavFrameWriter {
    inner: Option<WavWriter<BufWriter<File>>>,
    sample_rate: u32,
    /// Set at create(); the amount of silence preceding the first real frame
    /// is computed from it. Every track shares a common t=0 (chunk-open
    /// time). Arrival-time fallback for unstamped frames.
    created: Instant,
    /// Chunk-open reference in the capture-stamp clock domain (see
    /// [`host_now_ns`]). With stamped frames, silence padding follows
    /// `frame_ns - created_ns` — one clock, so a queued/backlogged delivery
    /// or a mid-stream capture stall cannot skew track alignment.
    created_ns: u64,
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
            created_ns: host_now_ns(),
            started: false,
        })
    }

    pub fn write_frame(&mut self, frame: &[i16]) -> Result<()> {
        self.write_frame_stamped(frame, 0)
    }

    /// Append one frame whose first sample was captured at `frame_ns`
    /// (capture-backend clock domain; 0 = unstamped). Silence alignment:
    /// every downstream consumer (AEC frame pairing, the stereo mixdown,
    /// transcript timestamps) assumes both tracks start at chunk-open and
    /// stay time-contiguous. With stamps, the frame is placed at
    /// `frame_ns - created_ns` — leading silence pads a late-starting
    /// stream, and a mid-stream capture stall is filled with silence so
    /// later audio keeps its true position. Unstamped frames fall back to
    /// arrival-time leading padding only.
    pub fn write_frame_stamped(&mut self, frame: &[i16], frame_ns: u64) -> Result<()> {
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

        if frame_ns != 0 && self.created_ns != 0 {
            // Stamp-driven placement, both times in one clock domain.
            let target = (frame_ns.saturating_sub(self.created_ns) as u128
                * sample_rate as u128
                / 1_000_000_000) as u64;
            let deficit = target.saturating_sub(writer.len() as u64);
            let threshold = if self.started { GAP_FILL_MIN_SAMPLES } else { 1 };
            if deficit >= threshold {
                let secs = deficit as f64 / sample_rate as f64;
                if deficit <= MAX_LEAD_PAD_SECS * sample_rate as u64 {
                    if self.started {
                        log::info!("wav: filling {deficit} samples ({secs:.2}s) of silence for a capture gap");
                    } else {
                        log::debug!("wav: padding {deficit} samples ({secs:.2}s) of leading silence to align track start");
                    }
                    for _ in 0..deficit {
                        writer.write_sample(0i16)?;
                    }
                } else {
                    log::warn!("wav: stamp gap {secs:.0}s exceeds {MAX_LEAD_PAD_SECS}s cap — not padding (clock glitch?)");
                }
            }
            self.started = true;
        } else if !self.started {
            // Unstamped: pad the head once from arrival time.
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

    #[cfg(test)]
    fn set_created_ns(&mut self, ns: u64) {
        self.created_ns = ns;
    }

    /// Samples written so far, INCLUDING silence pads and gap fills. This is
    /// the file offset a consumer needs to place matching audio (e.g. the
    /// live-cancelled mic) at the same position as the raw track — the pads
    /// are clock-derived and cannot be recomputed after the fact.
    pub fn samples_written(&self) -> u64 {
        self.inner.as_ref().map(|w| w.len() as u64).unwrap_or(0)
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
    fn stamped_late_start_pads_from_capture_clock_not_arrival() {
        // Chunk opens at T (capture clock); the stream's first frame is
        // captured 90.4s later but ARRIVES immediately (queued backlog /
        // skewed arrival clock). The pad must follow the monotonic stamps,
        // sample-exact.
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("stamped.wav");
        let mut w = WavFrameWriter::create(&p, 16_000).unwrap();
        let t0: u64 = 5_000_000_000_000;
        w.set_created_ns(t0);
        w.write_frame_stamped(&[1i16; FRAME_SAMPLES], t0 + 90_400_000_000).unwrap();
        w.close().unwrap();
        let expected = (90.4 * 16_000.0) as u32 + FRAME_SAMPLES as u32;
        assert_eq!(read_len(&p), expected, "pad must equal the stamp delta exactly");
    }

    #[test]
    fn stamped_mid_stream_stall_is_gap_filled() {
        // Frames at T and T+20ms, then the capture stalls and resumes at
        // T+10s (the mid-call handoff shape): the resumed frame must land at
        // its true position, with silence filling the stall.
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("gap.wav");
        let mut w = WavFrameWriter::create(&p, 16_000).unwrap();
        let t0: u64 = 5_000_000_000_000;
        let frame_ns = FRAME_SAMPLES as u64 * 1_000_000_000 / 16_000;
        w.set_created_ns(t0);
        w.write_frame_stamped(&[1i16; FRAME_SAMPLES], t0).unwrap();
        w.write_frame_stamped(&[1i16; FRAME_SAMPLES], t0 + frame_ns).unwrap();
        w.write_frame_stamped(&[1i16; FRAME_SAMPLES], t0 + 10_000_000_000).unwrap();
        w.close().unwrap();
        let expected = 10 * 16_000 + FRAME_SAMPLES as u32;
        assert_eq!(read_len(&p), expected, "stall must be silence-filled to the stamp position");
    }

    #[test]
    fn stamped_jitter_below_threshold_is_left_alone() {
        // A 150 ms stamp hiccup is jitter, not a stall — no fill.
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("jitter.wav");
        let mut w = WavFrameWriter::create(&p, 16_000).unwrap();
        let t0: u64 = 5_000_000_000_000;
        w.set_created_ns(t0);
        w.write_frame_stamped(&[1i16; FRAME_SAMPLES], t0).unwrap();
        w.write_frame_stamped(&[1i16; FRAME_SAMPLES], t0 + 150_000_000).unwrap();
        w.close().unwrap();
        assert_eq!(read_len(&p), 2 * FRAME_SAMPLES as u32);
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
