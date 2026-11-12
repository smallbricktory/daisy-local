// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Live speaker names in captions:
//! a worker per recording reads the track's audio from disk, runs the
//! streaming Nemotron 3 Diarization graph, names slots against saved voices,
//! emits `live-speakers` events and keeps `live_speakers.<track>.json`.

pub mod display;
pub mod engine;
pub mod naming;
pub mod overlay;
pub mod worker;

use serde::Serialize;

/// Tauri event the frontend labels captions from.
pub const EVENT: &str = "live-speakers";

/// An emitter that sends each update to the frontend as the `live-speakers` event.
pub fn tauri_emit(app: tauri::AppHandle) -> worker::Emit {
    use tauri::Emitter;
    Box::new(move |u| {
        if let Err(e) = app.emit(EVENT, u) {
            log::warn!("live speakers: emit: {e}");
        }
    })
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TurnOut {
    pub start_s: f64,
    pub end_s: f64,
}

/// Who a slot is: a saved voice, or "Speaker n" by first appearance.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub enum NameOut {
    Voice { voice_id: String, display_name: String },
    Anonymous { ordinal: u32 },
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SlotOut {
    pub slot: u32,
    pub turns: Vec<TurnOut>,
    pub name: NameOut,
}

/// One slot in a `live-speakers` event. `turns` are closed turns, which no
/// later event changes: every one so far when the event is `full`, else the
/// ones closed since the previous event. `open` is the turn still growing
/// (in pieces when it crosses a pause) and replaces the previous event's.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SlotDelta {
    pub slot: u32,
    pub turns: Vec<TurnOut>,
    pub open: Vec<TurnOut>,
    pub name: NameOut,
}

/// Payload of the `live-speakers` event: every slot that has spoken on the
/// track, with what changed since the previous event. A `full` event
/// carries the whole history and replaces what the receiver kept.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LiveSpeakers {
    pub session_id: String,
    pub track: String,
    pub full: bool,
    pub tracks: Vec<SlotDelta>,
}

/// Reads a track from the chunk WAVs as the recorder writes them.
struct FeedSource(crate::live_audio::ChunkFeed);

impl worker::Source for FeedSource {
    fn poll(&mut self, done: bool, on: &mut dyn FnMut(worker::Pcm<'_>)) -> std::io::Result<()> {
        self.0.poll(done, &mut |e| {
            if let crate::live_audio::FeedEvent::Samples { offset_ms, start_sample, samples, .. } = e {
                on(worker::Pcm { offset_ms, start_sample, samples });
            }
        })
    }
}

/// The workers of one recording and the saved voices they name against.
struct Running {
    workers: Vec<worker::Handle>,
    voices: naming::Voices,
}

fn handles() -> std::sync::MutexGuard<'static, std::collections::HashMap<String, Running>> {
    static H: std::sync::LazyLock<std::sync::Mutex<std::collections::HashMap<String, Running>>> =
        std::sync::LazyLock::new(Default::default);
    H.lock().unwrap_or_else(|e| e.into_inner())
}

/// The tracks a recording runs live names on, each with the audio it reads:
/// the system track always; the mic, from its live echo-cancelled file, when
/// the local end holds more than one voice and captions show.
pub fn tracks_for(single_local_speaker: bool, show_captions: bool) -> Vec<(overlay::Track, crate::live_audio::FeedTrack)> {
    use crate::live_audio::FeedTrack;
    let mut t = vec![(overlay::Track::System, FeedTrack::System)];
    if !single_local_speaker && show_captions {
        t.push((overlay::Track::Mic, FeedTrack::MicLiveAec));
    }
    t
}

/// How often [`Captioned`] reads the captions setting.
const CAPTIONS_CHECK: std::time::Duration = std::time::Duration::from_secs(2);

/// A source that delivers only while captions show. Audio written while
/// they are hidden stays on disk and is read once they show again.
struct Captioned<S> {
    inner: S,
    shown: Box<dyn FnMut() -> bool + Send>,
    checked: Option<(std::time::Instant, bool)>,
}

impl<S: worker::Source> worker::Source for Captioned<S> {
    fn poll(&mut self, done: bool, on: &mut dyn FnMut(worker::Pcm<'_>)) -> std::io::Result<()> {
        if done {
            return self.inner.poll(done, on);
        }
        let now = std::time::Instant::now();
        let shown = match self.checked {
            Some((at, v)) if now.duration_since(at) < CAPTIONS_CHECK => v,
            _ => {
                let v = (self.shown)();
                self.checked = Some((now, v));
                v
            }
        };
        if !shown {
            return Ok(());
        }
        self.inner.poll(done, on)
    }
}

/// Starts live names on the recording at `root`, naming slots against the
/// unlocked vault's voices (none while it is locked): the system track, and
/// the mic in the room case (see [`tracks_for`]). `single_local_speaker` is
/// the value the recording started with.
pub fn start_for_recording(app: &tauri::AppHandle, session_id: &str, root: std::path::PathBuf, single_local_speaker: bool) {
    use tauri::Manager;
    let state = app.state::<crate::state::AppState>();
    if !state.machine_settings().live_speaker_names {
        return;
    }
    let vault = app.state::<crate::state::VaultState>();
    let contacts = crate::commands::contacts::load_contacts(&state).unwrap_or_default();
    let voices: naming::Voices = std::sync::Arc::new(std::sync::Mutex::new(
        vault.keys.lock().unwrap().as_ref().map(|k| worker::known_voices(k, &contacts)).unwrap_or_default(),
    ));
    let show_captions = state.machine_settings().show_live_captions;
    let mut started = Vec::new();
    for (track, feed) in tracks_for(single_local_speaker, show_captions) {
        let params = worker::Params {
            session_id: session_id.to_string(),
            session_root: root.clone(),
            track,
            model_dir: worker::model_dir(),
            voices: voices.clone(),
        };
        let chunks = FeedSource(crate::live_audio::ChunkFeed::new(root.clone(), feed));
        let source: Box<dyn worker::Source> = match track {
            overlay::Track::System => Box::new(chunks),
            overlay::Track::Mic => {
                let app = app.clone();
                Box::new(Captioned {
                    inner: chunks,
                    shown: Box::new(move || app.state::<crate::state::AppState>().machine_settings().show_live_captions),
                    checked: None,
                })
            }
        };
        started.push(worker::start(params, source, tauri_emit(app.clone())));
    }
    add(session_id, Running { workers: started, voices });
}

/// Registers a recording's workers; one already registered for the session
/// is told the recording is done.
fn add(session_id: &str, running: Running) {
    if let Some(old) = handles().insert(session_id.to_string(), running) {
        for h in &old.workers {
            h.recording_done();
        }
        std::thread::spawn(move || drop(old));
    }
}

/// The recorder has stopped: each worker reads what is left, writes its
/// final overlay and ends on its own thread.
pub fn recording_stopped(session_id: &str) {
    if let Some(r) = handles().get(session_id) {
        for h in &r.workers {
            h.recording_done();
        }
    }
}

/// How long [`cancel`] waits for the workers to end.
const CANCEL_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

/// Ends the session's workers without another overlay write. Returns once
/// none can write, after waiting up to five seconds for their threads.
pub fn cancel(session_id: &str) {
    let Some(mut r) = handles().remove(session_id) else { return };
    for h in &r.workers {
        h.abort();
    }
    let until = std::time::Instant::now() + CANCEL_WAIT;
    for h in &mut r.workers {
        if !h.wait(until) {
            log::warn!("live speakers {session_id}: a worker is still running after cancel");
        }
    }
}

/// Waits up to `limit` for the session's workers to write their final
/// overlays; a worker still running then is cancelled. Returns once none of
/// them can write.
pub fn wait(session_id: &str, limit: std::time::Duration) {
    let Some(mut r) = handles().remove(session_id) else { return };
    let until = std::time::Instant::now() + limit;
    for h in &mut r.workers {
        h.recording_done();
    }
    for h in &mut r.workers {
        if !h.wait(until) {
            log::warn!("live speakers {session_id}: still running after {limit:?}; cancelled");
            h.abort();
        }
    }
}

/// App exit: every worker is told the recording is done and given until
/// `limit` to finish; those still running are cancelled and given one more
/// second.
pub fn shutdown(limit: std::time::Duration) {
    let all: Vec<(String, Running)> = handles().drain().collect();
    let until = std::time::Instant::now() + limit;
    let mut left = Vec::new();
    for (_, mut r) in all {
        for h in &r.workers {
            h.recording_done();
        }
        for mut h in r.workers.drain(..) {
            if !h.wait(until) {
                h.abort();
                left.push(h);
            }
        }
    }
    let until = std::time::Instant::now() + std::time::Duration::from_secs(1);
    for mut h in left {
        h.wait(until);
    }
}

/// The vault locked: every running recording's workers lose the saved
/// voices; their slots read by ordinal from the next step on.
pub fn forget_voices() {
    for r in handles().values() {
        r.voices.lock().unwrap_or_else(|e| e.into_inner()).clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_mic_gets_an_instance_only_in_the_room_case_with_captions_shown() {
        use crate::live_audio::FeedTrack;
        let sys = (overlay::Track::System, FeedTrack::System);
        let mic = (overlay::Track::Mic, FeedTrack::MicLiveAec);
        assert_eq!(tracks_for(true, true), vec![sys]);
        assert_eq!(tracks_for(true, false), vec![sys]);
        assert_eq!(tracks_for(false, false), vec![sys]);
        assert_eq!(tracks_for(false, true), vec![sys, mic]);
    }

    struct Counting(std::sync::Arc<std::sync::atomic::AtomicU32>);
    impl worker::Source for Counting {
        fn poll(&mut self, _: bool, on: &mut dyn FnMut(worker::Pcm<'_>)) -> std::io::Result<()> {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            on(worker::Pcm { offset_ms: 0, start_sample: 0, samples: &[0.0; 4] });
            Ok(())
        }
    }

    #[test]
    fn hidden_captions_hold_the_mic_source_and_shown_ones_pass_it() {
        use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
        use std::sync::Arc;
        use worker::Source;
        let polls = Arc::new(AtomicU32::new(0));
        let shown = Arc::new(AtomicBool::new(false));
        let reads = Arc::new(AtomicU32::new(0));
        let mut src = Captioned {
            inner: Counting(polls.clone()),
            shown: {
                let (shown, reads) = (shown.clone(), reads.clone());
                Box::new(move || {
                    reads.fetch_add(1, Ordering::SeqCst);
                    shown.load(Ordering::SeqCst)
                })
            },
            checked: None,
        };
        let mut got = 0;
        src.poll(false, &mut |_| got += 1).unwrap();
        src.poll(false, &mut |_| got += 1).unwrap();
        assert_eq!((got, polls.load(Ordering::SeqCst)), (0, 0));
        // The setting is read once per check interval.
        assert_eq!(reads.load(Ordering::SeqCst), 1);
        shown.store(true, Ordering::SeqCst);
        src.checked = None;
        src.poll(false, &mut |_| got += 1).unwrap();
        assert_eq!((got, polls.load(Ordering::SeqCst)), (1, 1));
    }

    #[test]
    fn captions_hidden_still_let_the_tail_through_after_stop() {
        use std::sync::atomic::{AtomicU32, Ordering};
        use std::sync::Arc;
        use worker::Source;
        let polls = Arc::new(AtomicU32::new(0));
        let mut src = Captioned { inner: Counting(polls.clone()), shown: Box::new(|| false), checked: None };
        let mut got = 0;
        src.poll(false, &mut |_| got += 1).unwrap();
        assert_eq!(got, 0);
        src.poll(true, &mut |_| got += 1).unwrap();
        assert_eq!((got, polls.load(Ordering::SeqCst)), (1, 1));
    }

    fn running(voices: Vec<naming::Voice>) -> Running {
        Running { workers: vec![], voices: std::sync::Arc::new(std::sync::Mutex::new(voices)) }
    }

    #[test]
    fn a_lock_empties_every_recordings_voices() {
        let v = naming::Voice { id: "v1".into(), display_name: "Ana Okafor".into(), is_me: false, print: vec![1.0] };
        let r = running(vec![v]);
        let shared = r.voices.clone();
        add("daisy-test-lock", r);
        forget_voices();
        assert!(shared.lock().unwrap().is_empty());
        cancel("daisy-test-lock");
        assert!(!handles().contains_key("daisy-test-lock"));
    }

    #[test]
    fn the_event_payload_has_the_spec_shape() {
        let p = LiveSpeakers {
            session_id: "daisy-1".into(),
            track: "system".into(),
            full: false,
            tracks: vec![
                SlotDelta {
                    slot: 0,
                    turns: vec![TurnOut { start_s: 1.0, end_s: 2.5 }],
                    open: vec![TurnOut { start_s: 3.0, end_s: 3.5 }],
                    name: NameOut::Voice { voice_id: "v1".into(), display_name: "Ana Okafor".into() },
                },
                SlotDelta { slot: 1, turns: vec![], open: vec![], name: NameOut::Anonymous { ordinal: 2 } },
            ],
        };
        let v = serde_json::to_value(&p).unwrap();
        assert_eq!(v["session_id"], "daisy-1");
        assert_eq!(v["full"], false);
        assert_eq!(v["tracks"][0]["open"][0]["end_s"], 3.5);
        assert_eq!(v["tracks"][0]["name"]["display_name"], "Ana Okafor");
        assert_eq!(v["tracks"][1]["name"]["ordinal"], 2);
        assert_eq!(v["tracks"][0]["turns"][0]["start_s"], 1.0);
    }
}
