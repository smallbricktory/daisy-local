// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Topology supervisor: consumes [`TopologyEvent`]s from the per-OS watcher,
//! the capture streams, and the health monitor; maintains the device
//! snapshot; reacts with policies; surfaces user-facing notices.

use audio_engine::source::Source;
use audio_engine::topology::{DeviceKey, Snapshot, TopologyEvent, Track};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime};

/// User-facing outcomes; the serialized value IS the frontend event payload
/// (`kind` tag + fields — see the frontend's AudioWarning union).
/// Device-list pushes are NOT here — the app-lifetime topology hub
/// subscriber emits those, recording or not.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Notice {
    /// A BT mic's card was downshifted to CVSD; persist the preference.
    /// Handled by the notice drain, never emitted to the frontend.
    CodecDownshifted { mac_stem: String },
    MicFailedOver { to: String, id: u32 },
    MicMutedExternally,
    /// The mic stream is alive but its level is sub-audible. Informational —
    /// recording continues.
    MicVeryQuiet,
    /// The mic delivers only zeros and no physical alternative exists (the
    /// only other inputs are virtual devices). Recording stays on the held
    /// mic and rebuilds it on a slow cadence until it produces signal again
    /// (e.g. the OS call holding it ends).
    MicRetrying,
    /// The mic delivers only sub-floor dither (dead air) while the stream
    /// looks alive; recovery is running. Warns the user to check the input.
    MicDeadAir,
    /// Another process holds the mic and ours reads pure silence. Nothing to
    /// fix and nothing to retry — the device returns when that process lets
    /// go, and rebuilding at it meanwhile wedges capture.
    MicHeldByCall,
    /// The held mic produces signal again (the holder released it); clears
    /// the retrying/silent warnings.
    MicRecovered,
    /// The AEC far-end reference has been starving for a sustained period —
    /// system audio capture stopped delivering while the mic still flows.
    SystemAudioLost,
    /// Reference frames resumed; clears the lost warning.
    SystemAudioRecovered,
    ReconnectFailed { track: Track },
}

/// The recording's mic identity: numeric id, stable key, and the previously
/// active key. One lock — the three always change together, so readers never
/// see a new id paired with an old key.
pub struct MicIdentity {
    pub id: u32,
    pub key: DeviceKey,
    pub prev_key: Option<DeviceKey>,
}

/// Performs a mic switch to the given source; returns false when it failed.
pub type SwitchFn = Arc<dyn Fn(&Source) -> bool + Send + Sync>;
/// Rebuilds the system stream on the current default output.
pub type ReopenFn = Arc<dyn Fn() -> bool + Send + Sync>;
/// Repoints the capture sink's loopback at the new default output and
/// resweeps playback streams into it. No-op without a Daisy-owned sink.
pub type RerouteFn = Arc<dyn Fn() + Send + Sync>;
/// Routes one new playback stream into the capture sink.
pub type RouteStreamFn = Arc<dyn Fn(u32) + Send + Sync>;

/// What a mic that keeps streaming but yields only zeros means here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MicSilence {
    /// A stale link a rebuild fixes: a post-Bluetooth PipeWire relink keeps
    /// Streaming and yields zeros forever while a fresh stream on the same
    /// device delivers audio.
    Rebuild,
    /// Another process's VoiceProcessingIO client silencing our raw capture
    /// (FaceTime/Phone call). Rebuild churn against it wedges this process's
    /// CoreAudio client until the app is relaunched — every AudioUnit it
    /// opens afterwards yields zeros — while leaving the device alone gets
    /// it back within a second of the call ending. The macOS backend
    /// escalates itself to VPIO when it detects this (capture-layer
    /// zero-run + new capturer); this variant covers the window before that
    /// escalation engages and the case where it fails.
    WaitForRelease,
}

/// How the platform's system-audio capture relates to the default output.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SystemShape {
    /// Bound to the default at open; a default switch strands it (Windows).
    ReopenOnDefaultChange,
    /// Follows routing on its own, or is handled outside the supervisor.
    Passive,
}
/// Returns the current source list; production = `list_sources()`.
pub type EnumerateFn = Arc<dyn Fn() -> Vec<Source> + Send + Sync>;
/// Returns the OS default input key; production = `default_input_key()`.
pub type DefaultInputFn = Arc<dyn Fn() -> Option<DeviceKey> + Send + Sync>;

/// Wall time that passed beyond monotonic time — the machine was asleep.
pub fn wake_gap(wall_delta: Duration, mono_delta: Duration) -> Duration {
    wall_delta.saturating_sub(mono_delta)
}

/// Sleep gap below this is clock jitter, not a suspend.
const WAKE_GAP_MIN: Duration = Duration::from_secs(5);
#[cfg(not(test))]
const TICK: Duration = Duration::from_secs(1);
/// The unit-test suite runs the loop 10× compressed; every sleep/margin in
/// `mod tests` is scaled to these values.
#[cfg(test)]
const TICK: Duration = Duration::from_millis(100);
/// Reconnect attempts per track before giving up (until a healthy interval).
const RECONNECT_MAX: u8 = 3;
#[cfg(not(test))]
const RECONNECT_BACKOFF: Duration = Duration::from_millis(500);
#[cfg(test)]
const RECONNECT_BACKOFF: Duration = Duration::from_millis(50);
/// This long without a death on a track resets its reconnect budget.
const HEALTHY_RESET: Duration = Duration::from_secs(30);
/// Minimum gap between mic-silence bouts that are allowed to drive
/// recovery. Zero-run bouts inside the gap are classified and logged but
/// never rebuild — a device flapping between zeros and audio cannot earn a
/// fresh rebuild ladder per flap.
#[cfg(not(test))]
const SILENCE_BOUT_MIN_GAP: Duration = Duration::from_secs(300);
#[cfg(test)]
const SILENCE_BOUT_MIN_GAP: Duration = Duration::from_millis(1_500);
/// Rebuild cadence for a held mic (zeros, no physical alternative). Slow,
/// budget-free: the device heals when whatever holds it (an OS call app)
/// releases it, so recovery must outlive the reconnect budget.
#[cfg(not(test))]
const HELD_RETRY_GAP: Duration = Duration::from_secs(15);
#[cfg(test)]
const HELD_RETRY_GAP: Duration = Duration::from_millis(500);
/// External volume change smaller than this (vs the volume of the last
/// re-baseline) does not re-baseline. Call-app AGC flaps the input volume
/// by a fraction of a dB every few seconds (Teams: 0.75↔0.79); each
/// re-baseline discards AutoGain state, so flapping must not qualify.
/// Drift accumulates against the anchor — many small steps in one
/// direction still re-baseline once they add up.
const REBASELINE_DEADBAND: f32 = 0.05;

/// Mic zero-run bout tracking: whether a bout is active and whether this
/// bout may drive recovery (rate-limited by [`SILENCE_BOUT_MIN_GAP`]).
#[derive(Default)]
struct SilenceBout {
    silent: bool,
    /// True while the zero-run silence class is active (as opposed to a
    /// bout opened by dead air).
    zero_run: bool,
    /// A user-facing warning (MicDeadAir / MicRetrying) went out for this
    /// bout; its close must send MicRecovered to clear the toast.
    warned: bool,
    recovers: bool,
    last_recovering: Option<Instant>,
}

impl SilenceBout {
    fn start(&mut self) {
        self.silent = true;
        self.recovers = self
            .last_recovering
            .is_none_or(|t| t.elapsed() >= SILENCE_BOUT_MIN_GAP);
        if self.recovers {
            self.last_recovering = Some(Instant::now());
        } else {
            log::warn!(
                "supervisor: mic silence recovery suppressed — previous bout too recent"
            );
        }
    }
}

#[derive(Default)]
struct ReconnectState {
    attempts: u8,
    last_death: Option<Instant>,
    gave_up: bool,
}

impl ReconnectState {
    fn on_healthy_check(&mut self, now: Instant) {
        if let Some(t) = self.last_death {
            if now.duration_since(t) >= HEALTHY_RESET {
                self.reset();
            }
        }
    }

    /// Note a death and take one attempt from the budget. Marks gave-up and
    /// sends `ReconnectFailed` once the budget is spent.
    fn try_attempt(&mut self, track: Track, why: &str, notices: &Sender<Notice>) -> bool {
        self.last_death = Some(Instant::now());
        if self.gave_up {
            return false;
        }
        if self.attempts >= RECONNECT_MAX {
            self.gave_up = true;
            log::error!("supervisor: {track:?} recovery budget exhausted ({why})");
            let _ = notices.send(Notice::ReconnectFailed { track });
            return false;
        }
        self.attempts += 1;
        true
    }

    fn reset(&mut self) {
        self.attempts = 0;
        self.gave_up = false;
        self.last_death = None;
    }
}

/// Recent gain values we wrote ourselves; an OS volume event matching one
/// within the window is our own write echoed back, not an external change.
#[derive(Default)]
pub struct SelfWrites {
    entries: std::collections::VecDeque<(u32, f32, u64)>,
}

impl SelfWrites {
    const WINDOW_MS: u64 = 2_000;

    pub fn mark(&mut self, id: u32, gain: f32, now_ms: u64) {
        self.entries.push_back((id, gain, now_ms));
        while let Some(&(_, _, t)) = self.entries.front() {
            if now_ms.saturating_sub(t) > Self::WINDOW_MS {
                self.entries.pop_front();
            } else {
                break;
            }
        }
    }

    pub fn matches(&self, id: u32, gain: f32, now_ms: u64) -> bool {
        // Devices quantize written gains to their own step grid before echoing
        // them back (coarsest seen: 1/6 steps, 0.80 → 0.8416667), so the match
        // tolerance must cover one full step or the echo reads as an external
        // change and the supervisor rewrites forever.
        const STEP: f32 = 0.17;
        self.entries.iter().any(|&(i, g, t)| {
            i == id && (g - gain).abs() < STEP && now_ms.saturating_sub(t) <= Self::WINDOW_MS
        })
    }
}

pub struct SupervisorDeps {
    pub events_rx: Receiver<TopologyEvent>,
    pub snapshot: Arc<RwLock<Snapshot>>,
    /// Identity of the mic the recording is using; switches update it.
    /// Failover prefers its `prev_key`.
    pub mic: Arc<Mutex<MicIdentity>>,
    pub paused: Arc<AtomicBool>,
    pub switch: SwitchFn,
    pub reopen_system: ReopenFn,
    pub reroute: RerouteFn,
    pub route_stream: RouteStreamFn,
    pub enumerate: EnumerateFn,
    /// The OS default input; the failover ladder ranks it below the last
    /// working mic and above list order.
    pub default_input: DefaultInputFn,
    /// Live AEC engagement — the bridge samples it per frame.
    pub aec_live: Arc<AtomicBool>,
    /// A user override pins the AEC decision; output changes must not
    /// toggle `aec_live` or escalate the sticky flag then.
    pub aec_forced: bool,
    /// Gain writes the auto-gain tap made (self-echo suppression).
    pub self_writes: Arc<Mutex<SelfWrites>>,
    /// Epoch shared with the tap's SelfWrites stamps.
    pub gain_epoch: Instant,
    /// Re-baselines auto-gain to the active mic without a device change.
    pub rebaseline: Arc<dyn Fn() + Send + Sync>,
    /// Sticky "AEC ever needed" flag driving finalize; only escalates.
    pub needs_aec_sticky: Arc<AtomicBool>,
    /// True while the mic capture is OS-voice-processed (VPIO / comms
    /// mode). Those streams gate inter-speech residue to exact zeros, so a
    /// level-based "very quiet" reading carries no signal there.
    pub mic_voice_processed: Arc<AtomicBool>,
    pub system_shape: SystemShape,
    /// Whether a zero-run mic bout may drive rebuilds on this platform.
    pub mic_silence: MicSilence,
}

pub struct SupervisorHandle {
    pub notices: Receiver<Notice>,
    stop: Arc<AtomicBool>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl SupervisorHandle {
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

impl Drop for SupervisorHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

pub struct Supervisor;

impl Supervisor {
    /// The notices channel is created by the caller so other producers
    /// (the live pipeline's AEC reference watchdog) can hold a sender to
    /// the same drain.
    pub fn start(
        deps: SupervisorDeps,
        notices_tx: Sender<Notice>,
        notices_rx: Receiver<Notice>,
    ) -> SupervisorHandle {
        let stop = Arc::new(AtomicBool::new(false));
        let stop_loop = Arc::clone(&stop);
        let join = std::thread::Builder::new()
            .name("daisy-supervisor".into())
            .spawn(move || run_loop(deps, notices_tx, stop_loop))
            .expect("spawn supervisor thread");
        SupervisorHandle {
            notices: notices_rx,
            stop,
            join: Some(join),
        }
    }
}

/// Seed a snapshot from an enumeration pass.
pub fn seed_snapshot(snapshot: &RwLock<Snapshot>, sources: &[Source]) {
    let mut snap = snapshot.write().unwrap();
    for s in sources {
        snap.upsert(s.key.clone(), Some(s.id), s.kind.clone());
    }
}

fn run_loop(deps: SupervisorDeps, notices: Sender<Notice>, stop: Arc<AtomicBool>) {
    seed_snapshot(&deps.snapshot, &(deps.enumerate)());

    let mut last_mono = Instant::now();
    let mut last_wall = SystemTime::now();
    // Set on device churn; the next tick re-enumerates once (debounce).
    let mut needs_refresh = false;
    let mut mic_rc = ReconnectState::default();
    let mut sys_rc = ReconnectState::default();
    let mut mic_silence = SilenceBout::default();
    // Some(last rebuild time) while the active mic is held (zeros, no
    // physical alternative); drives the slow budget-free retry loop.
    let mut held_retry: Option<Instant> = None;
    let mut pending_reopen = false;
    let mut pending_reroute = false;
    // Active-mic volume at the last AGC (re)baseline; the deadband reference.
    let mut vol_anchor: Option<(DeviceKey, f32)> = None;

    loop {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        mic_rc.on_healthy_check(Instant::now());
        sys_rc.on_healthy_check(Instant::now());
        match deps.events_rx.recv_timeout(TICK) {
            Ok(ev) => {
                handle_event(
                    &deps,
                    &notices,
                    ev,
                    &mut needs_refresh,
                    &mut mic_rc,
                    &mut sys_rc,
                    &mut mic_silence,
                    &mut held_retry,
                    &mut pending_reopen,
                    &mut pending_reroute,
                    &mut vol_anchor,
                );
            }
            Err(RecvTimeoutError::Timeout) => {
                let mono_delta = last_mono.elapsed();
                let wall_delta = last_wall.elapsed().unwrap_or(mono_delta);
                last_mono = Instant::now();
                last_wall = SystemTime::now();
                let gap = wake_gap(wall_delta, mono_delta);
                if gap >= WAKE_GAP_MIN {
                    handle_event(
                        &deps,
                        &notices,
                        TopologyEvent::SystemWoke { gap },
                        &mut needs_refresh,
                        &mut mic_rc,
                        &mut sys_rc,
                        &mut mic_silence,
                        &mut held_retry,
                        &mut pending_reopen,
                        &mut pending_reroute,
                        &mut vol_anchor,
                    );
                }
                if needs_refresh {
                    needs_refresh = false;
                    seed_snapshot(&deps.snapshot, &(deps.enumerate)());
                }
                let wait_it_out = mic_silence.zero_run
                    && deps.mic_silence == MicSilence::WaitForRelease;
                if !deps.paused.load(Ordering::Relaxed) && !wait_it_out {
                    if let Some(last) = held_retry {
                        if last.elapsed() >= HELD_RETRY_GAP {
                            held_retry = Some(Instant::now());
                            retry_held_mic(&deps, &notices);
                        }
                    } else if mic_silence.silent && mic_silence.recovers {
                        recover_mic(
                            &deps,
                            &notices,
                            &mut mic_rc,
                            &mut mic_silence,
                            &mut held_retry,
                            "silent while streaming",
                        );
                    }
                }
                if std::mem::take(&mut pending_reopen) {
                    let ok = (deps.reopen_system)();
                    log::info!(
                        "supervisor: default output moved — system reopen {}",
                        if ok { "succeeded" } else { "failed" }
                    );
                }
                if std::mem::take(&mut pending_reroute) {
                    (deps.reroute)();
                }
            }
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// Resolve the source to fail over to: the last mic that worked, else the
/// OS default input, else any other mic — never the one that just vanished.
pub(crate) fn failover_target(
    sources: &[Source],
    lost: &DeviceKey,
    prev: Option<&DeviceKey>,
    default_key: Option<&DeviceKey>,
) -> Option<Source> {
    // Every branch is Mic-only AND physical-only: the OS default input can
    // legitimately be a monitor (screen-recorder setups) — failing the mic
    // stream onto one would record system audio twice — and a virtual input
    // yields only zeros, so landing there loses the rest of the meeting.
    // (Explicitly selecting a virtual mic is still allowed; the filter is
    // for automatic failover only.)
    let is_mic = |s: &&Source| {
        s.kind == audio_engine::source::SourceKind::Mic && !s.is_virtual && s.key != *lost
    };
    if let Some(pk) = prev {
        if let Some(s) = sources.iter().filter(is_mic).find(|s| s.key == *pk) {
            return Some(s.clone());
        }
    }
    if let Some(dk) = default_key {
        if let Some(s) = sources.iter().filter(is_mic).find(|s| s.key == *dk) {
            return Some(s.clone());
        }
    }
    sources.iter().find(is_mic).cloned()
}

/// Resolve the mic to (re)build on: the lost/active key itself when
/// `rebuild_same` and it is still enumerable, else the failover ladder
/// (prev mic → OS default → any other mic).
fn resolve_mic_target(
    deps: &SupervisorDeps,
    lost: &DeviceKey,
    sources: &[Source],
    rebuild_same: bool,
) -> Option<Source> {
    if rebuild_same {
        if let Some(s) = sources.iter().find(|s| s.key == *lost) {
            return Some(s.clone());
        }
    }
    let prev = deps.mic.lock().unwrap().prev_key.clone();
    let default_key = (deps.default_input)();
    failover_target(sources, lost, prev.as_ref(), default_key.as_ref())
}

/// Issue a mic switch, keeping the prev/active key bookkeeping and the
/// failover notice consistent across every recovery path.
fn switch_to(
    deps: &SupervisorDeps,
    notices: &Sender<Notice>,
    target: &Source,
    lost: &DeviceKey,
) -> bool {
    let failed_over = target.key != *lost;
    let ok = (deps.switch)(target);
    if ok {
        {
            let mut mic = deps.mic.lock().unwrap();
            if failed_over {
                mic.prev_key = Some(lost.clone());
            }
            mic.id = target.id;
            mic.key = target.key.clone();
        }
        if failed_over {
            let _ = notices.send(Notice::MicFailedOver {
                to: target.description.clone(),
                id: target.id,
            });
        }
    }
    ok
}

/// One bounded mic-recovery attempt: rebuild on the active key while budget
/// remains, fail over to a different mic on the last attempt. When the last
/// attempt finds no physical mic to fail over to, stays on the active device
/// and engages the slow held-mic retry loop instead. Returns false once the
/// budget is spent.
fn recover_mic(
    deps: &SupervisorDeps,
    notices: &Sender<Notice>,
    rc: &mut ReconnectState,
    mic_silence: &mut SilenceBout,
    held_retry: &mut Option<Instant>,
    why: &str,
) -> bool {
    if !rc.try_attempt(Track::Mic, why, notices) {
        return false;
    }
    // Backoff AFTER the budget check: a death storm past the budget must
    // not block the supervisor thread 500 ms per event.
    std::thread::sleep(RECONNECT_BACKOFF);
    let last_attempt = rc.attempts >= RECONNECT_MAX;
    let active = deps.mic.lock().unwrap().key.clone();
    // A silent Bluetooth mic on an active headset profile is the broken-mSBC
    // signature; downshift the card to CVSD before rebuilding the stream.
    // (No-op for non-Bluetooth keys — `downshift_codec` resolves the BT
    // address from the key and bails when there is none.)
    match audio_engine::bt_profile::downshift_codec(&active.0) {
        Ok(true) => {
            if let Some(mac) = audio_engine::bt_profile::bluez_address(&active.0) {
                let _ = notices.send(Notice::CodecDownshifted { mac_stem: mac });
            }
            std::thread::sleep(Duration::from_millis(250));
        }
        Ok(false) => {}
        Err(e) => log::warn!("supervisor: codec downshift failed: {e}"),
    }
    let sources = (deps.enumerate)();
    // On the last attempt, rebuilds on the same device did not help; try a
    // different mic.
    let mut target = resolve_mic_target(deps, &active, &sources, !last_attempt);
    if target.is_none() && last_attempt {
        // No physical mic to fail over to: stay on the held device and let
        // the slow retry loop rebuild it until it produces signal again.
        *held_retry = Some(Instant::now());
        log::warn!(
            "supervisor: no physical mic to fail over to — staying on '{}', retrying every {}s",
            active.0,
            HELD_RETRY_GAP.as_secs()
        );
        mic_silence.warned = true;
        let _ = notices.send(Notice::MicRetrying);
        target = sources.iter().find(|s| s.key == active).cloned();
    }
    let ok = match target {
        Some(t) => switch_to(deps, notices, &t, &active),
        None => false,
    };
    log::warn!(
        "supervisor: mic recovery ({why}) attempt {} {}",
        rc.attempts,
        if ok { "issued" } else { "failed" }
    );
    ok
}

/// Close the mic bout: signal is back (the held device healed or a rebuild
/// took). Shared exit for SilenceEnded and DeadAirEnded — resets the
/// reconnect budget and stops the slow held-retry loop.
fn end_mic_bout(
    mic_silence: &mut SilenceBout,
    mic_rc: &mut ReconnectState,
    held_retry: &mut Option<Instant>,
    notices: &Sender<Notice>,
) {
    mic_silence.silent = false;
    mic_rc.reset();
    held_retry.take();
    if std::mem::take(&mut mic_silence.warned) {
        log::info!("supervisor: held mic recovered — signal is back");
        let _ = notices.send(Notice::MicRecovered);
    }
}

/// One budget-free rebuild of the held mic; runs on the [`HELD_RETRY_GAP`]
/// cadence until the mic delivers signal again (SilenceEnded clears the
/// held state).
fn retry_held_mic(deps: &SupervisorDeps, notices: &Sender<Notice>) {
    let active = deps.mic.lock().unwrap().key.clone();
    let sources = (deps.enumerate)();
    let Some(t) = sources.iter().find(|s| s.key == active).cloned() else {
        log::warn!("supervisor: held-mic retry — '{}' not enumerable, waiting", active.0);
        return;
    };
    let ok = switch_to(deps, notices, &t, &active);
    log::info!(
        "supervisor: held-mic retry rebuild {}",
        if ok { "issued" } else { "failed" }
    );
}

#[allow(clippy::too_many_arguments)]
fn handle_event(
    deps: &SupervisorDeps,
    notices: &Sender<Notice>,
    ev: TopologyEvent,
    needs_refresh: &mut bool,
    mic_rc: &mut ReconnectState,
    sys_rc: &mut ReconnectState,
    mic_silence: &mut SilenceBout,
    held_retry: &mut Option<Instant>,
    pending_reopen: &mut bool,
    pending_reroute: &mut bool,
    vol_anchor: &mut Option<(DeviceKey, f32)>,
) {
    // The info echo of every event is the field-diagnosis trail AND the
    // topology-trace-check.sh contract. VolumeChanged alone is demoted:
    // it fires for every device before the active-mic filter, so a
    // volume-slider drag would flood the log.
    if matches!(ev, TopologyEvent::VolumeChanged { .. }) {
        log::debug!("supervisor: {ev:?}");
    } else {
        log::info!("supervisor: {ev:?}");
    }
    match ev {
        TopologyEvent::DeviceAdded { key, kind } => {
            {
                let mut snap = deps.snapshot.write().unwrap();
                // Present immediately; the numeric id arrives with the next
                // enumeration pass.
                if !snap.contains(&key) {
                    snap.upsert(key, None, kind);
                }
            }
            *needs_refresh = true;
        }
        TopologyEvent::DeviceRemoved { key } => {
            deps.snapshot.write().unwrap().remove(&key);
            *needs_refresh = true;
            let lost_active = deps.mic.lock().unwrap().key == key;
            if lost_active && !deps.paused.load(Ordering::Relaxed) {
                let sources = (deps.enumerate)();
                let switched = resolve_mic_target(deps, &key, &sources, false)
                    .map(|target| {
                        let ok = switch_to(deps, notices, &target, &key);
                        if ok {
                            log::warn!(
                                "supervisor: active mic '{}' vanished — failed over to '{}'",
                                key.0,
                                target.description
                            );
                        }
                        ok
                    })
                    .unwrap_or(false);
                if !switched {
                    let _ = notices.send(Notice::ReconnectFailed { track: Track::Mic });
                }
            }
        }
        TopologyEvent::DefaultInputChanged { .. } => {
            // Logged only. Failover queries the OS default fresh at decision
            // time (`default_input_key`), so no state is kept here.
        }
        TopologyEvent::DefaultOutputChanged { class, .. } => {
            if !deps.aec_forced {
                let engage = class.needs_aec();
                if deps.aec_live.swap(engage, Ordering::Relaxed) != engage {
                    log::info!(
                        "supervisor: live AEC {} (default output now {class:?})",
                        if engage { "engaged" } else { "disengaged" }
                    );
                }
                if engage {
                    deps.needs_aec_sticky.store(true, Ordering::Relaxed);
                }
            }
            if deps.system_shape == SystemShape::ReopenOnDefaultChange {
                // One reopen per churn burst, on the trailing edge.
                *pending_reopen = true;
            }
            // Sink plumbing coalesces on the same trailing edge: repointing
            // the loopback restarts it, so once per burst, not per event.
            *pending_reroute = true;
        }
        TopologyEvent::DeadAirStarted { track } => {
            // Same bout machinery as zero-run silence, plus a warning — a
            // stream this far under the device's learned floor reads as a
            // healthy recording while capturing nothing usable.
            if track == Track::Mic && !mic_silence.silent {
                mic_silence.start();
                mic_silence.warned = true;
                let _ = notices.send(Notice::MicDeadAir);
            }
        }
        TopologyEvent::DeadAirEnded { track } => {
            // If the zero-run silence class took over, that class owns the
            // bout until real audio returns.
            if track == Track::Mic && !mic_silence.zero_run {
                end_mic_bout(mic_silence, mic_rc, held_retry, notices);
            }
        }
        TopologyEvent::SilenceStarted { track } => {
            // Mic silence drives recovery; system silence is a notice only.
            if track == Track::Mic {
                mic_silence.zero_run = true;
                // A dead-air bout degrading to zeros keeps its bout (and its
                // rate-limit slot) instead of starting a second one.
                if !mic_silence.silent {
                    mic_silence.start();
                }
                if deps.mic_silence == MicSilence::WaitForRelease && !mic_silence.warned {
                    mic_silence.warned = true;
                    log::info!("supervisor: mic held by the OS — waiting for release, not rebuilding");
                    let _ = notices.send(Notice::MicHeldByCall);
                }
            }
        }
        TopologyEvent::SilenceEnded { track } => {
            if track == Track::Mic {
                mic_silence.zero_run = false;
                end_mic_bout(mic_silence, mic_rc, held_retry, notices);
            }
        }
        TopologyEvent::QuietStarted { track } => {
            // Quiet ≠ silent: the stream is alive, its level is just low.
            // Never recovery, never a dead-mic warning — a calm
            // informational notice only.
            // During an open dead-air/silence bout a "still recording"
            // reassurance would contradict the active warning.
            // A voice-processed capture zeroes inter-speech residue, so its
            // quiet bouts are the far end talking, not a fading mic.
            if track == Track::Mic
                && !mic_silence.silent
                && !deps.mic_voice_processed.load(Ordering::Relaxed)
            {
                let _ = notices.send(Notice::MicVeryQuiet);
            }
        }
        TopologyEvent::QuietEnded { track: _ } => {
            // Logged by the info echo above; no state to unwind.
        }
        TopologyEvent::StreamDied { track, reason } => {
            if deps.paused.load(Ordering::Relaxed) {
                return;
            }
            match track {
                Track::Mic => {
                    // While the held-mic retry loop owns recovery, a death
                    // storm must not burn the (spent) budget into a
                    // ReconnectFailed that contradicts the retrying notice.
                    if held_retry.is_none() {
                        recover_mic(deps, notices, mic_rc, mic_silence, held_retry, &format!("{reason:?}"));
                    }
                }
                Track::System => {
                    if !sys_rc.try_attempt(Track::System, &format!("{reason:?}"), notices) {
                        return;
                    }
                    std::thread::sleep(RECONNECT_BACKOFF);
                    let ok = (deps.reopen_system)();
                    log::warn!(
                        "supervisor: System died ({reason:?}) — reconnect attempt {} {}",
                        sys_rc.attempts,
                        if ok { "succeeded" } else { "failed" }
                    );
                }
            }
        }
        TopologyEvent::SinkInputAdded { stream_id } => {
            (deps.route_stream)(stream_id);
        }
        TopologyEvent::VolumeChanged { key, volume, muted } => {
            let active = deps.mic.lock().unwrap().key.clone();
            if key != active {
                return;
            }
            // An unresolved id (device just hotplugged) can't be checked
            // against SelfWrites — treat the change as external.
            let id = deps.snapshot.read().unwrap().id_of(&key);
            let now_ms = deps.gain_epoch.elapsed().as_millis() as u64;
            if let Some(id) = id {
                if deps.self_writes.lock().unwrap().matches(id, volume, now_ms) {
                    // Our own write: AutoGain already matches this volume;
                    // external deltas measure from here.
                    *vol_anchor = Some((key, volume));
                    return;
                }
            }
            if muted {
                log::warn!("supervisor: active mic muted outside Daisy");
                let _ = notices.send(Notice::MicMutedExternally);
            } else {
                if let Some((k, v)) = vol_anchor {
                    if *k == key && (volume - *v).abs() < REBASELINE_DEADBAND {
                        log::debug!(
                            "supervisor: external volume change on active mic ({volume:.2}) within deadband of baseline ({:.2}) — ignored",
                            v
                        );
                        return;
                    }
                }
                *vol_anchor = Some((key, volume));
                log::info!(
                    "supervisor: external volume change on active mic ({volume:.2}) — re-baselining"
                );
                (deps.rebaseline)();
            }
        }
        TopologyEvent::SystemWoke { gap } => {
            log::warn!("supervisor: system woke after a {:.0}s gap — restarting streams", gap.as_secs_f32());
            mic_rc.reset();
            sys_rc.reset();
            // Fresh world after a suspend: the post-wake restart below is the
            // recovery; a still-held mic re-enters via a new silence bout.
            *held_retry = None;
            // A real suspend is not device flap; the next silence bout may
            // recover immediately.
            mic_silence.last_recovering = None;
            *needs_refresh = true;
            if !deps.paused.load(Ordering::Relaxed) {
                let active = deps.mic.lock().unwrap().key.clone();
                let sources = (deps.enumerate)();
                if let Some(t) = resolve_mic_target(deps, &active, &sources, true) {
                    let ok = switch_to(deps, notices, &t, &active);
                    log::info!("supervisor: post-wake mic restart {}", if ok { "ok" } else { "failed" });
                }
                let ok = (deps.reopen_system)();
                log::info!("supervisor: post-wake system reopen {}", if ok { "ok" } else { "failed" });
            }
        }
        TopologyEvent::ProfileChanged { .. } => {
            // A profile change renumbers nodes; the churn refresh re-resolves.
            *needs_refresh = true;
        }
    }
}

#[cfg(test)]
pub(crate) mod test_harness {
    use super::*;
    use audio_engine::source::SourceKind;

    pub struct TestSupervisor {
        pub handle: SupervisorHandle,
        pub inject_tx: Sender<TopologyEvent>,
        pub snapshot: Arc<RwLock<Snapshot>>,
        pub switch_calls: Arc<Mutex<Vec<u32>>>,
        pub reopen_calls: Arc<Mutex<u32>>,
        pub reroute_calls: Arc<Mutex<u32>>,
        pub routed_streams: Arc<Mutex<Vec<u32>>>,
        pub paused: Arc<AtomicBool>,
        pub aec_live: Arc<AtomicBool>,
        pub rebaselines: Arc<Mutex<u32>>,
        pub mic: Arc<Mutex<MicIdentity>>,
        pub mic_voice_processed: Arc<AtomicBool>,
    }

    impl TestSupervisor {
        /// Drops the injector before joining, so the run loop exits on
        /// `Disconnected` instead of waiting out its current tick.
        pub fn stop(self) {
            let TestSupervisor {
                mut handle,
                inject_tx,
                ..
            } = self;
            drop(inject_tx);
            handle.stop();
        }
    }

    pub fn src_mic(id: u32, node: &str) -> Source {
        Source {
            id,
            key: DeviceKey(node.to_string()),
            node_name: node.to_string(),
            description: format!("desc {node}"),
            kind: SourceKind::Mic,
            default_sample_rate: 16_000,
            default_channels: 1,
            is_virtual: false,
        }
    }

    pub fn src_virtual_mic(id: u32, node: &str) -> Source {
        Source {
            is_virtual: true,
            ..src_mic(id, node)
        }
    }

    pub fn start(seed: Vec<Source>, active_mic: DeviceKey) -> TestSupervisor {
        start_with(seed, active_mic, SystemShape::Passive)
    }

    /// Platform where a silent mic means the OS holds the device.
    pub fn start_wait_for_release(seed: Vec<Source>, active_mic: DeviceKey) -> TestSupervisor {
        start_full(seed, active_mic, SystemShape::Passive, false, MicSilence::WaitForRelease, None)
    }

    pub fn start_with(
        seed: Vec<Source>,
        active_mic: DeviceKey,
        shape: SystemShape,
    ) -> TestSupervisor {
        start_full(seed, active_mic, shape, false, MicSilence::Rebuild, None)
    }

    pub fn start_forced_aec(seed: Vec<Source>, active_mic: DeviceKey) -> TestSupervisor {
        start_full(seed, active_mic, SystemShape::Passive, true, MicSilence::Rebuild, None)
    }

    /// Supervisor whose failover ladder sees `default_input` as the OS
    /// default; `start` and friends report no default at all.
    pub fn start_with_default_input(
        seed: Vec<Source>,
        active_mic: DeviceKey,
        default_input: DeviceKey,
    ) -> TestSupervisor {
        start_full(
            seed,
            active_mic,
            SystemShape::Passive,
            false,
            MicSilence::Rebuild,
            Some(default_input),
        )
    }

    fn start_full(
        seed: Vec<Source>,
        active_mic: DeviceKey,
        shape: SystemShape,
        aec_forced: bool,
        mic_silence: MicSilence,
        default_input: Option<DeviceKey>,
    ) -> TestSupervisor {
        let (events_tx, events_rx) = std::sync::mpsc::channel();
        let snapshot = Arc::new(RwLock::new(Snapshot::default()));
        let switch_calls: Arc<Mutex<Vec<u32>>> = Arc::new(Mutex::new(Vec::new()));
        let calls = Arc::clone(&switch_calls);
        let reopen_calls: Arc<Mutex<u32>> = Arc::new(Mutex::new(0));
        let reopens = Arc::clone(&reopen_calls);
        let reroute_calls: Arc<Mutex<u32>> = Arc::new(Mutex::new(0));
        let reroutes = Arc::clone(&reroute_calls);
        let routed_streams: Arc<Mutex<Vec<u32>>> = Arc::new(Mutex::new(Vec::new()));
        let routed = Arc::clone(&routed_streams);
        let aec_live = Arc::new(AtomicBool::new(true));
        let paused = Arc::new(AtomicBool::new(false));
        let mic = Arc::new(Mutex::new(MicIdentity {
            id: seed
                .iter()
                .find(|s| s.key == active_mic)
                .map(|s| s.id)
                .unwrap_or(0),
            key: active_mic,
            prev_key: None,
        }));
        let self_writes: Arc<Mutex<SelfWrites>> = Arc::new(Mutex::new(SelfWrites::default()));
        let rebaselines: Arc<Mutex<u32>> = Arc::new(Mutex::new(0));
        let rebaselines_cl = Arc::clone(&rebaselines);
        let mic_voice_processed = Arc::new(AtomicBool::new(false));
        let deps = SupervisorDeps {
            events_rx,
            snapshot: Arc::clone(&snapshot),
            mic: Arc::clone(&mic),
            paused: Arc::clone(&paused),
            switch: Arc::new(move |s: &Source| {
                calls.lock().unwrap().push(s.id);
                true
            }),
            reopen_system: Arc::new(move || {
                *reopens.lock().unwrap() += 1;
                true
            }),
            reroute: Arc::new(move || {
                *reroutes.lock().unwrap() += 1;
            }),
            route_stream: Arc::new(move |id| {
                routed.lock().unwrap().push(id);
            }),
            enumerate: Arc::new(move || seed.clone()),
            default_input: Arc::new(move || default_input.clone()),
            aec_live: Arc::clone(&aec_live),
            aec_forced,
            needs_aec_sticky: Arc::new(AtomicBool::new(false)),
            system_shape: shape,
            self_writes: Arc::clone(&self_writes),
            mic_silence,
            gain_epoch: Instant::now(),
            rebaseline: Arc::new(move || {
                *rebaselines_cl.lock().unwrap() += 1;
            }),
            mic_voice_processed: Arc::clone(&mic_voice_processed),
        };
        let (notices_tx, notices_rx) = std::sync::mpsc::channel::<Notice>();
        TestSupervisor {
            handle: Supervisor::start(deps, notices_tx, notices_rx),
            inject_tx: events_tx,
            snapshot,
            switch_calls,
            reopen_calls,
            reroute_calls,
            routed_streams,
            paused,
            aec_live,
            rebaselines,
            mic,
            mic_voice_processed,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_harness::*;
    use super::*;
    use audio_engine::source::SourceKind;

    #[test]
    fn device_added_updates_snapshot() {
        let t = start(
            vec![src_mic(64, "builtin")],
            DeviceKey("builtin".into()),
        );
        t.inject_tx
            .send(TopologyEvent::DeviceAdded {
                key: DeviceKey("bluez_input.AC".into()),
                kind: SourceKind::Mic,
            })
            .unwrap();
        std::thread::sleep(Duration::from_millis(150));
        // Present immediately; the debounced re-enumeration refines ids later.
        assert!(t
            .snapshot
            .read()
            .unwrap()
            .contains(&DeviceKey("bluez_input.AC".into())));
        // Device churn is the UI hub subscriber's business, not a notice.
        assert!(t.handle.notices.try_recv().is_err());
        t.stop();
    }

    #[test]
    fn device_removed_drops_from_snapshot() {
        // Hotplug "bt" instead of seeding it: the harness's enumerate only
        // returns the seed, so the debounced refresh after removal must not
        // resurrect a device that is no longer enumerable.
        let t = start(vec![src_mic(64, "builtin")], DeviceKey("builtin".into()));
        t.inject_tx
            .send(TopologyEvent::DeviceAdded {
                key: DeviceKey("bt".into()),
                kind: SourceKind::Mic,
            })
            .unwrap();
        t.inject_tx
            .send(TopologyEvent::DeviceRemoved {
                key: DeviceKey("bt".into()),
            })
            .unwrap();
        std::thread::sleep(Duration::from_millis(150));
        assert!(!t.snapshot.read().unwrap().contains(&DeviceKey("bt".into())));
        t.stop();
    }

    #[test]
    fn active_mic_removal_fails_over() {
        let t = start(
            vec![src_mic(64, "builtin"), src_mic(144, "bt")],
            DeviceKey("bt".into()),
        );
        t.inject_tx
            .send(TopologyEvent::DeviceRemoved {
                key: DeviceKey("bt".into()),
            })
            .unwrap();
        let n = t.handle.notices.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(matches!(n, Notice::MicFailedOver { .. }), "got {n:?}");
        assert_eq!(*t.switch_calls.lock().unwrap(), vec![64]);
        // Identity updates as one unit: id, key, and prev_key together.
        let mic = t.mic.lock().unwrap();
        assert_eq!(mic.id, 64);
        assert_eq!(mic.key, DeviceKey("builtin".into()));
        assert_eq!(mic.prev_key, Some(DeviceKey("bt".into())));
        drop(mic);
        t.stop();
    }

    #[test]
    fn inactive_mic_removal_does_not_switch() {
        let t = start(
            vec![src_mic(64, "builtin"), src_mic(144, "bt")],
            DeviceKey("builtin".into()),
        );
        t.inject_tx
            .send(TopologyEvent::DeviceRemoved {
                key: DeviceKey("bt".into()),
            })
            .unwrap();
        std::thread::sleep(Duration::from_millis(150));
        assert!(t.switch_calls.lock().unwrap().is_empty());
        t.stop();
    }

    #[test]
    fn failover_target_prefers_any_other_mic_over_the_lost_one() {
        let sources = vec![src_mic(64, "builtin"), src_mic(144, "bt")];
        let t = failover_target(&sources, &DeviceKey("bt".into()), None, None).unwrap();
        assert_eq!(t.id, 64);
        assert!(
            failover_target(&[src_mic(144, "bt")], &DeviceKey("bt".into()), None, None).is_none()
        );
    }

    #[test]
    fn failover_prefers_the_last_working_mic_over_list_order() {
        // iphone is first in the list; the previously active builtin wins.
        let sources = vec![
            src_mic(10, "iphone"),
            src_mic(64, "builtin"),
            src_mic(144, "bt"),
        ];
        let prev = DeviceKey("builtin".into());
        let t = failover_target(&sources, &DeviceKey("bt".into()), Some(&prev), None).unwrap();
        assert_eq!(t.id, 64);
        // A prev that equals the lost device falls through.
        let t =
            failover_target(&sources, &DeviceKey("builtin".into()), Some(&prev), None).unwrap();
        assert_ne!(t.key.0, "builtin");
    }

    #[test]
    fn failover_prefers_the_os_default_over_list_order() {
        // iphone leads the list; the OS default is what a user expects next.
        let sources = vec![
            src_mic(10, "iphone"),
            src_mic(64, "builtin"),
            src_mic(144, "bt"),
        ];
        let dk = DeviceKey("builtin".into());
        let t = failover_target(&sources, &DeviceKey("bt".into()), None, Some(&dk)).unwrap();
        assert_eq!(t.id, 64);
        // The last working mic still outranks the OS default.
        let prev = DeviceKey("iphone".into());
        let t =
            failover_target(&sources, &DeviceKey("bt".into()), Some(&prev), Some(&dk)).unwrap();
        assert_eq!(t.id, 10);
        // A default that is the device we just lost falls through to the list.
        let lost = DeviceKey("bt".into());
        let t = failover_target(&sources, &DeviceKey("bt".into()), None, Some(&lost)).unwrap();
        assert_eq!(t.id, 10);
        // A virtual default is no more acceptable than a virtual list entry.
        let sources = vec![src_virtual_mic(98, "cupcake"), src_mic(78, "builtin")];
        let dk = DeviceKey("cupcake".into());
        let t = failover_target(&sources, &DeviceKey("bt".into()), None, Some(&dk)).unwrap();
        assert_eq!(t.id, 78);
    }

    #[test]
    fn active_mic_removal_fails_over_to_the_os_default() {
        // List order would land on iphone; the OS default names builtin.
        let t = start_with_default_input(
            vec![
                src_mic(10, "iphone"),
                src_mic(64, "builtin"),
                src_mic(144, "bt"),
            ],
            DeviceKey("bt".into()),
            DeviceKey("builtin".into()),
        );
        t.inject_tx
            .send(TopologyEvent::DeviceRemoved {
                key: DeviceKey("bt".into()),
            })
            .unwrap();
        let n = t.handle.notices.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(matches!(n, Notice::MicFailedOver { id: 64, .. }), "got {n:?}");
        assert_eq!(*t.switch_calls.lock().unwrap(), vec![64]);
        assert_eq!(t.mic.lock().unwrap().key, DeviceKey("builtin".into()));
        t.stop();
    }

    #[test]
    fn failover_target_never_picks_a_virtual_mic() {
        // Even as the only candidate, and even as the previously active
        // device, a virtual mic is not an automatic failover target.
        let sources = vec![src_mic(78, "builtin"), src_virtual_mic(98, "cupcake")];
        let t = failover_target(&sources, &DeviceKey("builtin".into()), None, None);
        assert!(t.is_none(), "failed over to a virtual mic: {t:?}");
        let prev = DeviceKey("cupcake".into());
        let t = failover_target(&sources, &DeviceKey("builtin".into()), Some(&prev), None);
        assert!(t.is_none(), "prev-key ladder picked a virtual mic: {t:?}");
        // A physical alternative is still found next to the virtual one.
        let sources = vec![
            src_virtual_mic(98, "cupcake"),
            src_mic(78, "builtin"),
            src_mic(144, "bt"),
        ];
        let t = failover_target(&sources, &DeviceKey("bt".into()), None, None).unwrap();
        assert_eq!(t.id, 78);
    }

    /// Matrix row (held device): zeros on the active mic and only a virtual
    /// alternative → every attempt rebuilds the held device, the slow retry
    /// loop keeps rebuilding it past the budget, and signal returning ends
    /// recovery with the recording still on the original mic.
    fn drain(t: &TestSupervisor) -> Vec<Notice> {
        let mut out = Vec::new();
        while let Ok(n) = t.handle.notices.try_recv() {
            out.push(n);
        }
        out
    }

    #[test]
    fn dead_air_starts_recovery_and_emits_notice() {
        let t = start(
            vec![src_mic(78, "builtin"), src_mic(11, "usb")],
            DeviceKey("builtin".into()),
        );
        t.inject_tx
            .send(TopologyEvent::DeadAirStarted { track: Track::Mic })
            .unwrap();
        std::thread::sleep(Duration::from_millis(400));
        let notices = drain(&t);
        assert_eq!(
            notices.iter().filter(|n| **n == Notice::MicDeadAir).count(),
            1,
            "got {notices:?}"
        );
        assert!(
            !t.switch_calls.lock().unwrap().is_empty(),
            "dead air must drive the recovery ladder"
        );
        t.stop();
    }

    #[test]
    fn dead_air_ended_clears_bout_and_emits_recovered() {
        // Single physical mic: the ladder ends in the held-retry loop.
        let t = start(vec![src_mic(78, "builtin")], DeviceKey("builtin".into()));
        t.inject_tx
            .send(TopologyEvent::DeadAirStarted { track: Track::Mic })
            .unwrap();
        std::thread::sleep(Duration::from_millis(800));
        assert!(drain(&t).contains(&Notice::MicDeadAir));
        t.inject_tx
            .send(TopologyEvent::DeadAirEnded { track: Track::Mic })
            .unwrap();
        std::thread::sleep(Duration::from_millis(150));
        let notices = drain(&t);
        assert!(notices.contains(&Notice::MicRecovered), "got {notices:?}");
        // Held loop stopped: no further rebuilds.
        let settled = t.switch_calls.lock().unwrap().len();
        std::thread::sleep(HELD_RETRY_GAP + Duration::from_millis(200));
        assert_eq!(t.switch_calls.lock().unwrap().len(), settled);
        t.stop();
    }

    #[test]
    fn dead_air_ended_keeps_bout_when_silence_took_over() {
        let t = start(vec![src_mic(78, "builtin")], DeviceKey("builtin".into()));
        t.inject_tx
            .send(TopologyEvent::DeadAirStarted { track: Track::Mic })
            .unwrap();
        std::thread::sleep(Duration::from_millis(800));
        // Stream degrades to pure zeros: the zero-run class takes over.
        t.inject_tx
            .send(TopologyEvent::SilenceStarted { track: Track::Mic })
            .unwrap();
        t.inject_tx
            .send(TopologyEvent::DeadAirEnded { track: Track::Mic })
            .unwrap();
        std::thread::sleep(Duration::from_millis(150));
        let notices = drain(&t);
        assert!(
            !notices.contains(&Notice::MicRecovered),
            "silence owns the bout; DeadAirEnded must not fake recovery: {notices:?}"
        );
        // Held loop keeps rebuilding under the silence bout.
        let before = t.switch_calls.lock().unwrap().len();
        std::thread::sleep(HELD_RETRY_GAP + Duration::from_millis(300));
        assert!(t.switch_calls.lock().unwrap().len() > before, "held retry stopped");
        // Real audio ends it.
        t.inject_tx
            .send(TopologyEvent::SilenceEnded { track: Track::Mic })
            .unwrap();
        std::thread::sleep(Duration::from_millis(150));
        assert!(drain(&t).contains(&Notice::MicRecovered));
        t.stop();
    }

    #[test]
    fn dead_air_within_rate_limit_gap_does_not_recover() {
        let t = start(vec![src_mic(78, "builtin")], DeviceKey("builtin".into()));
        t.inject_tx
            .send(TopologyEvent::DeadAirStarted { track: Track::Mic })
            .unwrap();
        std::thread::sleep(Duration::from_millis(800));
        t.inject_tx
            .send(TopologyEvent::DeadAirEnded { track: Track::Mic })
            .unwrap();
        std::thread::sleep(Duration::from_millis(150));
        drain(&t);
        // Second bout inside SILENCE_BOUT_MIN_GAP: classified, no rebuild.
        t.inject_tx
            .send(TopologyEvent::DeadAirStarted { track: Track::Mic })
            .unwrap();
        std::thread::sleep(Duration::from_millis(150));
        let settled = t.switch_calls.lock().unwrap().len();
        std::thread::sleep(Duration::from_millis(500));
        assert_eq!(
            t.switch_calls.lock().unwrap().len(),
            settled,
            "rate-limited bout must not rebuild"
        );
        t.stop();
    }

    #[test]
    fn held_mic_waits_for_release_where_silence_means_the_os_holds_it() {
        // macOS: a FaceTime/Phone call silences the mic. Rebuild churn
        // against the seized device wedges the process's CoreAudio client
        // until relaunch, so the platform policy is to touch nothing and
        // let the OS hand the device back.
        let t = start_wait_for_release(
            vec![src_mic(78, "builtin"), src_mic(11, "usb")],
            DeviceKey("builtin".into()),
        );
        t.inject_tx
            .send(TopologyEvent::SilenceStarted { track: Track::Mic })
            .unwrap();
        std::thread::sleep(HELD_RETRY_GAP + Duration::from_millis(400));
        assert!(
            t.switch_calls.lock().unwrap().is_empty(),
            "a held mic must not be rebuilt or failed over: {:?}",
            t.switch_calls.lock().unwrap()
        );
        let notices = drain(&t);
        assert_eq!(
            notices.iter().filter(|n| **n == Notice::MicHeldByCall).count(),
            1,
            "expected exactly one held-by-call notice, got {notices:?}"
        );
        assert!(
            !notices.contains(&Notice::MicRetrying),
            "retrying is the wrong story on this platform: {notices:?}"
        );
        // The call ends: the OS returns the device on its own.
        t.inject_tx
            .send(TopologyEvent::SilenceEnded { track: Track::Mic })
            .unwrap();
        std::thread::sleep(Duration::from_millis(200));
        assert!(drain(&t).contains(&Notice::MicRecovered));
        assert!(t.switch_calls.lock().unwrap().is_empty(), "still no rebuilds");
        t.stop();
    }

    #[test]
    fn dead_air_still_recovers_where_silence_waits() {
        // The wait-it-out policy covers the zero-run class only: sub-floor
        // dither is a degraded input, not a call, and still earns a rebuild.
        let t = start_wait_for_release(
            vec![src_mic(78, "builtin"), src_mic(11, "usb")],
            DeviceKey("builtin".into()),
        );
        t.inject_tx
            .send(TopologyEvent::DeadAirStarted { track: Track::Mic })
            .unwrap();
        std::thread::sleep(Duration::from_millis(400));
        assert!(
            !t.switch_calls.lock().unwrap().is_empty(),
            "dead air must still drive the recovery ladder"
        );
        t.stop();
    }

    #[test]
    fn silent_mic_with_only_virtual_alternative_stays_and_retries() {
        let t = start(
            vec![src_mic(78, "builtin"), src_virtual_mic(98, "cupcake")],
            DeviceKey("builtin".into()),
        );
        t.inject_tx
            .send(TopologyEvent::SilenceStarted { track: Track::Mic })
            .unwrap();
        // Budgeted ladder runs its 3 attempts (~600 ms at test cadence).
        std::thread::sleep(Duration::from_millis(800));
        let calls = t.switch_calls.lock().unwrap().clone();
        assert_eq!(calls, vec![78, 78, 78], "must never switch to the virtual mic");
        let mut notices = Vec::new();
        while let Ok(n) = t.handle.notices.try_recv() {
            notices.push(n);
        }
        assert!(notices.contains(&Notice::MicRetrying), "got {notices:?}");
        assert!(
            !notices.iter().any(|n| matches!(n, Notice::MicFailedOver { .. })),
            "held case must not announce a failover: {notices:?}"
        );
        assert!(
            !notices.contains(&Notice::ReconnectFailed { track: Track::Mic }),
            "held case must not give up: {notices:?}"
        );
        // The slow loop rebuilds the held device past the spent budget.
        std::thread::sleep(HELD_RETRY_GAP + Duration::from_millis(300));
        let calls = t.switch_calls.lock().unwrap().clone();
        assert!(calls.len() > 3, "held retry never ran: {calls:?}");
        assert!(calls.iter().all(|&c| c == 78), "retry left the held device: {calls:?}");
        // Device heals (the OS call ended): signal back, retrying stops,
        // recording is on the original mic.
        t.inject_tx
            .send(TopologyEvent::SilenceEnded { track: Track::Mic })
            .unwrap();
        std::thread::sleep(Duration::from_millis(150));
        let settled = t.switch_calls.lock().unwrap().len();
        std::thread::sleep(HELD_RETRY_GAP + Duration::from_millis(200));
        assert_eq!(
            t.switch_calls.lock().unwrap().len(),
            settled,
            "held retry kept running after signal returned"
        );
        assert_eq!(t.mic.lock().unwrap().id, 78);
        t.stop();
    }

    /// Matrix row: losing the active mic with only a virtual device left
    /// reports the failure instead of recording zeros from the virtual one.
    #[test]
    fn active_mic_removal_with_only_virtual_alternative_fails() {
        let t = start(
            vec![src_mic(144, "bt"), src_virtual_mic(9, "cable")],
            DeviceKey("bt".into()),
        );
        t.inject_tx
            .send(TopologyEvent::DeviceRemoved {
                key: DeviceKey("bt".into()),
            })
            .unwrap();
        let n = t.handle.notices.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(n, Notice::ReconnectFailed { track: Track::Mic });
        assert!(t.switch_calls.lock().unwrap().is_empty());
        t.stop();
    }

    #[test]
    fn wake_gap_synthesizes_only_beyond_threshold() {
        assert_eq!(
            wake_gap(Duration::from_secs(70), Duration::from_secs(10)),
            Duration::from_secs(60)
        );
        assert_eq!(
            wake_gap(Duration::from_secs(10), Duration::from_secs(10)),
            Duration::ZERO
        );
        assert!(wake_gap(Duration::from_secs(10), Duration::from_secs(70)).is_zero());
    }

    #[test]
    fn mic_death_reconnects_same_key_bounded() {
        let t = start(
            vec![src_mic(64, "builtin"), src_mic(144, "bt")],
            DeviceKey("bt".into()),
        );
        for _ in 0..5 {
            t.inject_tx
                .send(TopologyEvent::StreamDied {
                    track: Track::Mic,
                    reason: audio_engine::topology::StreamDeathReason::TargetGone,
                })
                .unwrap();
        }
        // Give the loop time to process (each attempt sleeps 50 ms backoff).
        std::thread::sleep(Duration::from_millis(600));
        let calls = t.switch_calls.lock().unwrap().clone();
        assert!(calls.len() <= 3, "expected bounded attempts, got {calls:?}");
        // Ladder: same-key rebuilds first, a different mic on the last attempt.
        assert!(calls.starts_with(&[144]), "first recovery must rebuild the active key, got {calls:?}");
        if calls.len() == 3 {
            assert_eq!(calls[2], 64, "last attempt must fail over, got {calls:?}");
        }
        let mut notices = Vec::new();
        while let Ok(n) = t.handle.notices.try_recv() {
            notices.push(n);
        }
        assert!(notices.contains(&Notice::ReconnectFailed { track: Track::Mic }));
        t.stop();
    }

    #[test]
    fn system_death_calls_reopen() {
        let t = start(vec![src_mic(64, "builtin")], DeviceKey("builtin".into()));
        t.inject_tx
            .send(TopologyEvent::StreamDied {
                track: Track::System,
                reason: audio_engine::topology::StreamDeathReason::NoFrames,
            })
            .unwrap();
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(*t.reopen_calls.lock().unwrap(), 1);
        t.stop();
    }

    #[test]
    fn output_change_to_headphones_disengages_live_aec() {
        use audio_engine::routing::OutputClass;
        let t = start(vec![src_mic(64, "builtin")], DeviceKey("builtin".into()));
        t.inject_tx
            .send(TopologyEvent::DefaultOutputChanged {
                key: Some(DeviceKey("uid-airpods".into())),
                class: OutputClass::Headphone,
            })
            .unwrap();
        std::thread::sleep(Duration::from_millis(150));
        assert!(!t.aec_live.load(Ordering::Relaxed));
        t.inject_tx
            .send(TopologyEvent::DefaultOutputChanged {
                key: Some(DeviceKey("uid-speakers".into())),
                class: OutputClass::Speaker,
            })
            .unwrap();
        std::thread::sleep(Duration::from_millis(150));
        assert!(t.aec_live.load(Ordering::Relaxed));
        t.stop();
    }

    #[test]
    fn forced_aec_ignores_output_changes() {
        use audio_engine::routing::OutputClass;
        let t = start_forced_aec(vec![src_mic(64, "builtin")], DeviceKey("builtin".into()));
        // aec_live starts true in the harness; a headphone default must NOT
        // disengage it when the decision is user-pinned.
        t.inject_tx
            .send(TopologyEvent::DefaultOutputChanged {
                key: Some(DeviceKey("uid-airpods".into())),
                class: OutputClass::Headphone,
            })
            .unwrap();
        std::thread::sleep(Duration::from_millis(150));
        assert!(t.aec_live.load(Ordering::Relaxed));
        t.stop();
    }

    #[test]
    fn output_change_reopens_system_on_windows_shape() {
        use audio_engine::routing::OutputClass;
        let t = start_with(
            vec![src_mic(64, "builtin")],
            DeviceKey("builtin".into()),
            SystemShape::ReopenOnDefaultChange,
        );
        // A burst of default-output churn coalesces into ONE trailing reopen
        // and ONE trailing sink reroute.
        for _ in 0..4 {
            t.inject_tx
                .send(TopologyEvent::DefaultOutputChanged {
                    key: None,
                    class: OutputClass::Unknown,
                })
                .unwrap();
        }
        std::thread::sleep(Duration::from_millis(600));
        assert_eq!(*t.reopen_calls.lock().unwrap(), 1);
        assert_eq!(*t.reroute_calls.lock().unwrap(), 1);
        t.stop();
    }

    #[test]
    fn new_playback_stream_routes_into_the_sink() {
        let t = start(vec![src_mic(64, "builtin")], DeviceKey("builtin".into()));
        t.inject_tx
            .send(TopologyEvent::SinkInputAdded { stream_id: 77 })
            .unwrap();
        std::thread::sleep(Duration::from_millis(150));
        assert_eq!(*t.routed_streams.lock().unwrap(), vec![77]);
        t.stop();
    }

    #[test]
    fn self_inflicted_volume_change_is_suppressed() {
        let mut s = SelfWrites::default();
        s.mark(64, 0.80, 1_000);
        assert!(s.matches(64, 0.80, 1_500));
        // Hardware quantizes writes to its own volume-step grid (field case:
        // 0.80 read back as 0.8416667); the echo must still count as ours.
        assert!(s.matches(64, 0.841_666_7, 1_500));
        assert!(!s.matches(64, 0.60, 1_500));
        assert!(!s.matches(64, 0.30, 1_600));
        assert!(!s.matches(64, 0.80, 4_000));
        assert!(!s.matches(65, 0.80, 1_500));
    }

    #[test]
    fn external_mute_notice_and_volume_rebaseline() {
        let t = start(vec![src_mic(64, "builtin")], DeviceKey("builtin".into()));
        t.inject_tx
            .send(TopologyEvent::VolumeChanged {
                key: DeviceKey("builtin".into()),
                volume: 0.0,
                muted: true,
            })
            .unwrap();
        assert_eq!(
            t.handle.notices.recv_timeout(Duration::from_secs(2)).unwrap(),
            Notice::MicMutedExternally
        );
        t.inject_tx
            .send(TopologyEvent::VolumeChanged {
                key: DeviceKey("builtin".into()),
                volume: 0.3,
                muted: false,
            })
            .unwrap();
        std::thread::sleep(Duration::from_millis(150));
        assert_eq!(*t.rebaselines.lock().unwrap(), 1);
        // A different device's change is ignored.
        t.inject_tx
            .send(TopologyEvent::VolumeChanged {
                key: DeviceKey("other".into()),
                volume: 0.1,
                muted: true,
            })
            .unwrap();
        std::thread::sleep(Duration::from_millis(150));
        assert!(t.handle.notices.try_recv().is_err());
        t.stop();
    }

    #[test]
    fn volume_flap_within_deadband_rebaselines_once() {
        let t = start(vec![src_mic(64, "builtin")], DeviceKey("builtin".into()));
        let volume_change = |volume: f32| {
            t.inject_tx
                .send(TopologyEvent::VolumeChanged {
                    key: DeviceKey("builtin".into()),
                    volume,
                    muted: false,
                })
                .unwrap();
        };
        // Call-app AGC flap (Teams: 0.75↔0.79): one baseline, then quiet.
        volume_change(0.75);
        volume_change(0.79);
        volume_change(0.75);
        volume_change(0.79);
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(*t.rebaselines.lock().unwrap(), 1);
        // A real change measured from the 0.75 anchor re-baselines.
        volume_change(0.60);
        std::thread::sleep(Duration::from_millis(150));
        assert_eq!(*t.rebaselines.lock().unwrap(), 2);
        t.stop();
    }

    #[test]
    fn silent_mic_retries_then_fails_over_then_gives_up() {
        let t = start(
            vec![src_mic(64, "builtin"), src_mic(144, "bt")],
            DeviceKey("bt".into()),
        );
        t.inject_tx
            .send(TopologyEvent::SilenceStarted { track: Track::Mic })
            .unwrap();
        // Tick cadence is 100 ms; three attempts need ~600 ms of quiet loop.
        std::thread::sleep(Duration::from_millis(800));
        let calls = t.switch_calls.lock().unwrap().clone();
        assert_eq!(calls.len(), 3, "expected exactly 3 recovery switches, got {calls:?}");
        // Attempts 1-2 rebuild the silent mic; the last fails over.
        assert_eq!(calls[0], 144);
        assert_eq!(calls[1], 144);
        assert_eq!(calls[2], 64);
        let mut notices = Vec::new();
        while let Ok(n) = t.handle.notices.try_recv() {
            notices.push(n);
        }
        assert!(notices.iter().any(|n| matches!(n, Notice::MicFailedOver { .. })));
        t.stop();
    }

    #[test]
    fn silence_ended_stops_recovery_and_resets_budget() {
        let t = start(
            vec![src_mic(64, "builtin"), src_mic(144, "bt")],
            DeviceKey("bt".into()),
        );
        t.inject_tx
            .send(TopologyEvent::SilenceStarted { track: Track::Mic })
            .unwrap();
        std::thread::sleep(Duration::from_millis(200));
        t.inject_tx
            .send(TopologyEvent::SilenceEnded { track: Track::Mic })
            .unwrap();
        std::thread::sleep(Duration::from_millis(200));
        let calls_after_end = t.switch_calls.lock().unwrap().len();
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(
            t.switch_calls.lock().unwrap().len(),
            calls_after_end,
            "recovery kept running after SilenceEnded"
        );
        t.stop();
    }

    /// Matrix extension (Quiet state): a quiet-but-alive mic raises the calm
    /// notice and NOTHING else — no rebuild ladder, no failover. Zero-run
    /// silence (the row above) keeps driving recovery.
    #[test]
    fn quiet_mic_notices_but_never_recovers() {
        let t = start(
            vec![src_mic(64, "builtin"), src_mic(144, "bt")],
            DeviceKey("bt".into()),
        );
        t.inject_tx
            .send(TopologyEvent::QuietStarted { track: Track::Mic })
            .unwrap();
        let n = t.handle.notices.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(n, Notice::MicVeryQuiet);
        // Several ticks pass; a silence bout would have burned attempts here.
        std::thread::sleep(Duration::from_millis(500));
        assert!(
            t.switch_calls.lock().unwrap().is_empty(),
            "quiet mic triggered recovery"
        );
        t.inject_tx
            .send(TopologyEvent::QuietEnded { track: Track::Mic })
            .unwrap();
        // System-track quiet is not a mic notice.
        t.inject_tx
            .send(TopologyEvent::QuietStarted { track: Track::System })
            .unwrap();
        std::thread::sleep(Duration::from_millis(200));
        assert!(t.handle.notices.try_recv().is_err());
        assert!(t.switch_calls.lock().unwrap().is_empty());
        t.stop();
    }

    /// A voice-processed capture (VPIO / comms mode) zeroes inter-speech
    /// residue, so its quiet bouts mean the far end is talking — no notice.
    #[test]
    fn voice_processed_quiet_mic_stays_silent() {
        let t = start(
            vec![src_mic(64, "builtin"), src_mic(144, "bt")],
            DeviceKey("bt".into()),
        );
        t.mic_voice_processed.store(true, Ordering::Relaxed);
        t.inject_tx
            .send(TopologyEvent::QuietStarted { track: Track::Mic })
            .unwrap();
        std::thread::sleep(Duration::from_millis(300));
        assert!(t.handle.notices.try_recv().is_err(), "voice-processed quiet toasted");
        assert!(t.switch_calls.lock().unwrap().is_empty());
        t.stop();
    }

    /// A second zero-run bout right after the first must not earn a fresh
    /// rebuild ladder; once SILENCE_BOUT_MIN_GAP passes, recovery resumes.
    #[test]
    fn back_to_back_silence_bouts_recover_once_per_gap() {
        let t = start(
            vec![src_mic(64, "builtin"), src_mic(144, "bt")],
            DeviceKey("bt".into()),
        );
        t.inject_tx
            .send(TopologyEvent::SilenceStarted { track: Track::Mic })
            .unwrap();
        std::thread::sleep(Duration::from_millis(800));
        let first_bout = t.switch_calls.lock().unwrap().len();
        assert!(first_bout >= 1, "first bout must recover");
        t.inject_tx
            .send(TopologyEvent::SilenceEnded { track: Track::Mic })
            .unwrap();
        std::thread::sleep(Duration::from_millis(100));
        // Second bout inside the gap: classified, but no rebuilds.
        t.inject_tx
            .send(TopologyEvent::SilenceStarted { track: Track::Mic })
            .unwrap();
        std::thread::sleep(Duration::from_millis(400));
        assert_eq!(
            t.switch_calls.lock().unwrap().len(),
            first_bout,
            "throttled bout still recovered"
        );
        t.inject_tx
            .send(TopologyEvent::SilenceEnded { track: Track::Mic })
            .unwrap();
        // Past the gap: a new bout recovers again.
        std::thread::sleep(SILENCE_BOUT_MIN_GAP);
        t.inject_tx
            .send(TopologyEvent::SilenceStarted { track: Track::Mic })
            .unwrap();
        std::thread::sleep(Duration::from_millis(400));
        assert!(
            t.switch_calls.lock().unwrap().len() > first_bout,
            "recovery never resumed after the gap"
        );
        t.stop();
    }

    #[test]
    fn system_silence_never_triggers_reopen() {
        let t = start(vec![src_mic(64, "builtin")], DeviceKey("builtin".into()));
        t.inject_tx
            .send(TopologyEvent::SilenceStarted { track: Track::System })
            .unwrap();
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(*t.reopen_calls.lock().unwrap(), 0);
        t.stop();
    }

    /// Matrix rows 5, 16, 19-while-paused: NOTHING recovers while paused —
    /// not device loss, not stream death, not silence.
    #[test]
    fn paused_storm_never_switches_or_reopens() {
        let t = start(
            vec![src_mic(64, "builtin"), src_mic(144, "bt")],
            DeviceKey("bt".into()),
        );
        t.paused.store(true, Ordering::Relaxed);
        std::thread::sleep(Duration::from_millis(100));
        for _ in 0..3 {
            t.inject_tx
                .send(TopologyEvent::StreamDied {
                    track: Track::Mic,
                    reason: audio_engine::topology::StreamDeathReason::TargetGone,
                })
                .unwrap();
            t.inject_tx
                .send(TopologyEvent::StreamDied {
                    track: Track::System,
                    reason: audio_engine::topology::StreamDeathReason::NoFrames,
                })
                .unwrap();
        }
        t.inject_tx
            .send(TopologyEvent::SilenceStarted { track: Track::Mic })
            .unwrap();
        t.inject_tx
            .send(TopologyEvent::DeviceRemoved { key: DeviceKey("bt".into()) })
            .unwrap();
        // Silent-mic retries are tick-driven; give several ticks a chance.
        std::thread::sleep(Duration::from_millis(500));
        assert!(t.switch_calls.lock().unwrap().is_empty(), "switched while paused");
        assert_eq!(*t.reopen_calls.lock().unwrap(), 0, "reopened while paused");
        t.stop();
    }

    /// Matrix rows 15 → 24 → 14: a spent budget blocks further attempts
    /// until SystemWoke resets it; then recovery runs again.
    #[test]
    fn wake_resets_a_spent_budget() {
        let t = start(
            vec![src_mic(64, "builtin"), src_mic(144, "bt")],
            DeviceKey("bt".into()),
        );
        for _ in 0..5 {
            t.inject_tx
                .send(TopologyEvent::StreamDied {
                    track: Track::Mic,
                    reason: audio_engine::topology::StreamDeathReason::TargetGone,
                })
                .unwrap();
        }
        std::thread::sleep(Duration::from_millis(400));
        let spent = t.switch_calls.lock().unwrap().len();
        assert!(spent <= 3, "budget exceeded: {spent}");
        // Budget is spent: another death must not add attempts.
        t.inject_tx
            .send(TopologyEvent::StreamDied {
                track: Track::Mic,
                reason: audio_engine::topology::StreamDeathReason::TargetGone,
            })
            .unwrap();
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(t.switch_calls.lock().unwrap().len(), spent, "gave-up budget leaked an attempt");
        // Wake resets both budgets and restarts streams.
        t.inject_tx
            .send(TopologyEvent::SystemWoke { gap: Duration::from_secs(60) })
            .unwrap();
        std::thread::sleep(Duration::from_millis(200));
        let after_wake = t.switch_calls.lock().unwrap().len();
        assert!(after_wake > spent, "wake did not restart the mic");
        assert!(*t.reopen_calls.lock().unwrap() >= 1, "wake did not reopen the system");
        // And the budget is fresh again: a new death earns a new attempt.
        t.inject_tx
            .send(TopologyEvent::StreamDied {
                track: Track::Mic,
                reason: audio_engine::topology::StreamDeathReason::TargetGone,
            })
            .unwrap();
        std::thread::sleep(Duration::from_millis(250));
        assert!(
            t.switch_calls.lock().unwrap().len() > after_wake,
            "budget not reset by wake"
        );
        t.stop();
    }

    /// Seeded event storm (LCG, no external deps): the supervisor must
    /// survive an arbitrary interleaving without panicking, never exceed
    /// the recovery budget between resets, and still respond afterwards.
    #[test]
    fn seeded_event_storm_stays_sane_and_responsive() {
        let t = start(
            vec![src_mic(64, "builtin"), src_mic(144, "bt"), src_mic(9, "usb")],
            DeviceKey("bt".into()),
        );
        let mut rng: u64 = 0xDA151E5;
        let mut next = || {
            rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (rng >> 33) as u32
        };
        use audio_engine::routing::OutputClass;
        use audio_engine::source::SourceKind;
        for _ in 0..60 {
            let ev = match next() % 10 {
                0 => TopologyEvent::DeviceAdded {
                    key: DeviceKey(format!("hot-{}", next() % 3)),
                    kind: SourceKind::Mic,
                },
                1 => TopologyEvent::DeviceRemoved { key: DeviceKey(format!("hot-{}", next() % 3)) },
                2 => TopologyEvent::DefaultOutputChanged {
                    key: None,
                    class: if next() % 2 == 0 { OutputClass::Speaker } else { OutputClass::Headphone },
                },
                3 => TopologyEvent::DefaultInputChanged { key: Some(DeviceKey("usb".into())) },
                4 => TopologyEvent::ProfileChanged { card: "bluez_card.AC".into() },
                5 => TopologyEvent::SilenceStarted { track: Track::System },
                6 => TopologyEvent::SilenceEnded { track: Track::System },
                7 => TopologyEvent::SinkInputAdded { stream_id: next() % 100 },
                8 => TopologyEvent::VolumeChanged {
                    key: DeviceKey("bt".into()),
                    volume: (next() % 100) as f32 / 100.0,
                    muted: next() % 4 == 0,
                },
                _ => TopologyEvent::StreamDied {
                    track: if next() % 2 == 0 { Track::Mic } else { Track::System },
                    reason: audio_engine::topology::StreamDeathReason::NoFrames,
                },
            };
            t.inject_tx.send(ev).unwrap();
            if next() % 5 == 0 {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        // Storm had no budget resets (no mic SilenceEnded, no wake): the
        // total attempt count across mic + system stays within one budget
        // each, no matter the interleaving.
        std::thread::sleep(Duration::from_millis(600));
        let switches = t.switch_calls.lock().unwrap().len();
        let reopens = *t.reopen_calls.lock().unwrap();
        assert!(switches <= 3, "mic budget exceeded in storm: {switches}");
        // Reopens: ≤ 3 death-driven + trailing-edge output reroutes never
        // reopen on Passive shape.
        assert!(reopens <= 3, "system budget exceeded in storm: {reopens}");
        // Still alive and responsive after the storm.
        t.inject_tx
            .send(TopologyEvent::SinkInputAdded { stream_id: 777 })
            .unwrap();
        std::thread::sleep(Duration::from_millis(200));
        assert!(
            t.routed_streams.lock().unwrap().contains(&777),
            "supervisor unresponsive after storm"
        );
        t.stop();
    }

    #[test]
    fn silence_transitions_raise_no_notices() {
        let t = start(vec![src_mic(64, "builtin")], DeviceKey("builtin".into()));
        t.inject_tx
            .send(TopologyEvent::SilenceStarted { track: Track::System })
            .unwrap();
        t.inject_tx
            .send(TopologyEvent::SilenceEnded { track: Track::System })
            .unwrap();
        std::thread::sleep(Duration::from_millis(150));
        assert!(t.handle.notices.try_recv().is_err());
        t.stop();
    }
}
