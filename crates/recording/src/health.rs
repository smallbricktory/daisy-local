// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Continuous per-track capture health, fed from the live tee paths and
//! ticked by a 1 s timer. Distinguishes three failure shapes: no frames at
//! all (dead), frames of pure zeros (silent — a dead capture link), and
//! frames that carry nonzero but sub-audible samples (quiet — a live stream
//! whose level is merely low). Only the first two are stream
//! failures; quiet must never be treated as one.

use audio_engine::topology::{StreamDeathReason, Track, TopologyEvent};
use std::sync::atomic::{AtomicI16, AtomicU64, Ordering};
use std::sync::Arc;

/// Seconds without any frame before a track counts as dead.
pub const NO_FRAMES_SECS: u64 = 5;
/// Seconds of continuous zero samples (while frames flow) before a track
/// counts as silent; also the window for the quiet classification.
pub const ZERO_RUN_SECS: u64 = 10;
/// |sample| at or above this is audible signal (≈ −72 dBFS). Below it but
/// nonzero is quiet; only an exactly-zero stream is silence evidence.
const DEAD_PEAK_I16: i16 = 8;
/// Seconds with no sample above the device's learned dead-air floor (while
/// frames flow and the stream is neither silent nor quiet) before the
/// track counts as dead air.
pub const DEAD_AIR_SECS: u64 = 120;

/// Lock-free per-track counters, written from the frame path.
#[derive(Default)]
pub struct TrackHealth {
    /// Milliseconds-since-monitor-epoch of the newest frame; 0 = none yet.
    last_frame_ms: AtomicU64,
    /// Milliseconds of the newest frame carrying audible signal; 0 = none.
    last_sound_ms: AtomicU64,
    /// Milliseconds of the newest frame carrying ANY nonzero sample; 0 =
    /// none. Silence ends only when real samples actually arrive — a counter
    /// reset (rebaseline, rebuild) cannot fake a recovery.
    last_live_ms: AtomicU64,
    /// Learned dead-air peak floor; 0 = watchdog disabled for this track.
    dead_floor_i16: AtomicI16,
    /// Milliseconds of the newest frame with a sample above the floor.
    last_above_floor_ms: AtomicU64,
}

impl TrackHealth {
    /// Restart the delivery window only (mic switch / engine restart just
    /// happened). The sound window is untouched: an ongoing silence must
    /// survive a rebuild — only real audio ends it.
    pub fn rebaseline(&self, now_ms: u64) {
        self.last_frame_ms.store(now_ms.max(1), Ordering::Relaxed);
    }

    /// Restart all windows (resume after pause; the gap was commanded).
    fn grace(&self, now_ms: u64) {
        self.last_frame_ms.store(now_ms.max(1), Ordering::Relaxed);
        self.last_sound_ms.store(now_ms.max(1), Ordering::Relaxed);
        self.last_live_ms.store(now_ms.max(1), Ordering::Relaxed);
        self.last_above_floor_ms.store(now_ms.max(1), Ordering::Relaxed);
    }

    /// Arm (or re-arm on a device change) the dead-air floor. Restarts the
    /// window so a new device never inherits the previous device's
    /// starvation.
    pub fn set_dead_floor(&self, floor_i16: i16, now_ms: u64) {
        self.dead_floor_i16.store(floor_i16, Ordering::Relaxed);
        self.last_above_floor_ms.store(now_ms.max(1), Ordering::Relaxed);
    }

    /// Update the floor WITHOUT restarting the window: a same-device
    /// recovery rebuild must not look like a recovery — only real
    /// above-floor signal ends a dead-air bout (the rebaseline rule).
    pub fn refresh_dead_floor(&self, floor_i16: i16) {
        self.dead_floor_i16.store(floor_i16, Ordering::Relaxed);
    }

    /// Record one frame. `now_ms` shares the tick clock's epoch.
    pub fn observe(&self, samples: &[i16], now_ms: u64) {
        self.last_frame_ms.store(now_ms.max(1), Ordering::Relaxed);
        let peak = samples.iter().fold(0i16, |m, &s| m.max(s.saturating_abs()));
        if peak > 0 {
            self.last_live_ms.store(now_ms.max(1), Ordering::Relaxed);
        }
        if peak >= DEAD_PEAK_I16 {
            self.last_sound_ms.store(now_ms.max(1), Ordering::Relaxed);
        }
        let floor = self.dead_floor_i16.load(Ordering::Relaxed);
        if floor > 0 && peak >= floor {
            self.last_above_floor_ms.store(now_ms.max(1), Ordering::Relaxed);
        }
    }
}

struct TrackState {
    health: Arc<TrackHealth>,
    track: Track,
    dead: bool,
    silent: bool,
    quiet: bool,
    dead_air: bool,
}


/// Converts the raw counters into transition events, once per tick.
pub struct HealthMonitor {
    tracks: [TrackState; 2],
    /// Set while paused; the first unpaused tick re-baselines instead of
    /// reading pause-time starvation as death.
    was_paused: bool,
}

impl HealthMonitor {
    pub fn new(mic: Arc<TrackHealth>, system: Arc<TrackHealth>) -> Self {
        Self {
            tracks: [
                TrackState { health: mic, track: Track::Mic, dead: false, silent: false, quiet: false, dead_air: false },
                TrackState { health: system, track: Track::System, dead: false, silent: false, quiet: false, dead_air: false },
            ],
            was_paused: false,
        }
    }

    pub fn tick(&mut self, now_ms: u64, paused: bool) -> Vec<TopologyEvent> {
        if paused {
            self.was_paused = true;
            return Vec::new();
        }
        if self.was_paused {
            self.was_paused = false;
            for t in &self.tracks {
                t.health.grace(now_ms);
            }
            return Vec::new();
        }
        let mut out = Vec::new();
        for t in &mut self.tracks {
            let last = t.health.last_frame_ms.load(Ordering::Relaxed);
            // A system tap that has never delivered is idle, not dead — it
            // produces nothing until playback starts. The mic must always
            // deliver, so never-delivered mic still counts as dead.
            if t.track == Track::System && last == 0 {
                continue;
            }
            let stale = now_ms.saturating_sub(last) > NO_FRAMES_SECS * 1_000;
            if stale && !t.dead {
                t.dead = true;
                out.push(TopologyEvent::StreamDied {
                    track: t.track,
                    reason: StreamDeathReason::NoFrames,
                });
            } else if !stale && t.dead {
                t.dead = false;
            }

            // Silence needs zero-sample evidence: only a stream that stopped
            // producing ANY nonzero sample is a dead capture link. A stream
            // whose samples are nonzero but sub-audible is quiet — alive,
            // merely low — and is reported as its own state.
            let last_live = t.health.last_live_ms.load(Ordering::Relaxed);
            let silent = !stale
                && now_ms.saturating_sub(last_live.max(1)) > ZERO_RUN_SECS * 1_000;
            if silent && !t.silent {
                t.silent = true;
                out.push(TopologyEvent::SilenceStarted { track: t.track });
            } else if !silent && t.silent && !stale {
                t.silent = false;
                out.push(TopologyEvent::SilenceEnded { track: t.track });
            }

            let last_sound = t.health.last_sound_ms.load(Ordering::Relaxed);
            let quiet = !stale
                && !silent
                && now_ms.saturating_sub(last_sound.max(1)) > ZERO_RUN_SECS * 1_000;
            if quiet && !t.quiet {
                t.quiet = true;
                out.push(TopologyEvent::QuietStarted { track: t.track });
            } else if !quiet && t.quiet && !stale {
                t.quiet = false;
                out.push(TopologyEvent::QuietEnded { track: t.track });
            }

            let floor_on = t.health.dead_floor_i16.load(Ordering::Relaxed) > 0;
            let last_above = t.health.last_above_floor_ms.load(Ordering::Relaxed);
            let starved = floor_on
                && now_ms.saturating_sub(last_above.max(1)) > DEAD_AIR_SECS * 1_000;
            // Entry requires the stream to look otherwise healthy (silent
            // and quiet have their own correct handling); the EXIT is
            // signal-based only — a class flap (dither wandering across the
            // quiet threshold) must not read as a recovery.
            if starved && !stale && !silent && !quiet && !t.dead_air {
                t.dead_air = true;
                out.push(TopologyEvent::DeadAirStarted { track: t.track });
            } else if t.dead_air && !stale && !starved {
                t.dead_air = false;
                out.push(TopologyEvent::DeadAirEnded { track: t.track });
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn monitor() -> (Arc<TrackHealth>, Arc<TrackHealth>, HealthMonitor) {
        let mic = Arc::new(TrackHealth::default());
        let sys = Arc::new(TrackHealth::default());
        let m = HealthMonitor::new(Arc::clone(&mic), Arc::clone(&sys));
        (mic, sys, m)
    }

    fn died(ev: &TopologyEvent, want: Track) -> bool {
        matches!(ev, TopologyEvent::StreamDied { track, reason: StreamDeathReason::NoFrames } if *track == want)
    }

    #[test]
    fn no_frames_five_seconds_emits_stream_died_and_rearms() {
        let (mic, sys, mut m) = monitor();
        mic.observe(&[100i16; 320], 0);
        sys.observe(&[100i16; 320], 0);
        assert!(m.tick(1_000, false).is_empty());
        let evs = m.tick(6_000, false);
        assert_eq!(evs.len(), 2);
        assert!(evs.iter().any(|e| died(e, Track::Mic)));
        assert!(evs.iter().any(|e| died(e, Track::System)));
        // Latched while stale — no repeat.
        assert!(m.tick(7_000, false).is_empty());
        // Frames resume → re-arms, then fires again on the next stall.
        mic.observe(&[100i16; 320], 8_000);
        sys.observe(&[100i16; 320], 8_000);
        assert!(m.tick(8_500, false).is_empty());
        let evs = m.tick(15_000, false);
        assert_eq!(evs.len(), 2);
    }

    #[test]
    fn dead_from_first_sample_is_detected_for_mic_only() {
        let (_mic, _sys, mut m) = monitor();
        // The mic must deliver from t=0; a never-delivering system tap is
        // idle (nothing playing), not dead.
        let evs = m.tick(6_000, false);
        assert_eq!(evs.len(), 1);
        assert!(died(&evs[0], Track::Mic));
    }

    #[test]
    fn zeros_while_flowing_emit_silence_started_then_ended() {
        let (mic, sys, mut m) = monitor();
        let mut evs = Vec::new();
        for t in 0..12u64 {
            mic.observe(&[0i16; 16_000], t * 1_000);
            sys.observe(&[500i16; 16_000], t * 1_000);
            evs.extend(m.tick(t * 1_000, false));
        }
        assert!(evs
            .iter()
            .any(|e| matches!(e, TopologyEvent::SilenceStarted { track: Track::Mic })));
        assert!(!evs
            .iter()
            .any(|e| matches!(e, TopologyEvent::SilenceStarted { track: Track::System })));
        mic.observe(&[500i16; 320], 12_500);
        let evs = m.tick(13_000, false);
        assert!(evs
            .iter()
            .any(|e| matches!(e, TopologyEvent::SilenceEnded { track: Track::Mic })));
    }

    #[test]
    fn nonzero_quiet_emits_quiet_not_silence() {
        // A live but sub-audible stream carries tiny nonzero samples between
        // words; that must classify as Quiet — never Silent, which drives
        // mic recovery.
        let (mic, _sys, mut m) = monitor();
        let mut evs = Vec::new();
        for t in 0..12u64 {
            mic.observe(&[5i16; 16_000], t * 1_000);
            evs.extend(m.tick(t * 1_000, false));
        }
        assert!(evs
            .iter()
            .any(|e| matches!(e, TopologyEvent::QuietStarted { track: Track::Mic })));
        assert!(
            !evs.iter().any(|e| matches!(e, TopologyEvent::SilenceStarted { .. })),
            "quiet-but-alive stream classified silent"
        );
        // Audible signal ends the quiet bout.
        mic.observe(&[500i16; 320], 12_500);
        let evs = m.tick(13_000, false);
        assert!(evs
            .iter()
            .any(|e| matches!(e, TopologyEvent::QuietEnded { track: Track::Mic })));
        assert!(!evs.iter().any(|e| matches!(e, TopologyEvent::SilenceEnded { .. })));
    }

    #[test]
    fn quiet_stream_collapsing_to_zeros_becomes_silent() {
        let (mic, _sys, mut m) = monitor();
        let mut evs = Vec::new();
        for t in 0..12u64 {
            mic.observe(&[5i16; 16_000], t * 1_000);
            evs.extend(m.tick(t * 1_000, false));
        }
        assert!(evs
            .iter()
            .any(|e| matches!(e, TopologyEvent::QuietStarted { track: Track::Mic })));
        // The nonzero trickle stops; only zeros flow. Once the zero-run
        // window passes, the stream is a dead link: Quiet ends, Silent starts.
        let mut evs = Vec::new();
        for t in 13..25u64 {
            mic.observe(&[0i16; 16_000], t * 1_000);
            evs.extend(m.tick(t * 1_000, false));
        }
        assert!(evs
            .iter()
            .any(|e| matches!(e, TopologyEvent::QuietEnded { track: Track::Mic })));
        assert!(evs
            .iter()
            .any(|e| matches!(e, TopologyEvent::SilenceStarted { track: Track::Mic })));
    }

    #[test]
    fn rebaseline_during_silence_cannot_fake_recovery() {
        let (mic, _sys, mut m) = monitor();
        let mut evs = Vec::new();
        for t in 0..12u64 {
            mic.observe(&[0i16; 16_000], t * 1_000);
            evs.extend(m.tick(t * 1_000, false));
        }
        assert!(evs
            .iter()
            .any(|e| matches!(e, TopologyEvent::SilenceStarted { track: Track::Mic })));
        // A rebuild re-baselines; zeros keep flowing. Silence must NOT end —
        // only real sound ends it (a fake end resets the recovery ladder).
        mic.rebaseline(12_500);
        mic.observe(&[0i16; 16_000], 13_000);
        let evs = m.tick(13_000, false);
        assert!(
            !evs.iter().any(|e| matches!(e, TopologyEvent::SilenceEnded { .. })),
            "rebaseline faked a recovery"
        );
        // Real sound ends it once the silence window clears.
        mic.observe(&[900i16; 320], 24_000);
        let evs = m.tick(24_000, false);
        assert!(evs
            .iter()
            .any(|e| matches!(e, TopologyEvent::SilenceEnded { track: Track::Mic })));
    }

    #[test]
    fn sub_floor_dither_emits_dead_air_started_then_ended() {
        let (mic, _sys, mut m) = monitor();
        // Learned floor 80 ≈ -52 dBFS. Dither at 20 is above DEAD_PEAK_I16
        // (not quiet, not silent) but below the floor.
        mic.set_dead_floor(80, 0);
        let mut evs = Vec::new();
        for t in 0..125u64 {
            mic.observe(&[20i16; 16_000], t * 1_000);
            evs.extend(m.tick(t * 1_000, false));
        }
        assert!(evs
            .iter()
            .any(|e| matches!(e, TopologyEvent::DeadAirStarted { track: Track::Mic })));
        assert!(!evs.iter().any(|e| matches!(e, TopologyEvent::SilenceStarted { .. })));
        assert!(!evs.iter().any(|e| matches!(e, TopologyEvent::QuietStarted { .. })));
        // One sample above the floor ends the bout.
        mic.observe(&[500i16; 320], 126_000);
        let evs = m.tick(126_000, false);
        assert!(evs
            .iter()
            .any(|e| matches!(e, TopologyEvent::DeadAirEnded { track: Track::Mic })));
    }

    #[test]
    fn dead_air_never_fires_without_a_floor() {
        let (mic, _sys, mut m) = monitor();
        let mut evs = Vec::new();
        for t in 0..125u64 {
            mic.observe(&[20i16; 16_000], t * 1_000);
            evs.extend(m.tick(t * 1_000, false));
        }
        assert!(!evs.iter().any(|e| matches!(e, TopologyEvent::DeadAirStarted { .. })));
    }

    #[test]
    fn dead_air_never_fires_while_quiet_or_silent() {
        // Quiet-class dither (below DEAD_PEAK_I16): stays Quiet, never DeadAir.
        let (mic, _sys, mut m) = monitor();
        mic.set_dead_floor(80, 0);
        let mut evs = Vec::new();
        for t in 0..125u64 {
            mic.observe(&[5i16; 16_000], t * 1_000);
            evs.extend(m.tick(t * 1_000, false));
        }
        assert!(evs
            .iter()
            .any(|e| matches!(e, TopologyEvent::QuietStarted { track: Track::Mic })));
        assert!(!evs.iter().any(|e| matches!(e, TopologyEvent::DeadAirStarted { .. })));
        // Pure zeros: Silent owns it, never DeadAir.
        let (mic2, _sys2, mut m2) = monitor();
        mic2.set_dead_floor(80, 0);
        let mut evs = Vec::new();
        for t in 0..125u64 {
            mic2.observe(&[0i16; 16_000], t * 1_000);
            evs.extend(m2.tick(t * 1_000, false));
        }
        assert!(evs
            .iter()
            .any(|e| matches!(e, TopologyEvent::SilenceStarted { track: Track::Mic })));
        assert!(!evs.iter().any(|e| matches!(e, TopologyEvent::DeadAirStarted { .. })));
    }

    #[test]
    fn set_dead_floor_restarts_the_window() {
        let (mic, _sys, mut m) = monitor();
        mic.set_dead_floor(80, 0);
        let mut evs = Vec::new();
        for t in 0..60u64 {
            mic.observe(&[20i16; 16_000], t * 1_000);
            evs.extend(m.tick(t * 1_000, false));
        }
        // Mic switch at t=60 re-arms the floor: no event until 120s later.
        mic.set_dead_floor(80, 60_000);
        for t in 60..170u64 {
            mic.observe(&[20i16; 16_000], t * 1_000);
            evs.extend(m.tick(t * 1_000, false));
        }
        assert!(!evs.iter().any(|e| matches!(e, TopologyEvent::DeadAirStarted { .. })));
        let mut evs = Vec::new();
        for t in 170..185u64 {
            mic.observe(&[20i16; 16_000], t * 1_000);
            evs.extend(m.tick(t * 1_000, false));
        }
        assert!(evs
            .iter()
            .any(|e| matches!(e, TopologyEvent::DeadAirStarted { track: Track::Mic })));
    }

    #[test]
    fn paused_ticks_are_ignored_and_unpause_rebaselines() {
        let (mic, _sys, mut m) = monitor();
        mic.observe(&[100i16; 320], 0);
        assert!(m.tick(60_000, true).is_empty());
        // First unpaused tick re-baselines rather than reading the pause as death.
        assert!(m.tick(61_000, false).is_empty());
        assert!(m.tick(62_000, false).is_empty());
        let evs = m.tick(68_000, false);
        assert_eq!(evs.len(), 2);
    }
}
