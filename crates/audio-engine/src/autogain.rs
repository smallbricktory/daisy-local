//! Mic anti-clipping auto-gain — the shared decision logic.
//!
//! Watches a mic stream for sustained clipping and decides when to step the
//! OS input gain down. Pure + platform-agnostic: the caller supplies mic
//! frames + a monotonic clock, applies the returned step via the per-OS
//! `set_input_gain`, and restores the original gain on stop. Down-only,
//! feed-forward.

/// |sample| at/above this counts as clipped (~0.99 of i16 full-scale).
const CLIP_LEVEL: i16 = 32_440;
/// Window clip-rate above this triggers a step down.
const CLIP_TRIGGER: f32 = 0.01; // 1%
/// Evaluation window length.
const WINDOW_MS: u64 = 1_500;
/// Minimum gap between steps (let the new gain take effect before stepping again).
const COOLDOWN_MS: u64 = 3_000;
/// Multiplicative step per trigger.
const STEP_FACTOR: f32 = 0.8;
/// Never lower the input below this (keep the mic alive).
const GAIN_FLOOR: f32 = 0.2;

/// Emitted when the loop decides to lower the gain.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GainStep {
    pub new_gain: f32,
}

pub struct AutoGain {
    current: f32,
    clipped: u64,
    total: u64,
    window_start_ms: Option<u64>,
    last_step_ms: Option<u64>,
}

impl AutoGain {
    /// `start_gain` is the device's input gain at capture start (0..1).
    pub fn new(start_gain: f32) -> Self {
        Self {
            current: start_gain.clamp(0.0, 1.0),
            clipped: 0,
            total: 0,
            window_start_ms: None,
            last_step_ms: None,
        }
    }

    pub fn current_gain(&self) -> f32 {
        self.current
    }

    /// Feed one mic frame batch + a monotonic timestamp (ms). Returns `Some` when
    /// it decides to lower the gain this tick (caller applies it), else `None`.
    pub fn observe(&mut self, frames: &[i16], now_ms: u64) -> Option<GainStep> {
        if frames.is_empty() {
            return None;
        }
        self.clipped += frames.iter().filter(|&&s| s.unsigned_abs() >= CLIP_LEVEL as u16).count() as u64;
        self.total += frames.len() as u64;
        let start = *self.window_start_ms.get_or_insert(now_ms);
        if now_ms.saturating_sub(start) < WINDOW_MS || self.total == 0 {
            return None;
        }

        let rate = self.clipped as f32 / self.total as f32;
        self.clipped = 0;
        self.total = 0;
        self.window_start_ms = Some(now_ms);

        let cooled = self.last_step_ms.map_or(true, |t| now_ms.saturating_sub(t) >= COOLDOWN_MS);
        if rate > CLIP_TRIGGER && cooled && self.current > GAIN_FLOOR {
            let new = (self.current * STEP_FACTOR).max(GAIN_FLOOR);
            if new < self.current {
                self.current = new;
                self.last_step_ms = Some(now_ms);
                return Some(GainStep { new_gain: new });
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clip_frames(n: usize) -> Vec<i16> { vec![i16::MAX; n] }
    fn quiet_frames(n: usize) -> Vec<i16> { vec![100; n] }

    #[test]
    fn sustained_clipping_steps_down_with_cooldown() {
        let mut ag = AutoGain::new(1.0);
        // t=0: first batch starts the window, no decision yet.
        assert_eq!(ag.observe(&clip_frames(1600), 0), None);
        // t=1600 (> WINDOW): window of all-clipping → step down to 0.8.
        let s = ag.observe(&clip_frames(1600), 1600).expect("step");
        assert!((s.new_gain - 0.8).abs() < 1e-6);
        // Window ends ~3200 but cooldown (3s from the 1600 step) not met → no step
        // (and the window resets to 3200).
        assert_eq!(ag.observe(&clip_frames(1600), 3200), None);
        // Next window ends ~4800, now past cooldown → steps again to 0.64.
        let s2 = ag.observe(&clip_frames(1600), 4800).expect("step2");
        assert!((s2.new_gain - 0.64).abs() < 1e-6);
    }

    #[test]
    fn quiet_audio_never_steps() {
        let mut ag = AutoGain::new(1.0);
        for t in (0..10_000).step_by(800) {
            assert_eq!(ag.observe(&quiet_frames(800), t), None);
        }
        assert_eq!(ag.current_gain(), 1.0);
    }

    #[test]
    fn single_transient_below_window_no_step() {
        let mut ag = AutoGain::new(1.0);
        // A burst of clipping then quiet within one window → rate < 1%.
        let mut frames = clip_frames(10);
        frames.extend(quiet_frames(2000));
        assert_eq!(ag.observe(&frames, 0), None);
        assert_eq!(ag.observe(&quiet_frames(100), 1600), None);
        assert_eq!(ag.current_gain(), 1.0);
    }

    #[test]
    fn stops_at_floor() {
        let mut ag = AutoGain::new(0.25); // already near the 0.2 floor
        assert_eq!(ag.observe(&clip_frames(1600), 0), None);
        let s = ag.observe(&clip_frames(1600), 1600).expect("step to floor");
        assert!((s.new_gain - 0.2).abs() < 1e-6); // 0.25*0.8=0.2
        // At floor, no further steps even with clipping.
        assert_eq!(ag.observe(&clip_frames(1600), 5000), None);
        assert_eq!(ag.current_gain(), 0.2);
    }
}
