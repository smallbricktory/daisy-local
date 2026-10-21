// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Mix a recorded session's per-chunk, per-track WAVs into a single
//! Ogg-Opus file ("the meeting") for playback and archival. Chunks are
//! concatenated in index order and encoded once: stereo (L = mic, R = system)
//! for a session with a system track, mono (mic) for one without.

use crate::compress::{encode_mono_pcm16, encode_stereo_pcm16, CompressParams};
use crate::error::{RecordingError, Result};
use crate::manifest::SessionManifest;
use std::path::Path;

/// File name (relative to the session directory) of the mixed-down meeting
/// audio. Ogg-Opus container.
pub const MEETING_AUDIO_NAME: &str = "meeting.opus";

/// One chunk track as mono i16 with its rate.
fn read_track(path: &Path) -> Option<(Vec<i16>, u32)> {
    transcript::wav_io::read_wav_mono_i16(path).ok()
}

/// Target track loudness for mixdown level-matching (-20 dBFS RMS); quiet
/// tracks are boosted toward it and hot tracks pulled down to it (subject to
/// the peak guard below).
const TARGET_RMS_DBFS: f32 = -20.0;
/// Hard peak ceiling (≈ -0.4 dBFS) the makeup gain may not push a track
/// past. Mirrors the per-block ceiling used in the live system-gain
/// normalizer.
const PEAK_CEILING: f32 = 0.95;
/// Full-scale magnitude of a 16-bit sample (used to normalize to [0, 1]).
const I16_MAX_F: f32 = i16::MAX as f32;

/// Per-track makeup gain that moves a track toward [`TARGET_RMS_DBFS`] without
/// pushing its peak past [`PEAK_CEILING`]. Returns 1.0 for empty or
/// near-silent tracks. The gain may be < 1.0 — a track already hotter than
/// target is attenuated — and is always clamped down to whatever the peak
/// headroom allows; the result never clips.
fn level_match_gain(samples: &[i16]) -> f32 {
    if samples.is_empty() {
        return 1.0;
    }
    let mut sumsq = 0.0f64;
    let mut peak = 0i32;
    for &s in samples {
        let v = f64::from(s);
        sumsq += v * v;
        peak = peak.max(i32::from(s).abs());
    }
    let rms = ((sumsq / samples.len() as f64).sqrt() as f32) / I16_MAX_F;
    if rms <= 1e-4 {
        return 1.0; // silence / DC — nothing to match, leave untouched
    }
    let target = 10f32.powf(TARGET_RMS_DBFS / 20.0); // ~0.1 in linear scale
    let mut gain = target / rms;
    let peak_norm = peak as f32 / I16_MAX_F;
    if peak_norm > 0.0 {
        gain = gain.min(PEAK_CEILING / peak_norm); // never clip
    }
    gain
}

/// Apply a level-match gain to one sample, rounding and clamping to i16.
fn apply_gain(s: i16, gain: f32) -> i16 {
    if gain == 1.0 {
        return s;
    }
    (f32::from(s) * gain)
        .round()
        .clamp(i16::MIN as f32, i16::MAX as f32) as i16
}

/// Build `<session_dir>/meeting.opus` from the session's chunk WAVs. Returns
/// the number of bytes written. Chunks whose WAVs are missing are skipped
/// (e.g. after a "clear recordings" run); if no audio is found at all, returns
/// `RecordingError::SessionMissing`. A session with a system track is written
/// as stereo (L = mic, R = system); a session without one
/// (`!has_system_track()`) is written as mono from the mic track.
pub fn build_meeting_audio(
    session_dir: &Path,
    manifest: &SessionManifest,
    params: &CompressParams,
) -> Result<u64> {
    let mut chunks = manifest.chunks.clone();
    chunks.sort_by_key(|c| c.index);

    // Each track accumulates in full (mic and system kept separate); the two
    // sides are level-matched against their whole-recording loudness before
    // interleaving. Per chunk, both tracks are padded to the chunk's max
    // length, keeping the L/R streams sample-aligned in time.
    let mut mic_all: Vec<i16> = Vec::new();
    let mut sys_all: Vec<i16> = Vec::new();
    let mut sample_rate = manifest.sample_rate.max(8000);
    let mut any = false;
    let stereo = manifest.has_system_track();

    for c in &chunks {
        // Mic-track preference: denoised (mic_dn) > echo-cancelled (mic_aec)
        // > raw mic. A referenced sidecar missing on disk falls back to the
        // next choice. Chunk WAV paths in the manifest are relative to the
        // session root (e.g. "chunks/0001/mic.wav").
        let mic_path = match c.mic_dn_wav_relative.as_ref() {
            Some(p) if syncsafe::join_rel(session_dir, p).is_file() => syncsafe::join_rel(session_dir, p),
            _ => match c.mic_aec_wav_relative.as_ref() {
                Some(p) if syncsafe::join_rel(session_dir, p).is_file() => syncsafe::join_rel(session_dir, p),
                _ => syncsafe::join_rel(session_dir, &c.mic_wav_relative),
            },
        };
        let sys_path = syncsafe::join_rel(session_dir, &c.system_wav_relative);

        let mic = read_track(&mic_path);
        let sys = if stereo { read_track(&sys_path) } else { None };
        if mic.is_none() && sys.is_none() {
            continue; // chunk audio gone: skipped
        }
        any = true;

        let (m, mr) = mic.unzip();
        let (s, sr) = sys.unzip();
        if let Some(r) = mr.or(sr) {
            sample_rate = r;
        }
        let m = m.unwrap_or_default();
        let s = s.unwrap_or_default();
        let len = m.len().max(s.len());
        for i in 0..len {
            mic_all.push(m.get(i).copied().unwrap_or(0));
            sys_all.push(s.get(i).copied().unwrap_or(0));
        }
    }

    if !any || (mic_all.is_empty() && sys_all.is_empty()) {
        return Err(RecordingError::SessionMissing(session_dir.to_path_buf()));
    }

    // Level-match the two tracks to a common loudness. Each track gets one
    // makeup gain derived from its whole-recording RMS, clamped by a peak
    // ceiling; the gain never clips the i16 range. Gain may go below 1.0: a
    // too-hot track is attenuated.
    let g_mic = level_match_gain(&mic_all);
    let g_sys = level_match_gain(&sys_all);

    let out = session_dir.join(MEETING_AUDIO_NAME);
    // Builder-unique tmp file.
    static TMP_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let tmp = out.with_extension(format!(
        "opus.{}.{}.tmp",
        std::process::id(),
        TMP_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));

    // Interleave into stereo: L = mic, R = system. The tracks stay separable
    // via decode_opus + deinterleave_stereo. Without a system track the file
    // is the mic track alone, one channel.
    let encoded = if stereo {
        encode_stereo_pcm16(&stereo_interleave(&mic_all, &sys_all, g_mic, g_sys), sample_rate, params, &tmp)
    } else {
        let mono: Vec<i16> = mic_all.iter().map(|&m| apply_gain(m, g_mic)).collect();
        encode_mono_pcm16(&mono, sample_rate, params, &tmp)
    };
    let n = encoded.inspect_err(|_| {
        let _ = syncsafe::remove_file(&tmp);
    })?;
    syncsafe::rename(&tmp, &out).map_err(|e| {
        let _ = syncsafe::remove_file(&tmp);
        RecordingError::Io {
            path: out.clone(),
            source: e,
        }
    })?;
    Ok(n)
}

/// Interleave `mic` (L) and `sys` (R), level-matched by `g_mic` / `g_sys`;
/// the shorter track is zero-padded.
fn stereo_interleave(mic_all: &[i16], sys_all: &[i16], g_mic: f32, g_sys: f32) -> Vec<i16> {
    let frames = mic_all.len().max(sys_all.len());
    let mut mixed: Vec<i16> = Vec::with_capacity(frames * 2);
    for i in 0..frames {
        let m = mic_all.get(i).copied().unwrap_or(0);
        let s = sys_all.get(i).copied().unwrap_or(0);
        mixed.push(apply_gain(m, g_mic)); // L = mic
        mixed.push(apply_gain(s, g_sys)); // R = system
    }
    mixed
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Linear RMS of a track, normalized to [0, 1] (full-scale = 1.0).
    fn rms_norm(s: &[i16]) -> f32 {
        if s.is_empty() {
            return 0.0;
        }
        let sumsq: f64 = s.iter().map(|&v| f64::from(v) * f64::from(v)).sum();
        ((sumsq / s.len() as f64).sqrt() as f32) / I16_MAX_F
    }

    fn peak_norm(s: &[i16]) -> f32 {
        s.iter().map(|&v| i32::from(v).abs()).max().unwrap_or(0) as f32 / I16_MAX_F
    }

    #[test]
    fn silent_track_keeps_unity_gain() {
        assert_eq!(level_match_gain(&[]), 1.0);
        assert_eq!(level_match_gain(&[0i16; 1000]), 1.0);
    }

    #[test]
    fn quiet_track_is_boosted_toward_target() {
        // ~-40 dBFS tone: well below the -20 dBFS target → gain should boost.
        let quiet: Vec<i16> = (0..16_000)
            .map(|i| ((i as f32 * 0.2).sin() * 327.0) as i16)
            .collect();
        let g = level_match_gain(&quiet);
        assert!(g > 1.0, "expected boost, got {g}");
        let after: Vec<i16> = quiet.iter().map(|&s| apply_gain(s, g)).collect();
        let target = 10f32.powf(TARGET_RMS_DBFS / 20.0);
        // Within ~1.5 dB of target (peak guard didn't bind here).
        assert!(
            (rms_norm(&after) - target).abs() < 0.02,
            "after RMS {} vs target {target}",
            rms_norm(&after)
        );
    }

    #[test]
    fn hot_track_is_attenuated() {
        // ~-6 dBFS tone: hotter than target → gain should attenuate (< 1.0).
        let hot: Vec<i16> = (0..16_000)
            .map(|i| ((i as f32 * 0.2).sin() * 16_000.0) as i16)
            .collect();
        let g = level_match_gain(&hot);
        assert!(g < 1.0, "expected attenuation, got {g}");
    }

    #[test]
    fn gain_never_clips_a_near_full_scale_track() {
        // Peak already near full scale: the peak guard caps gain and the
        // result stays under the ceiling even though RMS is below target.
        let mut spiky: Vec<i16> = vec![100i16; 16_000];
        spiky[0] = 32_000; // lone hot peak, low overall RMS → big naive makeup
        let g = level_match_gain(&spiky);
        let after: Vec<i16> = spiky.iter().map(|&s| apply_gain(s, g)).collect();
        assert!(
            peak_norm(&after) <= PEAK_CEILING + 1e-3,
            "peak {} exceeded ceiling",
            peak_norm(&after)
        );
    }

    #[test]
    fn level_match_brings_two_tracks_together() {
        // A quiet mic and a hot system should end up within ~3 dB of each other.
        let mic: Vec<i16> = (0..16_000)
            .map(|i| ((i as f32 * 0.2).sin() * 800.0) as i16)
            .collect();
        let sys: Vec<i16> = (0..16_000)
            .map(|i| ((i as f32 * 0.2).sin() * 12_000.0) as i16)
            .collect();
        let before = rms_norm(&sys) / rms_norm(&mic);
        assert!(before > 5.0, "fixture should be lopsided, got {before}x");
        let gm = level_match_gain(&mic);
        let gs = level_match_gain(&sys);
        let mic_a: Vec<i16> = mic.iter().map(|&s| apply_gain(s, gm)).collect();
        let sys_a: Vec<i16> = sys.iter().map(|&s| apply_gain(s, gs)).collect();
        let ratio = rms_norm(&sys_a) / rms_norm(&mic_a);
        assert!(
            (0.71..1.41).contains(&ratio),
            "tracks not matched: {ratio}x apart"
        );
    }
}
