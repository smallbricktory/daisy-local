// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! The per-recording live-names thread: polls a track's audio source, feeds
//! the engine, emits updates and keeps the overlay file. The thread never
//! stops for falling behind; it reads what is on disk and catches up.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use providers_local::nemotron3_diar::{self, LiveDiarizer};

use super::engine::Engine;
use super::naming::{self, Voice, Voices};
use super::overlay::{self, Overlay, Track};
use super::LiveSpeakers;
use crate::commands::contacts::Contact;
use crate::state::DecryptedKeys;

/// ONNX Runtime threads the diarizer may use.
pub const DIAR_THREADS: usize = 2;
/// Audio between two overlay writes.
const WRITE_EVERY_S: f64 = 60.0;
const IDLE: Duration = Duration::from_millis(250);

/// One stretch of a track's audio: mono 16 kHz, `start_sample` samples into
/// the chunk that begins `offset_ms` on the session clock.
pub struct Pcm<'a> {
    pub offset_ms: u32,
    pub start_sample: u64,
    pub samples: &'a [f32],
}

/// Where a worker reads audio from. `done` is true once the recording has
/// stopped; the source then delivers what is left.
pub trait Source: Send {
    fn poll(&mut self, done: bool, on: &mut dyn FnMut(Pcm<'_>)) -> std::io::Result<()>;
}

pub struct Params {
    pub session_id: String,
    pub session_root: PathBuf,
    pub track: Track,
    pub model_dir: PathBuf,
    pub voices: Voices,
}

pub type Emit = Box<dyn FnMut(&LiveSpeakers) + Send>;

/// What a worker's owner tells it, and whether it has ended.
#[derive(Default)]
struct Ctl {
    done: AtomicBool,
    abort: AtomicBool,
    /// Held across each overlay write; `abort` is set under it, so no write
    /// starts once [`Handle::abort`] returns.
    write: Mutex<()>,
    ended: Mutex<bool>,
    ended_cv: Condvar,
}

impl Ctl {
    /// Writes `o` unless the worker was aborted.
    fn write(&self, params: &Params, o: &Overlay) {
        let _g = self.write.lock().unwrap_or_else(|e| e.into_inner());
        if self.abort.load(Ordering::SeqCst) {
            return;
        }
        if let Err(e) = overlay::write(&params.session_root, params.track, o) {
            log::warn!("live speakers: write overlay: {e}");
        }
    }

    fn aborted(&self) -> bool {
        self.abort.load(Ordering::SeqCst)
    }
}

/// Marks the worker ended when its thread returns or unwinds.
struct Ended(Arc<Ctl>);

impl Drop for Ended {
    fn drop(&mut self) {
        *self.0.ended.lock().unwrap_or_else(|e| e.into_inner()) = true;
        self.0.ended_cv.notify_all();
    }
}

pub struct Handle {
    ctl: Arc<Ctl>,
    join: Option<JoinHandle<()>>,
}

impl Handle {
    /// The recording has stopped: the worker reads what is left, writes its
    /// final overlay and ends.
    pub fn recording_done(&self) {
        self.ctl.done.store(true, Ordering::SeqCst);
    }

    /// The worker stops at its next step and writes nothing more; once this
    /// returns, no overlay write of it is under way.
    pub fn abort(&self) {
        let _g = self.ctl.write.lock().unwrap_or_else(|e| e.into_inner());
        self.ctl.abort.store(true, Ordering::SeqCst);
    }

    /// Waits until the worker ends or `until` passes; true once it has
    /// ended (its thread is then joined).
    pub fn wait(&mut self, until: Instant) -> bool {
        let mut ended = self.ctl.ended.lock().unwrap_or_else(|e| e.into_inner());
        while !*ended {
            let now = Instant::now();
            if now >= until {
                return false;
            }
            ended = self.ctl.ended_cv.wait_timeout(ended, until - now).unwrap_or_else(|e| e.into_inner()).0;
        }
        drop(ended);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
        true
    }

    /// Marks the recording done and waits for the final overlay.
    pub fn stop(mut self) {
        self.recording_done();
        while !self.wait(Instant::now() + Duration::from_secs(3600)) {}
    }
}

/// Directory the diarizer's files load from: `DAISY_NEMOTRON3_DIAR_DIR`, else
/// `models/nemotron3-diar/` next to the executable.
pub fn model_dir() -> PathBuf {
    if let Ok(p) = std::env::var("DAISY_NEMOTRON3_DIAR_DIR") {
        return PathBuf::from(p);
    }
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|p| p.to_path_buf()))
        .unwrap_or_default()
        .join("models")
        .join("nemotron3-diar")
}

fn fold(s: &str) -> String {
    s.trim().to_lowercase()
}

/// The saved voices a namer compares against: each voice's samples in this
/// build's embedding space, meaned. A voice is the local user's when its contact
/// is named "Me", or, with no contact on file, when it is.
pub fn known_voices(keys: &DecryptedKeys, contacts: &[Contact]) -> Vec<Voice> {
    keys.voiceprints
        .iter()
        .filter_map(|v| {
            let print = naming::mean_print(&v.embeddings())?;
            let name = v
                .contact_id
                .as_deref()
                .and_then(|id| contacts.iter().find(|c| c.id == id))
                .map(|c| c.display_name.as_str())
                .unwrap_or(v.display_name.as_str());
            Some(Voice { id: v.id.clone(), display_name: v.display_name.clone(), is_me: fold(name) == "me", print })
        })
        .collect()
}

/// Starts the worker for one track. A missing model writes the overlay with
/// `failure: "not_downloaded"` and starts nothing; captions stay unlabelled.
pub fn start(params: Params, source: Box<dyn Source>, emit: Emit) -> Handle {
    let ctl = Arc::new(Ctl::default());
    if !nemotron3_diar::model_present(&params.model_dir) {
        ctl.write(&params, &Overlay::failed(nemotron3_diar::MODEL_NAME, "not_downloaded"));
        *ctl.ended.lock().unwrap_or_else(|e| e.into_inner()) = true;
        return Handle { ctl, join: None };
    }
    let worker_ctl = ctl.clone();
    let join = std::thread::Builder::new()
        .name(format!("live-names-{}", params.track.as_str()))
        .spawn(move || {
            let _ended = Ended(worker_ctl.clone());
            run(params, source, emit, &worker_ctl)
        })
        .map_err(|e| log::warn!("live speakers: spawn worker: {e}"))
        .ok();
    if join.is_none() {
        *ctl.ended.lock().unwrap_or_else(|e| e.into_inner()) = true;
    }
    Handle { ctl, join }
}

fn run(params: Params, mut source: Box<dyn Source>, mut emit: Emit, ctl: &Ctl) {
    transcript::cpu::lower_to_background_priority();
    let mut emit = |mut u: LiveSpeakers| {
        u.session_id.clone_from(&params.session_id);
        emit(&u);
    };
    let loaded = Instant::now();
    let diar = match LiveDiarizer::load(&params.model_dir, DIAR_THREADS) {
        Ok(d) => d,
        Err(e) => {
            log::warn!("live speakers: load diarizer: {e:#}");
            ctl.write(&params, &Overlay::failed(nemotron3_diar::MODEL_NAME, "load_failed"));
            return;
        }
    };
    let load_seconds = loaded.elapsed().as_secs_f64();
    let has_voices = !params.voices.lock().unwrap_or_else(|e| e.into_inner()).is_empty();
    let embedder = if !has_voices || ctl.aborted() {
        None
    } else {
        voiceprints::Encoder::load_background()
            .map_err(|e| log::warn!("live speakers: load speaker encoder: {e}; slots stay unnamed"))
            .ok()
    };
    let mut engine = Engine::new(params.track.as_str(), diar, embedder, params.voices.clone(), Some(load_seconds));
    let mut last_write_s = 0.0;
    loop {
        if ctl.aborted() {
            return;
        }
        if let Some(u) = engine.forget_names_without_voices() {
            emit(u);
        }
        let finishing = ctl.done.load(Ordering::SeqCst);
        let mut got = false;
        let mut failed = false;
        let polled = source.poll(finishing, &mut |p: Pcm<'_>| {
            if failed || ctl.aborted() {
                return;
            }
            got = true;
            match engine.push(p.offset_ms, p.start_sample, p.samples) {
                Ok(Some(u)) => emit(u),
                Ok(None) => {}
                Err(e) => {
                    log::warn!("live speakers: step failed: {e:#}");
                    failed = true;
                }
            }
        });
        if let Err(e) = polled {
            log::warn!("live speakers: read audio: {e}");
        }
        if failed {
            ctl.write(&params, &engine.overlay(nemotron3_diar::MODEL_NAME, Some("load_failed")));
            return;
        }
        let audio_s = engine.overlay_audio_seconds();
        if audio_s - last_write_s >= WRITE_EVERY_S {
            last_write_s = audio_s;
            ctl.write(&params, &engine.overlay(nemotron3_diar::MODEL_NAME, None));
        }
        if finishing && !got {
            break;
        }
        if !got {
            std::thread::sleep(IDLE);
        }
    }
    if ctl.aborted() {
        return;
    }
    match engine.finish() {
        Ok(Some(u)) => emit(u),
        Ok(None) => {}
        Err(e) => log::warn!("live speakers: finish: {e:#}"),
    }
    ctl.write(&params, &engine.overlay(nemotron3_diar::MODEL_NAME, None));
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Nothing;
    impl Source for Nothing {
        fn poll(&mut self, _: bool, _: &mut dyn FnMut(Pcm<'_>)) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_missing_model_writes_not_downloaded_and_starts_no_thread() {
        let td = tempfile::tempdir().unwrap();
        let params = Params {
            session_id: "daisy-1".into(),
            session_root: td.path().to_path_buf(),
            track: Track::System,
            model_dir: td.path().join("absent"),
            voices: Default::default(),
        };
        let h = start(params, Box::new(Nothing), Box::new(|_| {}));
        assert!(h.join.is_none());
        h.stop();
        let o = overlay::read(td.path(), Track::System).unwrap();
        assert_eq!(o.failure.as_deref(), Some("not_downloaded"));
    }

    /// Delivers `total` seconds of tone in one-second pieces, then nothing.
    struct Tone {
        left: u32,
        sent: u32,
    }
    impl Source for Tone {
        fn poll(&mut self, _: bool, on: &mut dyn FnMut(Pcm<'_>)) -> std::io::Result<()> {
            while self.left > 0 {
                let s: Vec<f32> = (0..16_000).map(|i| 0.1 * (i as f32 * 0.05).sin()).collect();
                on(Pcm { offset_ms: self.sent * 1000, start_sample: 0, samples: &s });
                self.left -= 1;
                self.sent += 1;
            }
            Ok(())
        }
    }

    /// Needs the exported graph; runs only when `NEMOTRON3_DIAR_DIR` names it.
    #[test]
    fn a_worker_streams_a_source_to_a_final_overlay() {
        let Ok(dir) = std::env::var("NEMOTRON3_DIAR_DIR") else { return };
        let td = tempfile::tempdir().unwrap();
        let params = Params {
            session_id: "daisy-1".into(),
            session_root: td.path().to_path_buf(),
            track: Track::System,
            model_dir: PathBuf::from(dir),
            voices: Default::default(),
        };
        let h = start(params, Box::new(Tone { left: 6, sent: 0 }), Box::new(|_| {}));
        h.stop();
        let o = overlay::read(td.path(), Track::System).unwrap();
        assert_eq!(o.failure, None);
        assert!((o.audio_seconds - 6.0).abs() < 0.01, "{}", o.audio_seconds);
        assert_eq!(o.model, "nemotron3/fast32");
    }

    #[test]
    fn the_owner_is_the_voice_under_the_contact_named_me() {
        use crate::state::Voiceprint;
        let mut vp: Voiceprint = serde_json::from_value(serde_json::json!({
            "id": "v1", "display_name": "Dana Whitfield", "vector": [], "vectors": [],
            "samples": [], "created_at_unix_seconds": 0, "session_ids": [], "contact_id": "c1"
        }))
        .unwrap();
        vp.samples.push(crate::sync::voices::Sample {
            key: Default::default(),
            model: crate::sync::voices::MODEL.into(),
            v: {
                use base64::{engine::general_purpose::STANDARD as B64, Engine};
                let bytes: Vec<u8> = [1.0f32, 0.0].iter().flat_map(|f| f.to_le_bytes()).collect();
                B64.encode(bytes)
            },
            at: 0,
            extra: Default::default(),
        });
        let mut keys = DecryptedKeys::default();
        keys.voiceprints.push(vp);
        let me = Contact {
            id: "c1".into(),
            display_name: " me ".into(),
            emails: vec![],
            created_at_unix_seconds: 0,
            updated_at_unix_seconds: None,
            extra: Default::default(),
        };
        let v = known_voices(&keys, &[me]);
        assert_eq!(v.len(), 1);
        assert!(v[0].is_me);
        assert_eq!(v[0].display_name, "Dana Whitfield");
    }

    #[test]
    fn an_aborted_worker_writes_nothing_and_a_wait_is_bounded() {
        let td = tempfile::tempdir().unwrap();
        let params = Params {
            session_id: "daisy-1".into(),
            session_root: td.path().to_path_buf(),
            track: Track::System,
            model_dir: td.path().join("absent"),
            voices: Default::default(),
        };
        let ctl = Ctl::default();
        let h = Handle { ctl: Arc::new(ctl), join: None };
        h.abort();
        h.ctl.write(&params, &Overlay::failed("m", "x"));
        assert!(overlay::read(td.path(), Track::System).is_none());
        let mut h = h;
        let t = Instant::now();
        assert!(!h.wait(t + Duration::from_millis(50)));
        assert!(t.elapsed() < Duration::from_secs(2));
    }

    /// Delivers audio forever, slowly, and counts the polls.
    struct Endless(Arc<std::sync::atomic::AtomicU32>);
    impl Source for Endless {
        fn poll(&mut self, _: bool, _: &mut dyn FnMut(Pcm<'_>)) -> std::io::Result<()> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    #[test]
    fn a_worker_ends_when_its_thread_returns_and_the_wait_joins_it() {
        let ctl = Arc::new(Ctl::default());
        let polls = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let worker_ctl = ctl.clone();
        let mut src = Endless(polls.clone());
        let join = std::thread::spawn(move || {
            let _ended = Ended(worker_ctl.clone());
            while !worker_ctl.aborted() {
                let _ = src.poll(false, &mut |_| {});
                std::thread::sleep(Duration::from_millis(5));
            }
        });
        let mut h = Handle { ctl, join: Some(join) };
        assert!(!h.wait(Instant::now() + Duration::from_millis(30)));
        h.abort();
        assert!(h.wait(Instant::now() + Duration::from_secs(5)));
        assert!(h.join.is_none());
        assert!(polls.load(Ordering::SeqCst) > 0);
    }
}
