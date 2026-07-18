//! Direction of a cross-track echo pair, decided acoustically.
//!
//! An echo lags its source. For a mic/system near-duplicate pair, the sign of
//! the GCC-PHAT cross-correlation peak lag between the two tracks over the
//! pair's time span says which side is the original:
//!
//! - system earlier (negative lag) → speaker→mic bleed; the mic copy is echo.
//! - mic earlier (positive lag) → the local voice came back through the call;
//!   the system copy is echo.
//!
//! A weak correlation peak decides nothing (`Unknown`), and the caller falls
//! back to the one-directional drop-the-mic-copy behavior. The mic track fed
//! in is the AEC one when available, which biases detection exactly the safe
//! way: bleed residue in the mic is AEC-attenuated (weak peak → `Unknown` →
//! mic dropped as before), while a genuine reverse echo keeps the local voice
//! at full strength in the mic and correlates strongly.

use crate::promote::{ChunkSpan, LiveSeg};
use rustfft::num_complex::Complex;
use rustfft::FftPlanner;
use std::cell::RefCell;
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EchoDirection {
    /// Mic copy is the echo (speaker→mic bleed). Matches the legacy behavior.
    MicIsEcho,
    /// System copy is the echo (local voice returned through the call).
    SystemIsEcho,
    /// Correlation too weak or audio unavailable — caller keeps legacy behavior.
    Unknown,
}

/// Round-trip echo through a call stays well under this.
const MAX_LAG_S: f32 = 3.0;
/// Peak must stand this far above the mean |correlation| to be trusted
/// (validated sessions measured ≥28×).
const MIN_PEAK_RATIO: f32 = 10.0;
/// Lags inside ±this are same-instant decode jitter, not an echo path.
const MIN_LAG_MS: f32 = 20.0;
/// Cap the analyzed span; enough context for a decisive peak.
const MAX_SPAN_S: f32 = 20.0;

/// Lag (seconds) of `system` relative to `mic` at the strongest PHAT peak,
/// with the peak-to-mean ratio. Positive lag = system later.
fn gcc_phat(mic: &[f32], system: &[f32], sample_rate: u32) -> Option<(f32, f32)> {
    let max_lag = (MAX_LAG_S * sample_rate as f32) as usize;
    let len = mic.len().max(system.len());
    if len == 0 || max_lag == 0 {
        return None;
    }
    let n = (len + max_lag).next_power_of_two();
    let mut planner = FftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(n);
    let ifft = planner.plan_fft_inverse(n);

    let mut a: Vec<Complex<f32>> =
        mic.iter().map(|&x| Complex::new(x, 0.0)).chain(std::iter::repeat(Complex::ZERO)).take(n).collect();
    let mut b: Vec<Complex<f32>> =
        system.iter().map(|&x| Complex::new(x, 0.0)).chain(std::iter::repeat(Complex::ZERO)).take(n).collect();
    fft.process(&mut a);
    fft.process(&mut b);

    // PHAT weighting: keep phase only — robust to level/spectral differences.
    let mut r: Vec<Complex<f32>> = a
        .iter()
        .zip(&b)
        .map(|(x, y)| {
            let c = y * x.conj();
            let m = c.norm();
            if m > 1e-12 { c / m } else { Complex::ZERO }
        })
        .collect();
    ifft.process(&mut r);

    // Circular correlation: lags [-max_lag, max_lag] live at the wrap point.
    let mut best_idx = 0usize;
    let mut best = 0.0f32;
    let mut sum = 0.0f32;
    let mut count = 0usize;
    for (k, idx) in (0..=2 * max_lag).map(|k| (k, (n - max_lag + k) % n)) {
        let v = r[idx].norm();
        sum += v;
        count += 1;
        if v > best {
            best = v;
            best_idx = k;
        }
    }
    let mean = sum / count.max(1) as f32;
    if mean <= 0.0 {
        return None;
    }
    let lag_s = (best_idx as f32 - max_lag as f32) / sample_rate as f32;
    Some((lag_s, best / mean))
}

pub fn direction_from_audio(mic: &[f32], system: &[f32], sample_rate: u32) -> EchoDirection {
    match gcc_phat(mic, system, sample_rate) {
        Some((lag, ratio)) if ratio >= MIN_PEAK_RATIO => {
            let lag_ms = lag * 1000.0;
            if lag_ms > MIN_LAG_MS {
                EchoDirection::SystemIsEcho
            } else if lag_ms < -MIN_LAG_MS {
                EchoDirection::MicIsEcho
            } else {
                EchoDirection::Unknown
            }
        }
        _ => EchoDirection::Unknown,
    }
}

/// WAV-backed oracle over a session's chunk files. Tracks load lazily, once
/// per chunk. Any IO/format problem yields `Unknown` — never an error.
pub struct WavOracle<'a> {
    chunks: &'a [ChunkSpan],
    cache: RefCell<HashMap<u32, Option<(Vec<f32>, Vec<f32>, u32)>>>,
}

impl<'a> WavOracle<'a> {
    pub fn new(chunks: &'a [ChunkSpan]) -> Self {
        Self { chunks, cache: RefCell::new(HashMap::new()) }
    }

    fn load(path: &std::path::Path) -> Option<(Vec<f32>, u32)> {
        let mut r = hound::WavReader::open(path).ok()?;
        let spec = r.spec();
        let samples: Vec<f32> = match spec.sample_format {
            hound::SampleFormat::Int => {
                let scale = 1.0 / (1i64 << (spec.bits_per_sample - 1)) as f32;
                r.samples::<i32>().filter_map(Result::ok).map(|s| s as f32 * scale).collect()
            }
            hound::SampleFormat::Float => r.samples::<f32>().filter_map(Result::ok).collect(),
        };
        if spec.channels > 1 {
            let ch = spec.channels as usize;
            return Some((
                samples.chunks(ch).map(|f| f.iter().sum::<f32>() / ch as f32).collect(),
                spec.sample_rate,
            ));
        }
        Some((samples, spec.sample_rate))
    }

    pub fn direction(&self, mic: &LiveSeg, system: &LiveSeg) -> EchoDirection {
        // Both segments must land in one chunk; cross-chunk pairs stay Unknown.
        let lo = mic.start_ms.min(system.start_ms);
        let hi = mic.end_ms.max(system.end_ms);
        let Some(ch) = self.chunks.iter().rev().find(|c| c.start_ms <= lo) else {
            return EchoDirection::Unknown;
        };
        let mut cache = self.cache.borrow_mut();
        let entry = cache.entry(ch.index).or_insert_with(|| {
            let (m, sr) = Self::load(&ch.mic_wav)?;
            let (s, sr2) = Self::load(&ch.system_wav)?;
            (sr == sr2).then_some((m, s, sr))
        });
        let Some((m, s, sr)) = entry.as_ref() else {
            return EchoDirection::Unknown;
        };
        let rel_lo = (lo - ch.start_ms) as usize * *sr as usize / 1000;
        let span_ms = (hi - lo).min((MAX_SPAN_S * 1000.0) as u32) as usize;
        let rel_hi = rel_lo + span_ms * *sr as usize / 1000;
        let mic_a = &m[rel_lo.min(m.len())..rel_hi.min(m.len())];
        let sys_a = &s[rel_lo.min(s.len())..rel_hi.min(s.len())];
        if mic_a.is_empty() || sys_a.is_empty() {
            return EchoDirection::Unknown;
        }
        direction_from_audio(mic_a, sys_a, *sr)
    }
}

/// Fraction of sampled windows where the two tracks are strongly correlated
/// at an acoustic-echo lag — i.e. how much of the meeting one track spent
/// replaying the other. Near 0 on a headphone session; ~1.0 when the mic
/// heard the speakers the whole time. Finalize gates live-promotion on it:
/// wholesale bleed fragments the two live decodes differently enough that
/// text dedup cannot re-pair them, while the whisper full pass on the AEC
/// track never sees the echo at all.
pub fn bleed_coverage(chunks: &[ChunkSpan], windows: usize) -> f32 {
    const WIN_S: usize = 6;
    const MIN_RMS: f32 = 0.003;
    const MIN_STRENGTH: f32 = 20.0;
    const MAX_ECHO_LAG_S: f32 = 0.5;
    let mut sampled = 0usize;
    let mut correlated = 0usize;
    for ch in chunks {
        let Some((m, sr)) = WavOracle::load(&ch.mic_wav) else { continue };
        let Some((s, sr2)) = WavOracle::load(&ch.system_wav) else { continue };
        if sr != sr2 {
            continue;
        }
        let sr = sr as usize;
        let len = m.len().min(s.len());
        let step = (len / windows.max(1)).max(WIN_S * sr);
        let mut at = 0usize;
        while at + WIN_S * sr <= len {
            let a = &m[at..at + WIN_S * sr];
            let b = &s[at..at + WIN_S * sr];
            at += step;
            let rms = |x: &[f32]| (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt();
            if rms(a) < MIN_RMS || rms(b) < MIN_RMS {
                continue; // silence on either side says nothing
            }
            sampled += 1;
            if let Some((lag, strength)) = gcc_phat(a, b, sr as u32) {
                let lag_ms = lag.abs() * 1000.0;
                if strength >= MIN_STRENGTH && (20.0..=MAX_ECHO_LAG_S * 1000.0).contains(&lag_ms) {
                    correlated += 1;
                }
            }
        }
    }
    if sampled == 0 {
        return 0.0;
    }
    correlated as f32 / sampled as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Speech-like burst: sum of enveloped tones, zero-padded into `total` samples at `at`.
    fn burst_at(total: usize, at: usize, sr: u32) -> Vec<f32> {
        let mut v = vec![0.0f32; total];
        let dur = sr as usize / 2;
        for i in 0..dur {
            let t = i as f32 / sr as f32;
            let env = (std::f32::consts::PI * i as f32 / dur as f32).sin();
            let x = (2.0 * std::f32::consts::PI * 220.0 * t).sin()
                + 0.5 * (2.0 * std::f32::consts::PI * 470.0 * t).sin()
                + 0.25 * (2.0 * std::f32::consts::PI * 1130.0 * t).sin();
            if at + i < total {
                v[at + i] = env * x * 0.5;
            }
        }
        v
    }

    #[test]
    fn detects_system_echo_of_mic() {
        let sr = 16_000u32;
        let n = sr as usize * 4;
        let mic = burst_at(n, 1000, sr);
        let system = burst_at(n, 1000 + sr as usize / 2, sr); // system 500ms later
        assert_eq!(direction_from_audio(&mic, &system, sr), EchoDirection::SystemIsEcho);
    }

    #[test]
    fn detects_mic_bleed_of_system() {
        let sr = 16_000u32;
        let n = sr as usize * 4;
        let system = burst_at(n, 1000, sr);
        let mic = burst_at(n, 1000 + sr as usize / 8, sr); // mic 125ms later
        assert_eq!(direction_from_audio(&mic, &system, sr), EchoDirection::MicIsEcho);
    }

    #[test]
    fn uncorrelated_audio_is_unknown() {
        let sr = 16_000u32;
        let n = sr as usize * 2;
        // Deterministic pseudo-noise, different seeds per track.
        let noise = |seed: u32| -> Vec<f32> {
            let mut x = seed;
            (0..n)
                .map(|_| {
                    x = x.wrapping_mul(1664525).wrapping_add(1013904223);
                    (x >> 16) as f32 / 32768.0 - 1.0
                })
                .collect()
        };
        assert_eq!(direction_from_audio(&noise(1), &noise(2), sr), EchoDirection::Unknown);
    }
}
