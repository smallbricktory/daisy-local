// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
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

/// Structural misalignment between a chunk's two WAVs, in samples: how many
/// samples LATER the same instant sits in `system.wav` than in `mic.wav`.
/// 0 = aligned (or not decidable). Derived from the manifest's per-chunk
/// capture stamps + sample counts and the decoded file lengths — no audio
/// content is inspected. Two estimators cross-check each other:
///
/// - head-anchored: `(system pad − mic pad) − (system_host_ns − mic_host_ns)`,
///   where each pad is `file length − samples captured`. Exact when a track
///   has no internal capture gaps.
/// - end-anchored: `system len − mic len`. Exact when both writers closed
///   together (the normal chunk close), regardless of internal gaps.
///
/// When they agree the offset is a consistent global shift. When they
/// disagree (a mid-chunk capture stall on one track, or a track that died
/// early), the end-anchored value wins if it is small enough to be a stall
/// (≤30 s) — that aligns the co-closed tail, where the far-end audio lives —
/// otherwise the head-anchored one, otherwise 0.
pub fn stamp_align_offset(
    mic_len: usize,
    sys_len: usize,
    mic_host_ns: Option<u64>,
    system_host_ns: Option<u64>,
    mic_samples: Option<u64>,
    system_samples: Option<u64>,
    sample_rate: u32,
) -> i64 {
    let (Some(mic_ns), Some(sys_ns), Some(mic_n), Some(sys_n)) =
        (mic_host_ns, system_host_ns, mic_samples, system_samples)
    else {
        return 0;
    };
    if mic_ns == 0 || sys_ns == 0 || sample_rate == 0 || mic_len == 0 || sys_len == 0 {
        return 0;
    }
    let sr = sample_rate as i128;
    let agree_tol = (sample_rate / 10) as i128;
    let stall_cap = 30 * sr;
    let hard_cap = 120 * sr;

    let m_tail = sys_len as i128 - mic_len as i128;
    let pad_m = (mic_len as i128 - mic_n as i128).max(0);
    let pad_s = (sys_len as i128 - sys_n as i128).max(0);
    let stamp_delta = (sys_ns as i128 - mic_ns as i128) * sr / 1_000_000_000;
    let m_head = (pad_s - pad_m) - stamp_delta;

    let m = if (m_tail - m_head).abs() <= agree_tol || m_tail.abs() <= stall_cap {
        m_tail
    } else if m_head.abs() <= stall_cap {
        m_head
    } else {
        0
    };
    if m.abs() > hard_cap || m.abs() >= mic_len.min(sys_len) as i128 {
        return 0;
    }
    m as i64
}

/// Lag (seconds) of `system` relative to `mic` at the strongest PHAT peak,
/// with the peak-to-mean ratio. Positive lag = system later (argument order
/// into the shared core carries the sign convention).
fn gcc_phat(mic: &[f32], system: &[f32], sample_rate: u32) -> Option<(f32, f32)> {
    let max_lag = (MAX_LAG_S * sample_rate as f32) as usize;
    let (lag, ratio) = xcorr::gcc_phat_core(system, mic, max_lag, None)?;
    Some((lag as f32 / sample_rate as f32, ratio))
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
    cache: RefCell<HashMap<u32, Option<(Vec<f32>, Vec<f32>, u32, i64)>>>,
}

impl<'a> WavOracle<'a> {
    pub fn new(chunks: &'a [ChunkSpan]) -> Self {
        Self { chunks, cache: RefCell::new(HashMap::new()) }
    }

    fn load(path: &std::path::Path) -> Option<(Vec<f32>, u32)> {
        crate::wav_io::read_wav_mono_f32(path).ok()
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
            if sr != sr2 {
                return None;
            }
            let off = stamp_align_offset(
                m.len(),
                s.len(),
                ch.mic_host_ns,
                ch.system_host_ns,
                ch.mic_samples,
                ch.system_samples,
                sr,
            );
            Some((m, s, sr, off))
        });
        let Some((m, s, sr, off)) = entry.as_ref() else {
            return EchoDirection::Unknown;
        };
        let rel_lo = (lo - ch.start_ms) as usize * *sr as usize / 1000;
        let span_ms = (hi - lo).min((MAX_SPAN_S * 1000.0) as u32) as usize;
        let rel_hi = rel_lo + span_ms * *sr as usize / 1000;
        // The system window is pre-shifted by the structural offset, so the
        // GCC-PHAT ±MAX_LAG_S search runs centered on it — total reach =
        // stamp offset ± MAX_LAG_S. Offset 0 (aligned or no stamps) is the
        // legacy behavior.
        let s_lo = (rel_lo as i64 + off).clamp(0, s.len() as i64) as usize;
        let s_hi = (rel_hi as i64 + off).clamp(0, s.len() as i64) as usize;
        let mic_a = &m[rel_lo.min(m.len())..rel_hi.min(m.len())];
        let sys_a = &s[s_lo..s_hi];
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
        let off = stamp_align_offset(
            m.len(),
            s.len(),
            ch.mic_host_ns,
            ch.system_host_ns,
            ch.mic_samples,
            ch.system_samples,
            sr,
        );
        let sr = sr as usize;
        let len = m.len().min(s.len());
        let step = (len / windows.max(1)).max(WIN_S * sr);
        let mut at = 0usize;
        while at + WIN_S * sr <= len {
            let a = &m[at..at + WIN_S * sr];
            // System windows ride the structural offset so a misaligned
            // legacy chunk still correlates at an acoustic-echo lag.
            let b_at = (at as i64 + off).clamp(0, s.len() as i64) as usize;
            let b_end = b_at + WIN_S * sr;
            at += step;
            if b_end > s.len() {
                continue;
            }
            let b = &s[b_at..b_end];
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
    fn stamp_offset_absent_or_zeroed_inputs_decide_nothing() {
        assert_eq!(stamp_align_offset(1000, 1000, None, None, None, None, 16_000), 0);
        assert_eq!(
            stamp_align_offset(1000, 1000, Some(0), Some(5), Some(900), Some(900), 16_000),
            0
        );
        assert_eq!(
            stamp_align_offset(0, 1000, Some(5), Some(5), Some(0), Some(900), 16_000),
            0
        );
    }

    #[test]
    fn stamp_offset_consistent_pad_error_is_a_global_shift() {
        // Legacy leading-pad overshoot on the system track: both estimators
        // agree at +64,000 samples (4 s).
        let m = stamp_align_offset(
            160_000,
            224_000,
            Some(1_000),
            Some(1_000),
            Some(160_000),
            Some(160_000),
            16_000,
        );
        assert_eq!(m, 64_000);
    }

    #[test]
    fn stamp_offset_mid_chunk_stall_uses_the_end_anchor() {
        // Synthetic session shaped like a mid-call handoff: the mic lost
        // ~4.15 s of samples mid-chunk, the system started 90.4 s late with
        // an arrival-correct pad. Head estimator says ~0 (the head IS
        // aligned); end estimator carries the stall. End wins: the far-end
        // audio lives in the co-closed tail.
        let m = stamp_align_offset(
            5_086_208,
            5_152_662,
            Some(1_000),
            Some(1_000 + 90_412_195_083),
            Some(5_085_760),
            Some(3_704_960),
            16_000,
        );
        assert_eq!(m, 5_152_662 - 5_086_208);
    }

    #[test]
    fn stamp_offset_dead_track_falls_back_to_the_head_anchor() {
        // System stream died 3 minutes before the chunk closed: the length
        // difference is bogus as an alignment (too large for a stall); the
        // head estimator (~0) applies.
        let m = stamp_align_offset(
            4_800_000,
            1_920_000,
            Some(1_000),
            Some(1_000),
            Some(4_799_680),
            Some(1_919_680),
            16_000,
        );
        assert_eq!(m, 0);
    }

    #[test]
    fn oracle_finds_direction_across_a_stamp_offset_beyond_max_lag() {
        // System content sits 4.2 s later in its file than the mic's
        // (structural), plus a 0.5 s acoustic echo lag. Raw GCC-PHAT
        // (±MAX_LAG_S = 3 s) cannot reach it; the stamp-seeded pre-shift
        // centers the search and the 0.5 s residual decides the direction.
        let sr = 16_000u32;
        let structural = 67_200usize; // 4.2 s
        let acoustic = 8_000usize; // 0.5 s
        let dir = tempfile::tempdir().unwrap();
        let mic_p = dir.path().join("mic.wav");
        let sys_p = dir.path().join("system.wav");
        let to_i16 = |v: Vec<f32>| -> Vec<i16> {
            v.into_iter().map(|x| (x * 0.5 * 32_767.0) as i16).collect()
        };
        // Both tracks close together, so the whole structural offset shows
        // up as the file-length difference (the end-anchored estimator).
        let mic_len = 8 * sr as usize;
        let sys_len = mic_len + structural;
        let mic = to_i16(burst_at(mic_len, sr as usize, sr));
        let system = to_i16(burst_at(sys_len, sr as usize + structural + acoustic, sr));
        crate::wav_io::write_wav_mono_i16(&mic_p, &mic, sr).unwrap();
        crate::wav_io::write_wav_mono_i16(&sys_p, &system, sr).unwrap();

        let span = |stamps: bool| ChunkSpan {
            index: 1,
            start_ms: 0,
            mic_track: crate::model::Track::Mic,
            mic_wav: mic_p.clone(),
            system_wav: sys_p.clone(),
            mic_host_ns: stamps.then_some(1_000),
            system_host_ns: stamps.then_some(1_000),
            mic_samples: stamps.then_some(mic_len as u64),
            system_samples: stamps.then_some(sys_len as u64),
        };
        let mic_seg = LiveSeg {
            is_system: false,
            start_ms: 900,
            end_ms: 2_600,
            text: "fictional overlap line".into(),
        };
        let sys_seg = LiveSeg { is_system: true, ..mic_seg.clone() };

        let seeded = [span(true)];
        let oracle = WavOracle::new(&seeded);
        assert_eq!(
            oracle.direction(&mic_seg, &sys_seg),
            EchoDirection::SystemIsEcho,
            "stamp-seeded search must resolve the 4.7s total lag"
        );

        let unseeded = [span(false)];
        let oracle = WavOracle::new(&unseeded);
        assert_eq!(
            oracle.direction(&mic_seg, &sys_seg),
            EchoDirection::Unknown,
            "without stamps the offset is beyond MAX_LAG_S"
        );
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
