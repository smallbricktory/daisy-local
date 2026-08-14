//! Global, lock-free metrics for the live whisper decoder, sampled by the
//! app's perf telemetry. One decoder runs per session; a single
//! process-global holds the state. Reset at record start.

use std::sync::atomic::{AtomicU64, Ordering};

static LAST_DECODE_MS: AtomicU64 = AtomicU64::new(0);
static WINDOW_MS: AtomicU64 = AtomicU64::new(0);
/// Audio that was queued-but-undecoded and collapsed in the last drain: how
/// far behind real-time the decoder was at that moment.
static BACKLOG_MS: AtomicU64 = AtomicU64::new(0);
static MAX_BACKLOG_MS: AtomicU64 = AtomicU64::new(0);
static DECODES: AtomicU64 = AtomicU64::new(0);
/// Controller state: current shared hop (ms) and global serialized utilization
/// `rho` ×100 (integer-stored). `rho` ≥ 100 means the decoder is at/over its
/// stability bound and the controller is shedding by raising the hop.
static HOP_MS: AtomicU64 = AtomicU64::new(0);
static RHO_X100: AtomicU64 = AtomicU64::new(0);
/// 1 when the decoder cannot keep up even at max hop (hard floor).
static FLOOR: AtomicU64 = AtomicU64::new(0);
/// Time the last decode spent waiting for the shared decode permit
/// (two-track contention). Kept out of `rho`.
static WAIT_MS: AtomicU64 = AtomicU64::new(0);

/// Record one completed decode: its wall-clock cost and the window it ran on.
/// `decode_ms` vs the hop is the real-time factor; `window_ms` shows how much
/// overlapping audio is being re-processed each hop.
pub fn record_decode(decode_ms: u64, window_ms: u64) {
    LAST_DECODE_MS.store(decode_ms, Ordering::Relaxed);
    WINDOW_MS.store(window_ms, Ordering::Relaxed);
    DECODES.fetch_add(1, Ordering::Relaxed);
}

/// Record the cross-track controller's hop, global utilization `rho`, and the
/// hard-floor flag.
pub fn record_controller(hop_ms: i64, rho: f64, floor: bool) {
    HOP_MS.store(hop_ms.max(0) as u64, Ordering::Relaxed);
    RHO_X100.store((rho * 100.0).max(0.0) as u64, Ordering::Relaxed);
    FLOOR.store(floor as u64, Ordering::Relaxed);
}

/// Record the semaphore-wait (ms) of the last decode (two-track contention).
pub fn record_wait(wait_ms: u64) {
    WAIT_MS.store(wait_ms, Ordering::Relaxed);
}

/// Record the backlog (ms of audio) collapsed in one drain.
pub fn record_backlog(backlog_ms: u64) {
    BACKLOG_MS.store(backlog_ms, Ordering::Relaxed);
    MAX_BACKLOG_MS.fetch_max(backlog_ms, Ordering::Relaxed);
}

/// Clear all counters — call at record start.
pub fn reset() {
    for a in [
        &LAST_DECODE_MS,
        &WINDOW_MS,
        &BACKLOG_MS,
        &MAX_BACKLOG_MS,
        &DECODES,
        &HOP_MS,
        &RHO_X100,
        &FLOOR,
        &WAIT_MS,
    ] {
        a.store(0, Ordering::Relaxed);
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct LiveMetrics {
    pub last_decode_ms: u64,
    pub window_ms: u64,
    pub backlog_ms: u64,
    pub max_backlog_ms: u64,
    pub decodes: u64,
    pub hop_ms: u64,
    /// Global serialized utilization `rho` ×100.
    pub rho_x100: u64,
    /// Hard floor: decoder can't keep up even at max hop.
    pub floor: bool,
    /// Semaphore-wait (ms) of the last decode — two-track contention.
    pub wait_ms: u64,
}

pub fn snapshot() -> LiveMetrics {
    LiveMetrics {
        last_decode_ms: LAST_DECODE_MS.load(Ordering::Relaxed),
        window_ms: WINDOW_MS.load(Ordering::Relaxed),
        backlog_ms: BACKLOG_MS.load(Ordering::Relaxed),
        max_backlog_ms: MAX_BACKLOG_MS.load(Ordering::Relaxed),
        decodes: DECODES.load(Ordering::Relaxed),
        hop_ms: HOP_MS.load(Ordering::Relaxed),
        rho_x100: RHO_X100.load(Ordering::Relaxed),
        floor: FLOOR.load(Ordering::Relaxed) != 0,
        wait_ms: WAIT_MS.load(Ordering::Relaxed),
    }
}
