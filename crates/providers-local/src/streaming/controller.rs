//! Cross-track catch-up controller for the serialized live-whisper decoder.
//!
//! Both track loops (mic + system) share one decoder (single-permit
//! semaphore); the stability measure is global serialized utilization, not
//! per-track:
//!
//! ```text
//! rho = Σ_active_tracks ( D_i / hop )     # D_i = EWMA decode service time
//! ```
//!
//! Each track reports its decode cost into a registered slot; the controller
//! sums the cost of *active* (recently-decoding) slots and steps a shared hop
//! to keep `rho` under target — shedding fast (and immediately on a deadline
//! miss), restoring slowly. When even the max hop cannot hold `rho` < 1 it
//! flags `cannot_keep_up` (the hard floor). The hop ladder is configurable
//! (settings.json override).

use std::sync::Mutex;

/// Default hop ladder (ms). Overridable via the `live_hop_ladder_ms` setting.
pub const DEFAULT_HOP_LADDER_MS: [i64; 6] = [1000, 1500, 2000, 3000, 4000, 5000];

// Tunables.
const EWMA_ALPHA: f64 = 0.3; // decode-cost smoothing
const SHED_RHO: f64 = 0.9; // raise hop when rho exceeds this
const RESTORE_RHO: f64 = 0.5; // lower hop only when rho is below this
const MIN_DWELL_MS: u64 = 4000; // min wall-time between any two (non-miss) hop changes
const RESTORE_DWELL_MS: u64 = 30_000; // sustained-health hold before a restore
const ACTIVE_TIMEOUT_MS: u64 = 5000; // a slot idle past max(this, 3·hop) drops out
const MAX_SHED_STEP: usize = 2; // cap on rungs jumped per shed
const FLOOR_RHO: f64 = 1.0; // at max hop with rho ≥ this, sustained = cannot keep up

/// Validate a custom ladder: non-empty, strictly ascending, all positive. Falls
/// back to the default if the override is malformed.
fn sanitize_ladder(ladder: Vec<i64>) -> Vec<i64> {
    let ok = !ladder.is_empty()
        && ladder.iter().all(|&v| v > 0)
        && ladder.windows(2).all(|w| w[0] < w[1]);
    if ok {
        ladder
    } else {
        DEFAULT_HOP_LADDER_MS.to_vec()
    }
}

#[derive(Clone, Copy)]
struct Slot {
    d_ewma_ms: f64,
    last_ms: u64,
    seen: bool,
}

struct Inner {
    ladder: Vec<i64>,
    hop_idx: usize,
    slots: Vec<Slot>,
    last_change_ms: u64,
    healthy_since_ms: Option<u64>,
    floor_since_ms: Option<u64>,
}

/// Shared, thread-safe catch-up controller. Both track loops hold one
/// `Arc<CatchupController>`; each registers a slot and reports decode times.
pub struct CatchupController {
    inner: Mutex<Inner>,
}

impl Default for CatchupController {
    fn default() -> Self {
        Self::new()
    }
}

impl CatchupController {
    pub fn new() -> Self {
        Self::with_ladder(DEFAULT_HOP_LADDER_MS.to_vec())
    }

    /// Build with a custom hop ladder (sanitized; bad input → default).
    pub fn with_ladder(ladder: Vec<i64>) -> Self {
        Self {
            inner: Mutex::new(Inner {
                ladder: sanitize_ladder(ladder),
                hop_idx: 0,
                slots: Vec::new(),
                last_change_ms: 0,
                healthy_since_ms: None,
                floor_since_ms: None,
            }),
        }
    }

    /// Register a track loop; returns its slot index. Call once per `run()`.
    pub fn register(&self) -> usize {
        let mut g = self.inner.lock().unwrap();
        g.slots.push(Slot { d_ewma_ms: 0.0, last_ms: 0, seen: false });
        g.slots.len() - 1
    }

    /// Current shared hop (ms). Read each turn before deciding to decode.
    pub fn hop_ms(&self) -> i64 {
        let g = self.inner.lock().unwrap();
        g.ladder[g.hop_idx]
    }

    /// Global serialized utilization at `now_ms`.
    pub fn rho(&self, now_ms: u64) -> f64 {
        self.inner.lock().unwrap().rho(now_ms)
    }

    /// True when the decoder can't keep up even at the max hop (hard floor).
    pub fn cannot_keep_up(&self, now_ms: u64) -> bool {
        self.inner.lock().unwrap().at_floor(now_ms)
    }

    /// Report a completed decode's service time for `slot` at monotonic
    /// `now_ms`; runs one control step and returns the (possibly updated) hop.
    /// `decode_ms` is the actual decode work, EXCLUDING the semaphore wait.
    pub fn report_decode(&self, slot: usize, decode_ms: u64, now_ms: u64) -> i64 {
        let mut g = self.inner.lock().unwrap();
        // Deadline miss: the decode exceeded its hop budget; sheds
        // immediately, bypassing the dwell.
        let miss = decode_ms as i64 >= g.ladder[g.hop_idx];
        if let Some(s) = g.slots.get_mut(slot) {
            s.d_ewma_ms = if s.seen {
                EWMA_ALPHA * decode_ms as f64 + (1.0 - EWMA_ALPHA) * s.d_ewma_ms
            } else {
                decode_ms as f64
            };
            s.last_ms = now_ms;
            s.seen = true;
        }
        g.step(now_ms, miss);
        g.ladder[g.hop_idx]
    }
}

impl Inner {
    fn active_window_ms(&self) -> u64 {
        ACTIVE_TIMEOUT_MS.max((self.ladder[self.hop_idx] as u64).saturating_mul(3))
    }

    fn sum_active_d(&self, now_ms: u64) -> f64 {
        let win = self.active_window_ms();
        self.slots
            .iter()
            .filter(|s| s.seen && now_ms.saturating_sub(s.last_ms) <= win)
            .map(|s| s.d_ewma_ms)
            .sum()
    }

    fn rho(&self, now_ms: u64) -> f64 {
        self.sum_active_d(now_ms) / self.ladder[self.hop_idx] as f64
    }

    /// At the max hop and rho has stayed ≥ FLOOR_RHO for at least one dwell.
    fn at_floor(&self, now_ms: u64) -> bool {
        self.floor_since_ms
            .map(|t| now_ms.saturating_sub(t) >= MIN_DWELL_MS)
            .unwrap_or(false)
    }

    fn step(&mut self, now_ms: u64, deadline_miss: bool) {
        let max_idx = self.ladder.len() - 1;
        let rho = self.rho(now_ms);

        // Hard-floor tracking: at the top rung with rho still over the floor.
        if self.hop_idx == max_idx && rho >= FLOOR_RHO {
            self.healthy_since_ms = None;
            self.floor_since_ms.get_or_insert(now_ms);
        } else {
            self.floor_since_ms = None;
            if rho < RESTORE_RHO {
                self.healthy_since_ms.get_or_insert(now_ms);
            } else {
                self.healthy_since_ms = None;
            }
        }

        // A minimum dwell separates hop changes, except a deadline miss,
        // which sheds immediately. The dwell still gates restores and
        // non-miss sheds.
        if !deadline_miss && now_ms.saturating_sub(self.last_change_ms) < MIN_DWELL_MS {
            return;
        }

        if (rho > SHED_RHO || deadline_miss) && self.hop_idx < max_idx {
            // Step up to the rung that brings projected rho under target, at
            // most MAX_SHED_STEP rungs at once.
            let sum_d = self.sum_active_d(now_ms);
            let ceil = (self.hop_idx + MAX_SHED_STEP).min(max_idx);
            let mut idx = self.hop_idx + 1;
            while idx < ceil && sum_d / self.ladder[idx] as f64 > SHED_RHO {
                idx += 1;
            }
            self.hop_idx = idx;
            self.last_change_ms = now_ms;
            self.healthy_since_ms = None;
        } else if rho < RESTORE_RHO && self.hop_idx > 0 {
            // Restore one rung, only after a sustained healthy stretch.
            let healthy_for =
                self.healthy_since_ms.map(|t| now_ms.saturating_sub(t)).unwrap_or(0);
            if healthy_for >= RESTORE_DWELL_MS {
                self.hop_idx -= 1;
                self.last_change_ms = now_ms;
                self.healthy_since_ms = Some(now_ms);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_healthy_track_holds_min_hop() {
        let c = CatchupController::new();
        let s = c.register();
        let mut t = 0u64;
        for _ in 0..20 {
            c.report_decode(s, 300, t); // 0.3s decode, 1s hop → rho 0.3
            t += 1000;
        }
        assert_eq!(c.hop_ms(), 1000, "healthy single track stays at min hop");
        assert!(c.rho(t) < 0.5);
    }

    #[test]
    fn two_track_overload_sheds_until_stable() {
        // 2 tracks, D≈0.84s, 1s hop → rho 1.68.
        let c = CatchupController::new();
        let a = c.register();
        let b = c.register();
        let mut t = 0u64;
        for _ in 0..40 {
            let hop = c.hop_ms() as u64;
            c.report_decode(a, 840, t);
            c.report_decode(b, 840, t + 1);
            t += hop;
        }
        assert!(c.hop_ms() > 1000, "raised the hop off 1s (got {})", c.hop_ms());
        assert!(c.rho(t) <= SHED_RHO + 1e-6, "rho under shed threshold (got {})", c.rho(t));
    }

    #[test]
    fn shed_steps_at_most_two_rungs_per_move() {
        let c = CatchupController::new();
        let a = c.register();
        c.report_decode(a, 9000, 0); // massive single spike at 1s hop
        assert!(
            c.hop_ms() <= DEFAULT_HOP_LADDER_MS[MAX_SHED_STEP],
            "capped to ≤2 rungs in one move (got {})",
            c.hop_ms()
        );
    }

    #[test]
    fn silent_track_drops_out_of_rho() {
        let c = CatchupController::new();
        let active = c.register();
        let _silent = c.register();
        let mut t = 0u64;
        for _ in 0..10 {
            c.report_decode(active, 600, t);
            t += 1000;
        }
        assert!((c.rho(t) - 0.6).abs() < 0.15, "rho ≈ single active D/hop (got {})", c.rho(t));
        assert_eq!(c.hop_ms(), 1000);
    }

    #[test]
    fn non_miss_change_respects_min_dwell() {
        let c = CatchupController::new();
        let a = c.register();
        let b = c.register();
        // Non-miss overload (each decode < hop) past the startup dwell → sheds once.
        c.report_decode(a, 700, 5000);
        c.report_decode(b, 700, 5000);
        let after = c.hop_ms();
        assert!(after > 1000, "sheds once past dwell");
        for dt in [5500u64, 6000, 8000, 5000 + MIN_DWELL_MS - 1] {
            c.report_decode(a, 700, dt);
            c.report_decode(b, 700, dt);
        }
        assert_eq!(c.hop_ms(), after, "no further change within dwell (non-miss)");
    }

    #[test]
    fn deadline_miss_sheds_immediately_bypassing_dwell() {
        let c = CatchupController::new();
        let a = c.register();
        c.report_decode(a, 1500, 0); // 1500 ≥ 1000 hop = miss
        assert!(c.hop_ms() > 1000, "miss shed immediately despite dwell (got {})", c.hop_ms());
    }

    #[test]
    fn restore_is_slow_and_floor_is_flagged() {
        let c = CatchupController::new();
        let a = c.register();
        let b = c.register();
        let mut t = 0u64;
        for _ in 0..30 {
            let hop = c.hop_ms() as u64;
            c.report_decode(a, 4000, t);
            c.report_decode(b, 4000, t);
            t += hop;
        }
        assert_eq!(c.hop_ms(), 5000, "pinned at max hop");
        assert!(c.rho(t) >= FLOOR_RHO, "rho over floor at max hop");
        assert!(c.cannot_keep_up(t), "hard floor flagged");

        for _ in 0..50 {
            c.report_decode(a, 150, t);
            t += 1000;
        }
        assert!(c.hop_ms() < 5000, "restored after sustained health (got {})", c.hop_ms());
        assert!(!c.cannot_keep_up(t), "floor cleared");
    }

    #[test]
    fn custom_ladder_is_used_and_bad_input_falls_back() {
        let c = CatchupController::with_ladder(vec![2000, 4000, 8000]);
        assert_eq!(c.hop_ms(), 2000, "custom ladder floor");
        // Malformed (not ascending) → default.
        let d = CatchupController::with_ladder(vec![3000, 1000]);
        assert_eq!(d.hop_ms(), DEFAULT_HOP_LADDER_MS[0]);
        // Empty → default.
        let e = CatchupController::with_ladder(vec![]);
        assert_eq!(e.hop_ms(), DEFAULT_HOP_LADDER_MS[0]);
    }
}
