// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Pauses the active recording when the computer sleeps and resumes it when
//! the computer wakes.
//!
//! - Suspend while recording: the recording pauses through `pause_impl`
//!   (chunk WAV closed, manifest segment closed) and the pause is held as a
//!   sleep pause.
//! - Resume after a sleep pause: the recording resumes through
//!   `resume_impl`, the new segment carries `reason: "system_sleep"`, and
//!   `recording:slept` goes to the frontend.
//! - A recording the user paused stays paused; repeated events are no-ops.
//!
//! Events run in arrival order on one reaction thread. The OS callback waits
//! for its reaction up to [`REACTION_WAIT`].

use crate::commands::recording::{
    pause_impl, recording_snapshot_impl, resume_impl, ActiveRecording, ActiveRecordingHandle,
};
use crate::error::Result;
use crate::state::AppState;
use serde::Serialize;
use std::path::Path;
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;
use system_sleep::{PowerEvent, PowerMonitor};

/// How long a power callback waits for the reaction before letting the OS
/// continue.
pub const REACTION_WAIT: Duration = Duration::from_secs(4);

/// Event carrying a [`SleptNotice`].
pub const SLEPT_EVENT: &str = "recording:slept";

/// The active recording's capture state, as the sleep reaction sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Capture {
    Recording,
    Paused,
}

/// What the sleep reaction needs from the active recording.
pub trait RecordingControl {
    /// Session id and capture state of the active recording; `None` when
    /// nothing is recording.
    fn current(&self) -> Option<(String, Capture)>;
    fn pause(&self) -> Result<()>;
    fn resume(&self) -> Result<()>;
    /// Marks the open segment of `session_id` as opened after a system sleep.
    fn mark_resumed_after_sleep(&self, session_id: &str) -> Result<()>;
}

/// A recording paused by a system sleep, reported on wake.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SleptNotice {
    pub session_id: String,
    pub paused_at_unix_seconds: i64,
    pub resumed_at_unix_seconds: i64,
    /// False when the recording failed to restart and stays paused.
    pub resumed: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Reaction {
    Nothing,
    Paused,
    Woke(SleptNotice),
}

#[derive(Debug)]
struct SleepPaused {
    session_id: String,
    paused_at: i64,
}

/// The sleep reaction's state: the pause it holds, if any.
#[derive(Debug, Default)]
pub struct SleepPause {
    held: Option<SleepPaused>,
}

impl SleepPause {
    /// Reacts to `event`, reported by the OS at `now` (Unix seconds).
    pub fn react(&mut self, event: PowerEvent, ctl: &dyn RecordingControl, now: i64) -> Reaction {
        match event {
            PowerEvent::Suspend => self.on_suspend(ctl, now),
            PowerEvent::Resume => self.on_resume(ctl, now),
        }
    }

    fn on_suspend(&mut self, ctl: &dyn RecordingControl, now: i64) -> Reaction {
        if self.held.is_some() {
            return Reaction::Nothing;
        }
        let Some((session_id, Capture::Recording)) = ctl.current() else {
            return Reaction::Nothing;
        };
        if let Err(e) = ctl.pause() {
            log::warn!("recording: pause for system sleep failed: {e}");
            if ctl.current() != Some((session_id.clone(), Capture::Paused)) {
                return Reaction::Nothing;
            }
        }
        log::info!("recording: paused session {session_id} for system sleep");
        self.held = Some(SleepPaused { session_id, paused_at: now });
        Reaction::Paused
    }

    fn on_resume(&mut self, ctl: &dyn RecordingControl, now: i64) -> Reaction {
        let Some(held) = self.held.take() else {
            return Reaction::Nothing;
        };
        match ctl.current() {
            Some((sid, Capture::Paused)) if sid == held.session_id => {}
            _ => {
                log::info!(
                    "recording: session {} was stopped or resumed during sleep; no auto-resume",
                    held.session_id
                );
                return Reaction::Nothing;
            }
        }
        if let Err(e) = ctl.resume() {
            log::warn!("recording: resume after system sleep failed: {e}");
        }
        let resumed = ctl.current() == Some((held.session_id.clone(), Capture::Recording));
        if resumed {
            if let Err(e) = ctl.mark_resumed_after_sleep(&held.session_id) {
                log::warn!("recording: marking the post-sleep segment failed: {e}");
            }
            log::info!("recording: resumed session {} after system sleep", held.session_id);
        } else {
            log::warn!("recording: session {} stays paused after system sleep", held.session_id);
        }
        Reaction::Woke(SleptNotice {
            session_id: held.session_id,
            paused_at_unix_seconds: held.paused_at,
            resumed_at_unix_seconds: now,
            resumed,
        })
    }
}

/// Sets `reason: "system_sleep"` on the open segment of the session at
/// `session_dir`.
pub fn mark_open_segment_after_sleep(session_dir: &Path) -> Result<()> {
    let mut session = recording::session::Session::load(session_dir)?;
    session.update_manifest(|m| {
        recording::manifest_ops::set_open_segment_reason(m, recording::manifest::SEGMENT_REASON_SYSTEM_SLEEP)
    })?;
    Ok(())
}

/// [`RecordingControl`] over the app's active recording slot.
pub struct LiveControl<'a> {
    pub app: &'a AppState,
    pub active: &'a Mutex<Option<ActiveRecording>>,
}

impl RecordingControl for LiveControl<'_> {
    fn current(&self) -> Option<(String, Capture)> {
        let snap = recording_snapshot_impl(self.active)?;
        let capture = match snap.state {
            "recording" => Capture::Recording,
            "paused" => Capture::Paused,
            _ => return None,
        };
        Some((snap.session_id, capture))
    }

    fn pause(&self) -> Result<()> {
        pause_impl(self.app, self.active).map(|_| ())
    }

    fn resume(&self) -> Result<()> {
        resume_impl(self.app, self.active).map(|_| ())
    }

    fn mark_resumed_after_sleep(&self, session_id: &str) -> Result<()> {
        // The recorder writes the manifest under this lock.
        let _slot = self.active.lock().unwrap();
        mark_open_segment_after_sleep(&self.app.profile.session_path(session_id))
    }
}

/// Tauri-managed keep-alive for the app-lifetime power monitor.
pub struct SleepWatch {
    _monitor: Option<PowerMonitor>,
}

/// Starts the power monitor and its reaction thread. Linux holds a logind
/// delay lock only while a recording is active.
pub fn start(app: &tauri::AppHandle) -> SleepWatch {
    let (tx, rx) = mpsc::channel::<(PowerEvent, i64, mpsc::Sender<()>)>();
    let reactor_app = app.clone();
    let spawned = std::thread::Builder::new().name("daisy-sleep-reaction".into()).spawn(move || {
        let mut pause = SleepPause::default();
        for (event, at, done) in rx {
            react_and_publish(&reactor_app, &mut pause, event, at);
            let _ = done.send(());
        }
    });
    if let Err(e) = spawned {
        log::warn!("power monitor: reaction thread spawn failed: {e}");
        return SleepWatch { _monitor: None };
    }
    let handler: system_sleep::monitor::PowerHandler = Arc::new(move |event| {
        log::info!("power: system {}", match event {
            PowerEvent::Suspend => "suspend",
            PowerEvent::Resume => "resume",
        });
        let (done_tx, done_rx) = mpsc::channel();
        if tx.send((event, crate::now_unix(), done_tx)).is_ok() && done_rx.recv_timeout(REACTION_WAIT).is_err() {
            log::warn!("power: reaction to {event:?} still running after {REACTION_WAIT:?}");
        }
    });
    let delay_wanted: system_sleep::monitor::DelayWanted =
        Arc::new(|| crate::perf::RECORDING_ACTIVE.load(std::sync::atomic::Ordering::Relaxed));
    SleepWatch { _monitor: PowerMonitor::start(handler, delay_wanted) }
}

/// Reacts to `event`, notified by the OS at `at` (Unix seconds).
fn react_and_publish(app: &tauri::AppHandle, pause: &mut SleepPause, event: PowerEvent, at: i64) {
    use tauri::{Emitter, Manager};
    let state = app.state::<AppState>();
    let active = app.state::<ActiveRecordingHandle>();
    let ctl = LiveControl { app: &state, active: &active.0 };
    let reaction = pause.react(event, &ctl, at);
    if reaction == Reaction::Nothing {
        return;
    }
    if let Some(snap) = recording_snapshot_impl(&active.0) {
        let _ = app.emit("recording:snapshot", &snap);
    }
    if let Reaction::Woke(notice) = reaction {
        let _ = app.emit(SLEPT_EVENT, &notice);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::recording::{resume_open_segment, stop_close_segment};
    use crate::error::AppError;
    use std::cell::{Cell, RefCell};

    const MANIFEST: &str = r#"{
      "schema_version": 2,
      "session_id": "daisy-100",
      "created_at_unix_seconds": 100,
      "sample_rate": 16000,
      "channels": 1,
      "chunks": [{
        "index": 1,
        "started_at_unix_seconds": 100,
        "mic_wav_relative": "chunks/0001/mic.wav",
        "system_wav_relative": "chunks/0001/system.wav"
      }],
      "recording_segments": [{ "started_at_unix_seconds": 100, "first_chunk_index": 1 }]
    }"#;

    /// A recording whose pause/resume run the same manifest segment
    /// functions as `pause_impl`/`resume_impl`, against a temp session.
    struct Fake {
        dir: tempfile::TempDir,
        state: RefCell<Option<Capture>>,
        clock: Cell<i64>,
        pauses: Cell<u32>,
        resumes: Cell<u32>,
        resume_fails: bool,
        /// The manifest write after the recorder's pause/resume fails.
        manifest_write_fails: bool,
    }

    impl Fake {
        fn new(state: Option<Capture>) -> Self {
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(dir.path().join("manifest.json"), MANIFEST).unwrap();
            Self {
                dir,
                state: RefCell::new(state),
                clock: Cell::new(100),
                pauses: Cell::new(0),
                resumes: Cell::new(0),
                resume_fails: false,
                manifest_write_fails: false,
            }
        }
        fn manifest(&self) -> recording::manifest::SessionManifest {
            recording::session::Session::load(self.dir.path()).unwrap().manifest().clone()
        }
        fn at(&self, t: i64) -> i64 {
            self.clock.set(t);
            t
        }
    }

    impl RecordingControl for Fake {
        fn current(&self) -> Option<(String, Capture)> {
            self.state.borrow().map(|c| ("daisy-100".to_string(), c))
        }
        fn pause(&self) -> Result<()> {
            if *self.state.borrow() != Some(Capture::Recording) {
                return Err(AppError::Recording("invalid transition".into()));
            }
            self.pauses.set(self.pauses.get() + 1);
            *self.state.borrow_mut() = Some(Capture::Paused);
            if self.manifest_write_fails {
                return Err(AppError::Config("write manifest: disk full".into()));
            }
            stop_close_segment(self.dir.path(), self.clock.get())
        }
        fn resume(&self) -> Result<()> {
            if self.resume_fails {
                return Err(AppError::Recording("device gone".into()));
            }
            if *self.state.borrow() != Some(Capture::Paused) {
                return Err(AppError::Recording("invalid transition".into()));
            }
            self.resumes.set(self.resumes.get() + 1);
            *self.state.borrow_mut() = Some(Capture::Recording);
            if self.manifest_write_fails {
                return Err(AppError::Config("write manifest: disk full".into()));
            }
            resume_open_segment(self.dir.path(), self.clock.get())
        }
        fn mark_resumed_after_sleep(&self, _session_id: &str) -> Result<()> {
            mark_open_segment_after_sleep(self.dir.path())
        }
    }

    #[test]
    fn suspend_while_recording_closes_the_segment_and_holds_a_sleep_pause() {
        let fake = Fake::new(Some(Capture::Recording));
        let mut pause = SleepPause::default();
        assert_eq!(pause.react(PowerEvent::Suspend, &fake, fake.at(160)), Reaction::Paused);
        assert_eq!(*fake.state.borrow(), Some(Capture::Paused));
        let m = fake.manifest();
        assert_eq!(m.recording_segments.len(), 1);
        assert_eq!(m.recording_segments[0].stopped_at_unix_seconds, Some(160));
        assert_eq!(m.recording_segments[0].last_chunk_index, Some(1));
        let held = pause.held.as_ref().expect("sleep pause held");
        assert_eq!((held.session_id.as_str(), held.paused_at), ("daisy-100", 160));
    }

    #[test]
    fn resume_after_a_sleep_pause_continues_recording_with_a_notice() {
        let fake = Fake::new(Some(Capture::Recording));
        let mut pause = SleepPause::default();
        pause.react(PowerEvent::Suspend, &fake, fake.at(160));
        let reaction = pause.react(PowerEvent::Resume, &fake, fake.at(400));
        assert_eq!(
            reaction,
            Reaction::Woke(SleptNotice {
                session_id: "daisy-100".into(),
                paused_at_unix_seconds: 160,
                resumed_at_unix_seconds: 400,
                resumed: true,
            })
        );
        assert_eq!(*fake.state.borrow(), Some(Capture::Recording));
        let m = fake.manifest();
        assert_eq!(m.recording_segments.len(), 2);
        assert_eq!(m.recording_segments[1].started_at_unix_seconds, 400);
        assert_eq!(m.recording_segments[1].first_chunk_index, 2);
        assert_eq!(m.recording_segments[1].reason.as_deref(), Some("system_sleep"));
        assert_eq!(m.recording_segments[0].reason, None);
        assert!(pause.held.is_none());
    }

    #[test]
    fn suspend_while_user_paused_stays_paused_on_wake() {
        let fake = Fake::new(Some(Capture::Paused));
        let mut pause = SleepPause::default();
        assert_eq!(pause.react(PowerEvent::Suspend, &fake, fake.at(160)), Reaction::Nothing);
        assert_eq!(pause.react(PowerEvent::Resume, &fake, fake.at(400)), Reaction::Nothing);
        assert_eq!(*fake.state.borrow(), Some(Capture::Paused));
        assert_eq!((fake.pauses.get(), fake.resumes.get()), (0, 0));
    }

    #[test]
    fn repeated_suspend_and_resume_events_act_once() {
        let fake = Fake::new(Some(Capture::Recording));
        let mut pause = SleepPause::default();
        assert_eq!(pause.react(PowerEvent::Suspend, &fake, fake.at(160)), Reaction::Paused);
        assert_eq!(pause.react(PowerEvent::Suspend, &fake, fake.at(161)), Reaction::Nothing);
        assert!(matches!(pause.react(PowerEvent::Resume, &fake, fake.at(400)), Reaction::Woke(_)));
        assert_eq!(pause.react(PowerEvent::Resume, &fake, fake.at(401)), Reaction::Nothing);
        assert_eq!((fake.pauses.get(), fake.resumes.get()), (1, 1));
        assert_eq!(fake.manifest().recording_segments.len(), 2);
    }

    #[test]
    fn a_resume_without_a_prior_suspend_is_ignored() {
        let fake = Fake::new(Some(Capture::Recording));
        let mut pause = SleepPause::default();
        assert_eq!(pause.react(PowerEvent::Resume, &fake, fake.at(400)), Reaction::Nothing);
        assert_eq!(fake.resumes.get(), 0);
    }

    #[test]
    fn a_recording_the_user_resumed_or_stopped_meanwhile_is_left_alone() {
        let fake = Fake::new(Some(Capture::Recording));
        let mut pause = SleepPause::default();
        pause.react(PowerEvent::Suspend, &fake, fake.at(160));
        fake.resume().unwrap();
        assert_eq!(pause.react(PowerEvent::Resume, &fake, fake.at(400)), Reaction::Nothing);
        assert_eq!(fake.resumes.get(), 1, "only the user's resume");

        let stopped = Fake::new(Some(Capture::Recording));
        let mut pause = SleepPause::default();
        pause.react(PowerEvent::Suspend, &stopped, stopped.at(160));
        *stopped.state.borrow_mut() = None;
        assert_eq!(pause.react(PowerEvent::Resume, &stopped, stopped.at(400)), Reaction::Nothing);
    }

    #[test]
    fn a_failed_resume_reports_the_sleep_and_stays_paused() {
        let mut fake = Fake::new(Some(Capture::Recording));
        fake.resume_fails = true;
        let mut pause = SleepPause::default();
        pause.react(PowerEvent::Suspend, &fake, fake.at(160));
        let Reaction::Woke(notice) = pause.react(PowerEvent::Resume, &fake, fake.at(400)) else {
            panic!("expected a notice");
        };
        assert!(!notice.resumed);
        assert_eq!(*fake.state.borrow(), Some(Capture::Paused));
        assert_eq!(fake.manifest().recording_segments.len(), 1);
    }

    #[test]
    fn a_pause_whose_manifest_write_failed_is_still_held_and_resumed() {
        let mut fake = Fake::new(Some(Capture::Recording));
        fake.manifest_write_fails = true;
        let mut pause = SleepPause::default();
        assert_eq!(pause.react(PowerEvent::Suspend, &fake, fake.at(160)), Reaction::Paused);
        let Reaction::Woke(notice) = pause.react(PowerEvent::Resume, &fake, fake.at(400)) else {
            panic!("expected a notice");
        };
        assert!(notice.resumed, "the recorder is running again");
        assert_eq!(*fake.state.borrow(), Some(Capture::Recording));
    }

    #[test]
    fn suspend_with_nothing_recording_does_nothing() {
        let fake = Fake::new(None);
        let mut pause = SleepPause::default();
        assert_eq!(pause.react(PowerEvent::Suspend, &fake, fake.at(160)), Reaction::Nothing);
        assert!(pause.held.is_none());
    }
}
