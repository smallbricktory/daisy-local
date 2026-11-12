// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Turns "another app holds the microphone" into one prompt per episode, and
//! "the app that held it during a recording let go" into one stop prompt.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use audio_engine::mic_clients::{mic_clients, mic_clients_support, MicClientsSupport};
use tauri::Manager;

use crate::settings::MeetingPrompt;

/// How long the microphone must stay taken before the prompt fires.
pub const DEBOUNCE: Duration = Duration::from_secs(5);

/// How often the monitor thread asks.
pub const PERIOD: Duration = Duration::from_secs(2);

/// How long another app must hold the microphone during a recording before
/// its release can end the meeting.
pub const ARM_AFTER: Duration = Duration::from_secs(5);

/// How long every armed app must be off the microphone, with the system track
/// quiet, before the stop prompt fires.
pub const DEBOUNCE_END: Duration = Duration::from_secs(5);

// Shortest gap between two prompts.
const MIN_GAP: Duration = Duration::from_secs(300);

#[derive(Debug, PartialEq, Eq)]
enum State {
    Idle,
    SeenSince(Instant),
    Busy,
}

/// Fires once per episode of the microphone being taken by another app, and
/// no more than once every [`MIN_GAP`].
#[derive(Debug)]
pub struct Detector {
    state: State,
    last_fired: Option<Instant>,
}

impl Default for Detector {
    fn default() -> Self {
        Self::new()
    }
}

impl Detector {
    pub fn new() -> Self {
        Self { state: State::Idle, last_fired: None }
    }

    /// Advances the machine. `true` means the prompt should be shown now.
    pub fn step(&mut self, clients_present: bool, now: Instant) -> bool {
        let would_fire = match (&self.state, clients_present) {
            (State::Idle, true) => {
                self.state = State::SeenSince(now);
                false
            }
            (State::SeenSince(since), true) => {
                if now.duration_since(*since) >= DEBOUNCE {
                    self.state = State::Busy;
                    true
                } else {
                    false
                }
            }
            (State::SeenSince(_), false) | (State::Busy, false) => {
                self.state = State::Idle;
                false
            }
            (State::Idle, false) | (State::Busy, true) => false,
        };
        if !would_fire {
            return false;
        }
        if self.last_fired.is_some_and(|t| now.duration_since(t) < MIN_GAP) {
            return false;
        }
        self.last_fired = Some(now);
        true
    }

    /// Moves the detector into its post-fire state without firing. A call
    /// that is still running when the tick observes it cannot re-prompt; the
    /// detector re-arms only after an absence.
    pub fn mark_handled(&mut self) {
        self.state = State::Busy;
    }
}

/// Fires once per episode of the call app leaving the microphone during a
/// recording.
///
/// An app arms the detector by holding the microphone for [`ARM_AFTER`]. The
/// episode ends when every armed app is off the microphone and the system
/// track is quiet, both continuously for [`DEBOUNCE_END`]. After firing, the
/// detector re-arms when an app holds the microphone for [`ARM_AFTER`] again.
#[derive(Debug, Default)]
pub struct EndDetector {
    /// Apps currently on the microphone, by the instant they appeared.
    present_since: BTreeMap<String, Instant>,
    armed: BTreeSet<String>,
    /// The armed app most recently seen on the microphone.
    last_armed_seen: Option<String>,
    quiet_since: Option<Instant>,
    fired: bool,
}

impl EndDetector {
    pub fn new() -> Self {
        Self::default()
    }

    /// Forgets the episode; used when the recording pauses or stops.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Advances the machine with the app names on the microphone and whether
    /// the system track was quiet since the previous step. Returns the name
    /// of the app that left when the stop prompt should be shown now.
    pub fn step(
        &mut self,
        clients: &[String],
        system_quiet: bool,
        now: Instant,
    ) -> Option<String> {
        self.present_since.retain(|name, _| clients.contains(name));
        for name in clients {
            self.present_since.entry(name.clone()).or_insert(now);
        }
        let qualified: Vec<String> = self
            .present_since
            .iter()
            .filter(|(_, since)| now.duration_since(**since) >= ARM_AFTER)
            .map(|(name, _)| name.clone())
            .collect();
        if self.fired {
            if qualified.is_empty() {
                return None;
            }
            let present_since = std::mem::take(&mut self.present_since);
            self.reset();
            self.present_since = present_since;
            return self.step(clients, system_quiet, now);
        }
        for name in &qualified {
            if self.armed.insert(name.clone()) {
                log::info!("meeting end: {name} armed the stop prompt");
            }
        }
        if let Some(name) = self
            .armed
            .iter()
            .rev()
            .find(|n| self.present_since.contains_key(*n))
        {
            self.last_armed_seen = Some(name.clone());
        }
        let ended = !self.armed.is_empty()
            && self.armed.iter().all(|n| !self.present_since.contains_key(n))
            && system_quiet;
        if !ended {
            if self.quiet_since.take().is_some() {
                log::info!(
                    "meeting end: window reset (armed app on the mic: {}, system quiet: {system_quiet})",
                    self.armed.iter().any(|n| self.present_since.contains_key(n))
                );
            }
            return None;
        }
        let since = *self.quiet_since.get_or_insert_with(|| {
            log::info!("meeting end: every armed app left the mic and the system track is quiet");
            now
        });
        if now.duration_since(since) < DEBOUNCE_END {
            return None;
        }
        self.fired = true;
        log::info!("meeting end: stop prompt fires for {:?}", self.last_armed_seen);
        self.last_armed_seen.clone()
    }
}

/// The recording as the monitor sees it on one tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Probe {
    Idle,
    Paused,
    /// Recording. `system_active` reports voice-level audio on the system
    /// track since the previous tick.
    Recording { system_active: bool },
}

/// Whether the prompt should stay quiet: unsupported hardware, the setting
/// off, a snooze still in effect, or the reminder window already showing.
pub fn should_hold(
    supported: bool,
    prompt: &MeetingPrompt,
    now_unix: i64,
    reminder_visible: bool,
) -> bool {
    !supported || !prompt.enabled || prompt.snoozed_at(now_unix) || reminder_visible
}

fn mic_clients_supported() -> bool {
    !matches!(mic_clients_support(), MicClientsSupport::No(_))
}

/// Whether onboarding is incomplete or the vault is locked. The detector
/// must not consult the microphone in either case.
fn onboarding_or_vault_locked(app: &tauri::AppHandle) -> bool {
    let vault_unlocked = app
        .try_state::<crate::state::VaultState>()
        .is_some_and(|v| v.is_unlocked());
    !crate::bootstrap::first_run_complete(
        crate::bootstrap::Eula::is_accepted(),
        crate::bootstrap::Consent::is_accepted(),
        crate::bootstrap::Onboarding::is_complete(),
        vault_unlocked,
    )
}

/// Starts the app-lifetime monitor: every [`PERIOD`] it asks who holds the
/// microphone. Outside a recording it calls `fire_start` when the episode has
/// lasted [`DEBOUNCE`]; during a recording it calls `fire_end` with the name
/// of the app that left when the meeting has ended. `probe` reports the
/// recording state.
pub fn spawn(
    app: tauri::AppHandle,
    probe: impl Fn() -> Probe + Send + 'static,
    fire_start: impl Fn(&tauri::AppHandle) + Send + 'static,
    fire_end: impl Fn(&tauri::AppHandle, &str) + Send + 'static,
) {
    std::thread::Builder::new()
        .name("daisy-meeting-detect".into())
        .spawn(move || {
            let mut detector = Detector::new();
            let mut end_detector = EndDetector::new();
            let mut asking_before = false;
            loop {
                std::thread::sleep(PERIOD);
                if onboarding_or_vault_locked(&app) {
                    continue;
                }
                let probed = probe();
                let Some(state) = app.try_state::<crate::state::AppState>() else {
                    continue;
                };
                let prompt = state.machine_settings().meeting_prompt;
                let visible = app
                    .get_webview_window(crate::mini_window::REMINDER_LABEL)
                    .and_then(|w| w.is_visible().ok())
                    .unwrap_or(false);
                match probed {
                    Probe::Paused => {
                        detector.mark_handled();
                        end_detector.reset();
                        continue;
                    }
                    Probe::Recording { system_active } => {
                        detector.mark_handled();
                        if !prompt.stop_enabled || !mic_clients_supported() {
                            end_detector.reset();
                            continue;
                        }
                        if visible {
                            continue;
                        }
                        let names: Vec<String> =
                            mic_clients().into_iter().map(|c| c.name).collect();
                        let left = end_detector.step(&names, !system_active, Instant::now());
                        if let Some(name) = left {
                            fire_end(&app, &name);
                        }
                        continue;
                    }
                    Probe::Idle => end_detector.reset(),
                }
                let now_unix = crate::now_unix();
                // The setting turning on, or a snooze ending, is a fresh
                // "ask me": a call already running gets its prompt after
                // the debounce, and the minimum gap starts over.
                let asking = prompt.enabled && !prompt.snoozed_at(now_unix);
                if asking && !asking_before {
                    detector = Detector::new();
                }
                asking_before = asking;
                let present = !mic_clients().is_empty();
                if !present {
                    detector.step(false, Instant::now());
                    continue;
                }
                if should_hold(mic_clients_supported(), &prompt, now_unix, visible) {
                    continue;
                }
                if detector.step(true, Instant::now()) {
                    fire_start(&app);
                }
            }
        })
        .expect("spawn meeting detect thread");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(base: Instant, secs: u64) -> Instant {
        base + Duration::from_secs(secs)
    }

    #[test]
    fn nothing_fires_before_five_seconds() {
        let t0 = Instant::now();
        let mut d = Detector::new();
        assert!(!d.step(true, at(t0, 0)));
        assert!(!d.step(true, at(t0, 2)));
        assert!(!d.step(true, at(t0, 4)));
    }

    #[test]
    fn one_fire_per_episode() {
        let t0 = Instant::now();
        let mut d = Detector::new();
        assert!(!d.step(true, at(t0, 0)));
        assert!(d.step(true, at(t0, 6)));
        assert!(!d.step(true, at(t0, 8)));
        assert!(!d.step(true, at(t0, 60)));
    }

    #[test]
    fn a_gap_before_five_seconds_restarts_the_wait() {
        let t0 = Instant::now();
        let mut d = Detector::new();
        assert!(!d.step(true, at(t0, 0)));
        assert!(!d.step(false, at(t0, 2)));
        assert!(!d.step(true, at(t0, 4)));
        assert!(!d.step(true, at(t0, 8)));
        assert!(d.step(true, at(t0, 10)));
    }

    #[test]
    fn a_new_episode_fires_again_once_the_minimum_gap_has_passed() {
        let t0 = Instant::now();
        let mut d = Detector::new();
        assert!(!d.step(true, at(t0, 0)));
        assert!(d.step(true, at(t0, 6)));
        assert!(!d.step(false, at(t0, 310)));
        assert!(!d.step(true, at(t0, 312)));
        assert!(d.step(true, at(t0, 318)));
    }

    #[test]
    fn an_idle_machine_never_fires() {
        let t0 = Instant::now();
        let mut d = Detector::new();
        for s in 0..30 {
            assert!(!d.step(false, at(t0, s)));
        }
    }

    #[test]
    fn a_flapping_client_fires_at_most_once_within_the_minimum_gap() {
        let t0 = Instant::now();
        let mut d = Detector::new();
        // First episode: present 6 s, fires once.
        assert!(!d.step(true, at(t0, 0)));
        assert!(d.step(true, at(t0, 6)));
        // Absent 2 s, then present again for 6 s: a second episode inside
        // the minimum gap since the first fire.
        assert!(!d.step(false, at(t0, 8)));
        assert!(!d.step(true, at(t0, 8)));
        assert!(!d.step(true, at(t0, 14)));
    }

    #[test]
    fn a_second_fire_is_allowed_after_the_minimum_gap() {
        let t0 = Instant::now();
        let mut d = Detector::new();
        assert!(!d.step(true, at(t0, 0)));
        assert!(d.step(true, at(t0, 6)));
        assert!(!d.step(false, at(t0, 8)));
        assert!(!d.step(true, at(t0, 8)));
        assert!(d.step(true, at(t0, 313)));
    }

    #[test]
    fn mark_handled_suppresses_a_running_call_then_absence_rearms() {
        let t0 = Instant::now();
        let mut d = Detector::new();
        d.mark_handled();
        assert!(!d.step(true, at(t0, 0)));
        assert!(!d.step(true, at(t0, 6)));
        assert!(!d.step(false, at(t0, 8)));
        assert!(!d.step(true, at(t0, 8)));
        assert!(d.step(true, at(t0, 14)));
    }

    fn apps(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| n.to_string()).collect()
    }

    /// Feeds one step every 2 s from `from` to `to` seconds inclusive and
    /// returns the first fire.
    fn run(
        d: &mut EndDetector,
        t0: Instant,
        from: u64,
        to: u64,
        clients: &[&str],
        quiet: bool,
    ) -> Option<String> {
        (from..=to)
            .step_by(2)
            .find_map(|s| d.step(&apps(clients), quiet, at(t0, s)))
    }

    #[test]
    fn an_in_person_recording_never_arms_or_fires() {
        let t0 = Instant::now();
        let mut d = EndDetector::new();
        assert_eq!(run(&mut d, t0, 0, 600, &[], true), None);
    }

    #[test]
    fn a_short_visitor_on_the_mic_does_not_arm() {
        let t0 = Instant::now();
        let mut d = EndDetector::new();
        assert_eq!(run(&mut d, t0, 0, 2, &["Fictional Call"], true), None);
        assert_eq!(run(&mut d, t0, 4, 120, &[], true), None);
    }

    #[test]
    fn fires_after_five_quiet_seconds_off_the_mic_and_names_the_app() {
        let t0 = Instant::now();
        let mut d = EndDetector::new();
        assert_eq!(run(&mut d, t0, 0, 60, &["Fictional Call"], false), None);
        assert_eq!(d.step(&[], true, at(t0, 62)), None);
        assert_eq!(d.step(&[], true, at(t0, 66)), None);
        assert_eq!(
            d.step(&[], true, at(t0, 67)),
            Some("Fictional Call".to_string())
        );
    }

    #[test]
    fn a_live_system_track_keeps_a_muted_participant_from_firing() {
        let t0 = Instant::now();
        let mut d = EndDetector::new();
        assert_eq!(run(&mut d, t0, 0, 30, &["Fictional Call"], false), None);
        assert_eq!(run(&mut d, t0, 32, 400, &[], false), None);
    }

    #[test]
    fn remote_voice_during_the_window_restarts_it() {
        let t0 = Instant::now();
        let mut d = EndDetector::new();
        assert_eq!(run(&mut d, t0, 0, 30, &["Fictional Call"], false), None);
        assert_eq!(run(&mut d, t0, 32, 34, &[], true), None);
        assert_eq!(d.step(&[], false, at(t0, 36)), None);
        assert_eq!(d.step(&[], true, at(t0, 38)), None);
        assert_eq!(d.step(&[], true, at(t0, 42)), None);
        assert!(d.step(&[], true, at(t0, 43)).is_some());
    }

    #[test]
    fn the_app_returning_restarts_the_window() {
        let t0 = Instant::now();
        let mut d = EndDetector::new();
        assert_eq!(run(&mut d, t0, 0, 30, &["Fictional Call"], false), None);
        assert_eq!(run(&mut d, t0, 32, 34, &[], true), None);
        assert_eq!(d.step(&apps(&["Fictional Call"]), true, at(t0, 36)), None);
        assert_eq!(run(&mut d, t0, 38, 42, &[], true), None);
        assert!(d.step(&[], true, at(t0, 43)).is_some());
    }

    #[test]
    fn every_armed_app_must_leave() {
        let t0 = Instant::now();
        let mut d = EndDetector::new();
        assert_eq!(
            run(&mut d, t0, 0, 30, &["Fictional Call", "Fictional Chat"], false),
            None
        );
        assert_eq!(run(&mut d, t0, 32, 120, &["Fictional Chat"], true), None);
        assert_eq!(
            run(&mut d, t0, 122, 160, &[], true),
            Some("Fictional Chat".to_string())
        );
    }

    #[test]
    fn fires_once_then_rearms_only_when_an_app_takes_the_mic_again() {
        let t0 = Instant::now();
        let mut d = EndDetector::new();
        assert_eq!(run(&mut d, t0, 0, 30, &["Fictional Call"], false), None);
        assert!(run(&mut d, t0, 32, 60, &[], true).is_some());
        assert_eq!(run(&mut d, t0, 62, 300, &[], true), None);
        assert_eq!(run(&mut d, t0, 302, 320, &["Fictional Call"], false), None);
        assert_eq!(
            run(&mut d, t0, 322, 360, &[], true),
            Some("Fictional Call".to_string())
        );
    }

    #[test]
    fn reset_forgets_the_episode() {
        let t0 = Instant::now();
        let mut d = EndDetector::new();
        assert_eq!(run(&mut d, t0, 0, 30, &["Fictional Call"], false), None);
        d.reset();
        assert_eq!(run(&mut d, t0, 32, 200, &[], true), None);
    }

    fn prompt(enabled: bool, snooze_until: Option<i64>) -> MeetingPrompt {
        MeetingPrompt { enabled, snooze_until, ..MeetingPrompt::default() }
    }

    #[test]
    fn should_hold_covers_each_condition_and_the_all_clear_case() {
        let now = 1_000;
        assert!(
            should_hold(false, &prompt(true, None), now, false),
            "unsupported hardware holds"
        );
        assert!(
            should_hold(true, &prompt(false, None), now, false),
            "disabled setting holds"
        );
        assert!(
            should_hold(true, &prompt(true, Some(now + 60)), now, false),
            "an active snooze holds"
        );
        assert!(
            should_hold(true, &prompt(true, None), now, true),
            "a visible reminder holds"
        );
        assert!(
            !should_hold(true, &prompt(true, None), now, false),
            "nothing holds"
        );
        assert!(
            !should_hold(true, &prompt(true, Some(now - 60)), now, false),
            "an expired snooze does not hold"
        );
    }
}
