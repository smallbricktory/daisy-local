// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Turns "another app holds the microphone" into one prompt per episode.

use std::time::{Duration, Instant};

use audio_engine::mic_clients::{mic_clients, mic_clients_support, MicClientsSupport};
use tauri::Manager;

use crate::settings::MeetingPrompt;

/// How long the microphone must stay taken before the prompt fires.
pub const DEBOUNCE: Duration = Duration::from_secs(5);

/// How often the monitor thread asks.
pub const PERIOD: Duration = Duration::from_secs(2);

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
/// microphone, and calls `fire` when the episode has lasted [`DEBOUNCE`].
/// `busy` reports a recording or paused session.
pub fn spawn(
    app: tauri::AppHandle,
    busy: impl Fn() -> bool + Send + 'static,
    fire: impl Fn(&tauri::AppHandle) + Send + 'static,
) {
    std::thread::Builder::new()
        .name("daisy-meeting-detect".into())
        .spawn(move || {
            let mut detector = Detector::new();
            let mut asking_before = false;
            loop {
                std::thread::sleep(PERIOD);
                if onboarding_or_vault_locked(&app) {
                    continue;
                }
                if busy() {
                    detector.mark_handled();
                    continue;
                }
                let Some(state) = app.try_state::<crate::state::AppState>() else {
                    continue;
                };
                let prompt = state.machine_settings().meeting_prompt;
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
                let visible = app
                    .get_webview_window(crate::mini_window::REMINDER_LABEL)
                    .and_then(|w| w.is_visible().ok())
                    .unwrap_or(false);
                let supported = !matches!(mic_clients_support(), MicClientsSupport::No(_));
                if should_hold(supported, &prompt, now_unix, visible) {
                    continue;
                }
                if detector.step(true, Instant::now()) {
                    fire(&app);
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

    fn prompt(enabled: bool, snooze_until: Option<i64>) -> MeetingPrompt {
        MeetingPrompt { enabled, snooze_until }
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
