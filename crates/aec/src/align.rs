// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Acoustic cross-correlation alignment for the live AEC reference.
//!
//! GCC-PHAT between a recent raw-mic tail and a recent reference tail
//! yields the lag at which the far-end audio appears in the mic — device
//! latencies and acoustic path included, which is exactly the alignment
//! the canceller needs. Probe-scoped: short inputs, small lag window.
//! (The finalize-side `transcript::echo_direction` runs its own wider
//! variant over whole segments.)

/// A probe input below this RMS carries no usable signal. Deliberately low
/// on purpose: a speaker echo picked up across a room peaks well under
/// 0.05, and the caller's confidence gate rejects weak correlations anyway.
const MIN_RMS: f32 = 0.002;

/// Lag (in samples) of `reference` content relative to its appearance in
/// `mic`, at the strongest PHAT peak within `±max_lag`, with the
/// peak-to-mean confidence ratio. Positive lag means the mic echo TRAILS
/// the reference by that many samples. Returns None when either input is
/// too short, too quiet, or the spectrum degenerates.
pub fn acoustic_lag(mic: &[f32], reference: &[f32], max_lag: usize) -> Option<(i64, f32)> {
    // Probe-scoped pre-gate: both windows must cover the lag range.
    if mic.len() < max_lag || reference.len() < max_lag {
        return None;
    }
    xcorr::gcc_phat_core(mic, reference, max_lag, Some(MIN_RMS))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noise(n: usize, seed: u64) -> Vec<f32> {
        // Deterministic pseudo-noise (xorshift) — broadband, correlation-friendly.
        let mut x = seed | 1;
        (0..n)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                ((x as i32) as f32 / i32::MAX as f32) * 0.3
            })
            .collect()
    }

    #[test]
    fn recovers_a_known_delay_with_sign() {
        let src = noise(16_000, 7);
        // Mic hears the reference 800 samples (50 ms) later, attenuated.
        let mut mic = vec![0.0f32; 800];
        mic.extend(src.iter().map(|&x| x * 0.2));
        mic.truncate(16_000);
        let (lag, ratio) = acoustic_lag(&mic, &src, 4_000).expect("confident lag");
        assert_eq!(lag, 800, "mic echo trails the reference by +800");
        assert!(ratio > 8.0, "clean synthetic delay must be decisive, got {ratio}");
    }

    #[test]
    fn recovers_a_negative_lag() {
        // Reference stream stamped late: mic echo appears EARLIER than the
        // compared reference window (echo leads by 500).
        let src = noise(16_000, 21);
        let mic: Vec<f32> = src[500..].iter().map(|&x| x * 0.2).collect();
        let (lag, _) = acoustic_lag(&mic, &src, 4_000).expect("confident lag");
        assert_eq!(lag, -500);
    }

    #[test]
    fn silence_and_unrelated_noise_are_rejected_or_unconfident() {
        assert!(acoustic_lag(&vec![0.0; 16_000], &noise(16_000, 3), 4_000).is_none());
        assert!(acoustic_lag(&noise(16_000, 3), &vec![0.0; 16_000], 4_000).is_none());
        // Unrelated signals: whatever peak exists must be weak.
        if let Some((_, ratio)) = acoustic_lag(&noise(16_000, 3), &noise(16_000, 999), 4_000) {
            assert!(ratio < 8.0, "unrelated noise must not look confident, got {ratio}");
        }
    }

    #[test]
    fn too_short_inputs_are_rejected() {
        assert!(acoustic_lag(&noise(1_000, 3), &noise(16_000, 5), 4_000).is_none());
    }
}
