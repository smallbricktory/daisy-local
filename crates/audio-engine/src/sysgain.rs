//! Upward normalization for the SYSTEM (loopback / tap) track.
//!
//! Applies a bounded, smoothed makeup gain toward a target level.
//!
//! Properties:
//!   * Up only. Never attenuates. Normal-level system audio (rms ≥ target)
//!     gets gain 1.0 — untouched.
//!   * Gated on a noise floor: a block below the floor is treated as silence
//!     and the gain holds.
//!   * Bounded to MAX_GAIN (+24 dB) and smoothed across blocks.
//!   * Hard-clamped to i16 range on apply.

/// Target RMS (normalized 0..1, ≈ -24 dBFS) — typical speech level.
const TARGET_RMS: f32 = 0.06;
/// Below this RMS (≈ -56 dBFS) a block is silence: the gain holds.
const NOISE_FLOOR: f32 = 0.0015;
/// Makeup-gain ceiling (≈ +24 dB).
const MAX_GAIN: f32 = 16.0;
/// Smoothing per block toward the desired gain (attack faster than release).
const ATTACK: f32 = 0.20;
const RELEASE: f32 = 0.05;
/// Per-block peak headroom (≈ -0.4 dBFS). The applied gain is hard-capped;
/// the block's loudest sample never reaches the i16 clamp.
const PEAK_CEILING: f32 = 0.95;

/// Full-trace flag (`DAISY_LIVE_TRACE`, set at app startup when debug level =
/// Full). Read once.
fn live_trace() -> bool {
    use std::sync::OnceLock;
    static T: OnceLock<bool> = OnceLock::new();
    *T.get_or_init(|| std::env::var("DAISY_LIVE_TRACE").is_ok())
}

pub struct SystemNormalizer {
    gain: f32,
}

impl Default for SystemNormalizer {
    fn default() -> Self {
        Self::new()
    }
}

impl SystemNormalizer {
    pub fn new() -> Self {
        Self { gain: 1.0 }
    }

    pub fn current_gain(&self) -> f32 {
        self.gain
    }

    /// Update the smoothed gain from this block's level and apply it in place.
    pub fn process(&mut self, frames: &mut [i16]) {
        if frames.is_empty() {
            return;
        }
        let rms = rms_norm(frames);
        let desired = if rms < NOISE_FLOOR {
            // Silence: any held gain decays back toward 1.
            1.0
        } else {
            (TARGET_RMS / rms).clamp(1.0, MAX_GAIN)
        };
        let a = if desired > self.gain { ATTACK } else { RELEASE };
        self.gain += (desired - self.gain) * a;
        self.gain = self.gain.max(1.0);
        // Hard peak guard: the smoothed makeup gain is applied, but never
        // above what this block's peak allows without clipping. Per-block +
        // hard (not smoothed).
        let peak = peak_norm(frames);
        let peak_cap = if peak > 1e-6 { (PEAK_CEILING / peak).max(1.0) } else { f32::INFINITY };
        let g = self.gain.min(peak_cap);
        if live_trace() && peak_cap < self.gain - 0.01 {
            log::debug!(
                target: "sysgain",
                "peak-guard: makeup {:.2}x capped to {:.2}x (block peak {:.3} fs) — clip prevented",
                self.gain, g, peak,
            );
        }
        if (g - 1.0).abs() < 0.01 {
            return; // ~unity (or block already at/above the ceiling) — untouched
        }
        for s in frames.iter_mut() {
            *s = ((*s as f32) * g).clamp(i16::MIN as f32, i16::MAX as f32) as i16;
        }
    }
}

fn rms_norm(frames: &[i16]) -> f32 {
    if frames.is_empty() {
        return 0.0;
    }
    let sum: f64 = frames.iter().map(|&s| (s as f64) * (s as f64)).sum();
    ((sum / frames.len() as f64).sqrt() / 32768.0) as f32
}

/// Peak amplitude of the block, normalized 0..1.
fn peak_norm(frames: &[i16]) -> f32 {
    frames.iter().fold(0i32, |m, &s| m.max((s as i32).abs())) as f32 / 32768.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(level: i16, n: usize) -> Vec<i16> {
        // Alternating ±: RMS ≈ |level|.
        (0..n).map(|i| if i % 2 == 0 { level } else { -level }).collect()
    }

    #[test]
    fn boosts_quiet_far_end_over_time() {
        let mut ag = SystemNormalizer::new();
        // A "quiet speech" block at ~-50 dBFS (level ~100 ≈ 0.003 > floor)
        // gets boosted.
        let mut acc_gain = 1.0;
        for _ in 0..50 {
            let mut b = block(100, 1600);
            ag.process(&mut b);
            acc_gain = ag.current_gain();
        }
        assert!(acc_gain > 4.0, "quiet speech should be boosted, got {acc_gain}");
        assert!(acc_gain <= MAX_GAIN);
    }

    #[test]
    fn leaves_normal_level_untouched() {
        let mut ag = SystemNormalizer::new();
        // rms ≈ 0.06 (target) → gain stays ~1.0, samples unchanged.
        for _ in 0..20 {
            let mut b = block(1966, 1600); // 1966/32768 ≈ 0.06
            ag.process(&mut b);
        }
        assert!((ag.current_gain() - 1.0).abs() < 0.05, "got {}", ag.current_gain());
    }

    #[test]
    fn never_attenuates_loud_audio() {
        let mut ag = SystemNormalizer::new();
        let mut b = block(16000, 1600); // very loud, rms ≈ 0.49
        let before = b.clone();
        ag.process(&mut b);
        assert!(ag.current_gain() >= 1.0);
        assert_eq!(b, before, "loud audio must not be attenuated");
    }

    #[test]
    fn loud_transient_after_quiet_does_not_clip() {
        // Makeup gain ramps up on quiet far-end speech; a sudden loud block
        // must not clip while the smoothed gain is still high.
        let mut ag = SystemNormalizer::new();
        for _ in 0..40 {
            let mut q = block(160, 1600); // rms ≈ 0.005 → desired ~12×
            ag.process(&mut q);
        }
        assert!(ag.current_gain() > 3.0, "gain should ramp up, got {}", ag.current_gain());
        let mut loud = block(15000, 1600); // peak ≈ 0.46 fs
        ag.process(&mut loud);
        assert!(
            loud.iter().all(|&s| s < 32767 && s > -32768),
            "loud transient must not clip under the held makeup gain"
        );
    }

    #[test]
    fn does_not_amplify_silence() {
        let mut ag = SystemNormalizer::new();
        // Pure silence stays silent; gain never climbs.
        for _ in 0..50 {
            let mut b = vec![0i16; 1600];
            ag.process(&mut b);
            assert!(b.iter().all(|&s| s == 0));
        }
        assert!((ag.current_gain() - 1.0).abs() < 0.05);
        // Near-floor hiss (below NOISE_FLOOR) also isn't chased.
        for _ in 0..50 {
            let mut b = block(20, 1600); // ~0.0006 < floor
            ag.process(&mut b);
        }
        assert!((ag.current_gain() - 1.0).abs() < 0.05, "got {}", ag.current_gain());
    }
}
