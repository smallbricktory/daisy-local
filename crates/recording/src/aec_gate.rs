// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Per-frame decision whether the echo canceller runs, from whether a
//! far-end reference exists for the frame.
//!
//! A frame's reference is *live* when reference data covers the frame and
//! carries a nonzero sample. The gate starts bypassed, engages on the first
//! live frame and bypasses again after [`BYPASS_AFTER_FRAMES`] consecutive
//! frames without one. While engaged every covered frame is cancelled; an
//! uncovered frame never is.

/// Consecutive frames without a live reference before the gate bypasses
/// (1 s of 10 ms frames).
pub const BYPASS_AFTER_FRAMES: u32 = 100;

#[derive(Debug, Default)]
pub struct RefGate {
    engaged: bool,
    absent_run: u32,
}

impl RefGate {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn engaged(&self) -> bool {
        self.engaged
    }

    /// Feeds one frame. `covered`: reference data exists for the frame's
    /// span. `far`: that data (ignored when not covered). Returns whether
    /// the canceller runs on this frame.
    pub fn step(&mut self, covered: bool, far: &[i16]) -> bool {
        if covered && far.iter().any(|&s| s != 0) {
            self.absent_run = 0;
            self.engaged = true;
        } else {
            self.absent_run = self.absent_run.saturating_add(1);
            if self.absent_run >= BYPASS_AFTER_FRAMES {
                self.engaged = false;
            }
        }
        covered && self.engaged
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIVE: [i16; 4] = [0, 5, -3, 0];
    const ZEROS: [i16; 4] = [0; 4];

    #[test]
    fn starts_bypassed_and_never_cancels_without_a_reference() {
        let mut g = RefGate::new();
        for _ in 0..1_000 {
            assert!(!g.step(false, &ZEROS));
            assert!(!g.step(true, &ZEROS));
        }
        assert!(!g.engaged());
    }

    #[test]
    fn a_live_frame_engages_at_once() {
        let mut g = RefGate::new();
        assert!(g.step(true, &LIVE));
        assert!(g.engaged());
    }

    /// One 10 ms frame of far end, then digital silence: the zero frames
    /// that carry its echo tail are cancelled for the full window.
    #[test]
    fn a_short_burst_keeps_its_echo_tail_cancelled() {
        let mut g = RefGate::new();
        assert!(g.step(true, &LIVE));
        for i in 1..BYPASS_AFTER_FRAMES {
            assert!(g.step(true, &ZEROS), "tail frame {i}");
        }
        assert!(!g.step(true, &ZEROS));
        assert!(!g.engaged());
    }

    #[test]
    fn an_uncovered_frame_is_never_cancelled_and_counts_toward_bypass() {
        let mut g = RefGate::new();
        g.step(true, &LIVE);
        for _ in 1..BYPASS_AFTER_FRAMES {
            assert!(!g.step(false, &LIVE));
            assert!(g.engaged());
        }
        assert!(!g.step(false, &LIVE));
        assert!(!g.engaged());
    }

    #[test]
    fn gaps_shorter_than_the_window_stay_engaged() {
        let mut g = RefGate::new();
        for _ in 0..50 {
            assert!(g.step(true, &LIVE));
            for _ in 0..BYPASS_AFTER_FRAMES - 1 {
                assert!(g.step(true, &ZEROS));
            }
        }
        assert!(g.engaged());
    }
}
