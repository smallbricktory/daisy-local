//! Read a mono i16 WAV once, then compute RMS-in-dBFS over arbitrary
//! [start_ms, end_ms) windows. The whole file is held in memory.

use crate::error::{Result, TranscriptError};
use std::path::Path;

pub struct WavSamples {
    samples: Vec<i16>,
    sample_rate: u32,
}

impl WavSamples {
    pub fn load(path: &Path) -> Result<Self> {
        let mut r = hound::WavReader::open(path).map_err(|e| TranscriptError::Io {
            path: path.to_path_buf(),
            source: std::io::Error::new(std::io::ErrorKind::Other, e.to_string()),
        })?;
        let spec = r.spec();
        let samples: std::result::Result<Vec<i16>, hound::Error> = r.samples::<i16>().collect();
        let samples = samples.map_err(|e| TranscriptError::Io {
            path: path.to_path_buf(),
            source: std::io::Error::new(std::io::ErrorKind::Other, e.to_string()),
        })?;
        Ok(Self {
            samples,
            sample_rate: spec.sample_rate,
        })
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }
    pub fn len(&self) -> usize {
        self.samples.len()
    }
    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    #[cfg(test)]
    pub(crate) fn from_raw(samples: Vec<i16>, sample_rate: u32) -> Self {
        Self { samples, sample_rate }
    }
}

/// Silence threshold used by `is_silent_wav`. Inputs whose overall RMS falls
/// below this level are treated as silent.
pub const SILENCE_THRESHOLD_DBFS: f32 = -65.0;

/// Returns `true` if the WAV file's overall RMS is below
/// [`SILENCE_THRESHOLD_DBFS`].
pub fn is_silent_wav(path: &Path) -> crate::error::Result<bool> {
    let wav = WavSamples::load(path)?;
    let db = rms_dbfs_window(&wav, 0, u32::MAX);
    Ok(db < SILENCE_THRESHOLD_DBFS)
}

/// Peak |sample| over [start_ms, end_ms), in dBFS (full-scale = i16::MAX).
/// Returns -120.0 for empty/out-of-range/all-zero windows.
pub fn peak_dbfs_window(wav: &WavSamples, start_ms: u32, end_ms: u32) -> f32 {
    if end_ms <= start_ms {
        return -120.0;
    }
    let sr = wav.sample_rate as u64;
    let start = ((start_ms as u64) * sr / 1000) as usize;
    let end = (((end_ms as u64) * sr / 1000) as usize).min(wav.samples.len());
    if start >= end {
        return -120.0;
    }
    let peak = wav.samples[start..end]
        .iter()
        .map(|&s| (s as i32).unsigned_abs())
        .max()
        .unwrap_or(0);
    if peak == 0 {
        return -120.0;
    }
    20.0 * ((peak as f32) / (i16::MAX as f32)).log10()
}

/// RMS over [start_ms, end_ms), expressed in dBFS (full-scale = i16::MAX).
/// Returns roughly -120 dB for empty/out-of-range windows.
pub fn rms_dbfs_window(wav: &WavSamples, start_ms: u32, end_ms: u32) -> f32 {
    if end_ms <= start_ms {
        return -120.0;
    }
    let sr = wav.sample_rate as u64;
    let start = ((start_ms as u64) * sr / 1000) as usize;
    let end = ((end_ms as u64) * sr / 1000) as usize;
    let end = end.min(wav.samples.len());
    if start >= end {
        return -120.0;
    }
    let slice = &wav.samples[start..end];
    let n = slice.len() as f64;
    let sum_sq: f64 = slice.iter().map(|&s| (s as f64).powi(2)).sum();
    let mean_sq = sum_sq / n;
    let rms = mean_sq.sqrt();
    let full_scale = i16::MAX as f64;
    if rms < 1e-9 {
        -120.0
    } else {
        (20.0 * (rms / full_scale).log10()) as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peak_dbfs_window_measures_peak_not_rms() {
        let mut s = vec![0i16; 16_000];
        s[8_000] = i16::MAX;
        let w = WavSamples::from_raw(s, 16_000);
        let peak = peak_dbfs_window(&w, 0, 1_000);
        assert!(peak > -0.1, "full-scale spike is ~0 dBFS, got {peak}");
        assert!(rms_dbfs_window(&w, 0, 1_000) < -30.0);
    }

    #[test]
    fn peak_dbfs_window_scales_linearly() {
        let v = (0.02 * i16::MAX as f32) as i16;
        let w = WavSamples::from_raw(vec![v; 16_000], 16_000);
        let peak = peak_dbfs_window(&w, 0, 1_000);
        assert!((peak - (-33.98)).abs() < 0.5, "got {peak}");
    }

    #[test]
    fn peak_dbfs_window_empty_and_out_of_range() {
        let w = WavSamples::from_raw(vec![0i16; 1_600], 16_000);
        assert_eq!(peak_dbfs_window(&w, 500, 500), -120.0);
        assert_eq!(peak_dbfs_window(&w, 5_000, 6_000), -120.0);
        assert_eq!(peak_dbfs_window(&w, 0, 100), -120.0);
    }
}
