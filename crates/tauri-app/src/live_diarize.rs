// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Speaker turns of the system track, found while the recording runs.
//!
//! One worker per recording reads `chunks/NNNN/system.wav` as it grows, segments each full window, resolves a chunk's turns when it closes and embeds turns once their audio is final.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use voiceprints::segmentation::{SegmentationModel, StreamSegmenter};
use voiceprints::turns::Turns;
use voiceprints::Encoder;

use crate::commands::voiceprints::{embed_turn_audio, turn_embed_key, turn_ms, SuppliedTurns, TurnKey};
use crate::live_audio::{ChunkFeed, FeedEvent, FeedTrack};

/// How often the worker looks for new audio while the recording runs.
const POLL: Duration = Duration::from_secs(2);

/// Window step, as finalize runs the segmentation model.
const STEP_S: f32 = 1.0;

/// What the live pass found for one session's system track.
#[derive(Debug, Default)]
pub struct LiveTurns {
    /// Per chunk index: the chunk's turns and the frames its WAV held.
    pub chunks: BTreeMap<u32, (Turns, u64)>,
    /// Embeddings of turns whose audio was final, by [`TurnKey`].
    pub embeddings: HashMap<TurnKey, Vec<f32>>,
}

impl LiveTurns {
    /// True when every chunk of the session's manifest that has a system
    /// WAV was read whole and the WAV still holds exactly those frames.
    pub fn matches_disk(&self, root: &Path) -> bool {
        let Ok(bytes) = syncsafe::read(root.join("manifest.json")) else { return false };
        let Ok(m) = serde_json::from_slice::<recording::manifest::SessionManifest>(&bytes) else { return false };
        m.chunks.iter().all(|c| {
            let wav = syncsafe::join_rel(root, &c.system_wav_relative);
            match (crate::live_audio::wav_frames(&wav), self.chunks.get(&c.index)) {
                (None, _) => true,
                (Some(n), Some((_, read))) => *read == n,
                (Some(_), None) => false,
            }
        })
    }

    /// The turns in the form finalize's diarization takes; the mic side, when
    /// diarized, comes from the segmentation model.
    pub fn supplied(self) -> SuppliedTurns {
        let mut s = SuppliedTurns { segment_mic: true, system_embeddings: self.embeddings, ..Default::default() };
        for (index, (turns, _)) in self.chunks {
            s.system.insert(index, turns.spans);
            s.system_overlaps.insert(index, turns.overlap_spans);
        }
        s
    }
}

struct Worker {
    done: Arc<AtomicBool>,
    cancel: Arc<AtomicBool>,
    thread: std::thread::JoinHandle<Option<LiveTurns>>,
}

fn workers() -> std::sync::MutexGuard<'static, HashMap<String, Worker>> {
    static LIVE: std::sync::LazyLock<Mutex<HashMap<String, Worker>>> = std::sync::LazyLock::new(Default::default);
    LIVE.lock().unwrap_or_else(|e| e.into_inner())
}

/// Starts the live pass for the recording at `root`. A pass already running
/// for the session is abandoned first.
pub fn start(session_id: &str, root: PathBuf) {
    cancel(session_id);
    let done = Arc::new(AtomicBool::new(false));
    let cancel_flag = Arc::new(AtomicBool::new(false));
    let (d, c) = (done.clone(), cancel_flag.clone());
    let sid = session_id.to_string();
    let spawned = std::thread::Builder::new().name("live-diarize".into()).spawn(move || {
        transcript::cpu::lower_to_background_priority();
        let r = run(&root, &d, &c);
        if r.is_none() && !c.load(Ordering::Relaxed) {
            log::info!("live diarize {sid}: no live turns; finalize segments the system track");
        }
        r
    });
    match spawned {
        Ok(thread) => {
            workers().insert(session_id.to_string(), Worker { done, cancel: cancel_flag, thread });
        }
        Err(e) => log::warn!("live diarize {session_id}: worker did not start: {e}"),
    }
}

/// The recorder has stopped writing: the worker reads what is left and
/// finishes, without waiting for finalize to ask.
pub fn recording_stopped(session_id: &str) {
    if let Some(w) = workers().get(session_id) {
        w.done.store(true, Ordering::Relaxed);
        w.thread.thread().unpark();
    }
}

/// How long [`cancel`] waits for the worker to return.
const CANCEL_WAIT: Duration = Duration::from_secs(5);
/// How long [`take`] waits for the worker to read the rest.
const TAKE_WAIT: Duration = Duration::from_secs(120);

impl Worker {
    /// Waits until the thread returns or `until` passes; its result once it
    /// has returned, else the worker back.
    fn join_by(self, until: std::time::Instant) -> Result<Option<LiveTurns>, Worker> {
        while !self.thread.is_finished() {
            if std::time::Instant::now() >= until {
                return Err(self);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        Ok(self.thread.join().ok().flatten())
    }

    fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
        self.thread.thread().unpark();
    }
}

/// Abandons the live pass; its results are dropped. Waits up to five
/// seconds for the worker to let go of the session's files.
pub fn cancel(session_id: &str) {
    let Some(w) = workers().remove(session_id) else { return };
    w.cancel();
    if w.join_by(std::time::Instant::now() + CANCEL_WAIT).is_err() {
        log::warn!("live diarize {session_id}: worker still running after cancel");
    }
}

/// True when the session's live pass has returned, or none is running.
pub fn is_finished(session_id: &str) -> bool {
    workers().get(session_id).is_none_or(|w| w.thread.is_finished())
}

/// App exit: every live pass is cancelled and given until `limit` to return.
pub fn shutdown(limit: Duration) {
    let all: Vec<Worker> = workers().drain().map(|(_, w)| w).collect();
    let until = std::time::Instant::now() + limit;
    for w in &all {
        w.cancel();
    }
    for w in all {
        let _ = w.join_by(until);
    }
}

/// Waits for the session's live pass to finish and returns its turns when
/// they still describe the audio on disk. `None` when no pass ran, it failed,
/// it did not finish within two minutes (it is then cancelled), or a chunk
/// changed after it was read.
pub fn take(session_id: &str, root: &Path) -> Option<SuppliedTurns> {
    take_within(session_id, root, TAKE_WAIT)
}

fn take_within(session_id: &str, root: &Path, limit: Duration) -> Option<SuppliedTurns> {
    let w = workers().remove(session_id)?;
    w.done.store(true, Ordering::Relaxed);
    w.thread.thread().unpark();
    let live = match w.join_by(std::time::Instant::now() + limit) {
        Ok(r) => r?,
        Err(w) => {
            log::warn!("live diarize {session_id}: not finished after {limit:?}; finalize segments the system track");
            w.cancel();
            return None;
        }
    };
    if !live.matches_disk(root) {
        log::warn!("live diarize {session_id}: chunk audio changed after the live pass; it is not used");
        return None;
    }
    Some(live.supplied())
}

/// The worker body: polls until `done`, then reads the rest and returns.
/// `None` when a model is missing or fails, or `cancel` is set.
pub fn run(root: &Path, done: &AtomicBool, cancel: &AtomicBool) -> Option<LiveTurns> {
    let mut model = match SegmentationModel::load_background() {
        Ok(m) => m,
        Err(e) => {
            log::warn!("live diarize: segmentation model: {e}");
            return None;
        }
    };
    let mut encoder = match Encoder::load_background() {
        Ok(e) => e,
        Err(e) => {
            log::warn!("live diarize: speaker encoder: {e}");
            return None;
        }
    };
    let mut feed = ChunkFeed::new(root.to_path_buf(), FeedTrack::System);
    let mut open: BTreeMap<u32, StreamSegmenter> = BTreeMap::new();
    let mut out = LiveTurns::default();
    // Keys already tried, embedded or not.
    let mut tried: std::collections::HashSet<TurnKey> = Default::default();
    loop {
        if cancel.load(Ordering::Relaxed) {
            return None;
        }
        let finishing = done.load(Ordering::Relaxed);
        let mut failed: Option<voiceprints::VpError> = None;
        let polled = feed.poll(finishing, &mut |e| {
            if failed.is_some() || cancel.load(Ordering::Relaxed) {
                return;
            }
            match e {
                FeedEvent::Samples { chunk, samples, .. } => {
                    let seg = open.entry(chunk).or_insert_with(|| StreamSegmenter::new(STEP_S));
                    if let Err(e) = seg.push(&mut model, samples) {
                        failed = Some(e);
                    }
                }
                FeedEvent::Closed { chunk, samples, .. } => {
                    let seg = open.remove(&chunk).unwrap_or_else(|| StreamSegmenter::new(STEP_S));
                    let audio_s = samples as f64 / crate::live_audio::SAMPLE_RATE as f64;
                    match seg.finish(&mut model, audio_s) {
                        Ok(t) => {
                            out.chunks.insert(chunk, (t, samples));
                        }
                        Err(e) => failed = Some(e),
                    }
                }
            }
        });
        if let Some(e) = failed {
            log::warn!("live diarize: segmentation failed: {e}");
            return None;
        }
        if let Err(e) = polled {
            if finishing {
                log::warn!("live diarize: reading the recording failed at Stop: {e}");
                return None;
            }
            log::debug!("live diarize: poll: {e}");
        }

        // Embed every turn whose audio is final: all turns of closed chunks,
        // the settled ones of the open chunk.
        let mut pending: Vec<(u32, (f64, f64))> = Vec::new();
        for (&chunk, (turns, _)) in &out.chunks {
            pending.extend(turns.spans.iter().map(|&s| (chunk, s)));
        }
        for (&chunk, seg) in &open {
            pending.extend(seg.provisional().spans.into_iter().map(|s| (chunk, s)));
        }
        for (chunk, (s, e)) in pending {
            if cancel.load(Ordering::Relaxed) {
                return None;
            }
            let (start_ms, end_ms) = turn_ms(s, e);
            let Some(key) = turn_embed_key(chunk, start_ms, end_ms) else { continue };
            if !tried.insert(key) {
                continue;
            }
            if let Some(emb) = embed_turn_audio(&feed_wav(root, chunk), start_ms, key.2, &mut encoder) {
                out.embeddings.insert(key, emb);
            }
        }

        if finishing && feed.all_closed() {
            return Some(out);
        }
        if !finishing {
            std::thread::park_timeout(POLL);
        }
    }
}

/// The system WAV of chunk `chunk`, as the manifest names it.
fn feed_wav(root: &Path, chunk: u32) -> PathBuf {
    root.join(recording::session::chunk_rel(chunk, "system.wav"))
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;

    fn insert(session_id: &str, body: impl FnOnce(Arc<AtomicBool>) -> Option<LiveTurns> + Send + 'static) {
        let done = Arc::new(AtomicBool::new(false));
        let cancel = Arc::new(AtomicBool::new(false));
        let c = cancel.clone();
        let thread = std::thread::spawn(move || body(c));
        workers().insert(session_id.into(), Worker { done, cancel, thread });
    }

    #[test]
    fn a_take_past_its_deadline_cancels_the_pass_and_returns_none() {
        let td = tempfile::tempdir().unwrap();
        insert("daisy-test-slow", |cancel| {
            while !cancel.load(Ordering::Relaxed) {
                std::thread::park_timeout(Duration::from_millis(10));
            }
            Some(LiveTurns::default())
        });
        let t = std::time::Instant::now();
        assert!(take_within("daisy-test-slow", td.path(), Duration::from_millis(100)).is_none());
        assert!(t.elapsed() < Duration::from_secs(3));
        assert!(!workers().contains_key("daisy-test-slow"));
    }

    #[test]
    fn a_cancel_waits_for_the_worker_to_return() {
        let returned = Arc::new(AtomicBool::new(false));
        let r = returned.clone();
        insert("daisy-test-cancel", move |cancel| {
            while !cancel.load(Ordering::Relaxed) {
                std::thread::park_timeout(Duration::from_millis(10));
            }
            r.store(true, Ordering::SeqCst);
            None
        });
        cancel("daisy-test-cancel");
        assert!(returned.load(Ordering::SeqCst));
    }
}

#[cfg(test)]
mod ab {
    //! The live handoff against Stop-only diarization on recorded sessions.
    //!
    //!   DAISY_AB_SESSIONS=/profile/sessions/<a>:/profile/sessions/<b> \
    //!   DAISY_AB_OUT=/scratch/ab.json [DAISY_AB_WORK=/scratch] \
    //!   DAISY_VOICEPRINT_DIR=/dir/with/model.onnx+segmentation-3.0.onnx \
    //!   cargo test --release -p tauri-app --lib live_diarize::ab -- --ignored --nocapture
    //!
    //! Each session is copied twice (sessions with only `meeting.opus` are
    //! rebuilt into one chunk first). Copy A runs finalize's diarization as
    //! shipped; copy B runs the live pass over the finished chunks, then
    //! diarization on its turns. Reports per session the time-weighted label
    //! agreement (one-to-one mapping) and NMI of B against A, and the wall
    //! seconds of A, of B's live pass and of B's diarization.
    use std::collections::HashMap;
    use std::path::Path;
    use std::sync::atomic::AtomicBool;
    use std::time::Instant;

    use crate::profile::ProfileDir;
    use crate::state::AppState;

    type Seg = (u32, transcript::Track, u32, u32, Option<u32>);
    type Table = HashMap<(Option<u32>, Option<u32>), u64>;

    fn copy_session(src: &Path, dst: &Path) {
        std::fs::create_dir_all(dst).unwrap();
        for e in std::fs::read_dir(src).unwrap() {
            let e = e.unwrap();
            let to = dst.join(e.file_name());
            if e.file_type().unwrap().is_dir() {
                copy_session(&e.path(), &to);
            } else {
                std::fs::copy(e.path(), &to).unwrap();
            }
        }
    }

    fn segments(root: &Path) -> Vec<Seg> {
        let p = crate::commands::session_store::preferred_transcript_path(root).unwrap();
        let st: transcript::SessionTranscript = serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap();
        st.chunks
            .iter()
            .flat_map(|c| {
                c.tracks.iter().flat_map(move |t| {
                    t.segments.iter().map(move |s| (c.chunk_index, t.track, s.start_ms, s.end_ms, s.speaker_id))
                })
            })
            .collect()
    }

    /// Milliseconds of overlap per (label in `a`, label in `b`).
    fn contingency(a: &[Seg], b: &[Seg]) -> Table {
        let mut m = HashMap::new();
        for x in a {
            for y in b.iter().filter(|y| y.0 == x.0 && y.1 == x.1) {
                let o = x.3.min(y.3).saturating_sub(x.2.max(y.2)) as u64;
                if o > 0 {
                    *m.entry((x.4, y.4)).or_insert(0) += o;
                }
            }
        }
        m
    }

    fn agreement(c: &Table) -> f64 {
        let total: u64 = c.values().sum();
        let mut pairs: Vec<_> = c.iter().collect();
        pairs.sort_by(|x, y| y.1.cmp(x.1));
        let (mut ua, mut ub, mut hit) = (Vec::new(), Vec::new(), 0u64);
        for ((a, b), w) in pairs {
            if !ua.contains(a) && !ub.contains(b) {
                ua.push(*a);
                ub.push(*b);
                hit += w;
            }
        }
        if total == 0 {
            1.0
        } else {
            hit as f64 / total as f64
        }
    }

    fn nmi(c: &Table) -> f64 {
        let n: f64 = c.values().sum::<u64>() as f64;
        if n == 0.0 {
            return 1.0;
        }
        let (mut pa, mut pb): (HashMap<Option<u32>, f64>, HashMap<Option<u32>, f64>) = Default::default();
        for ((a, b), w) in c {
            *pa.entry(*a).or_insert(0.0) += *w as f64 / n;
            *pb.entry(*b).or_insert(0.0) += *w as f64 / n;
        }
        let h = |p: &HashMap<Option<u32>, f64>| -p.values().map(|v| v * v.ln()).sum::<f64>();
        let mi: f64 = c
            .iter()
            .map(|((a, b), w)| {
                let p = *w as f64 / n;
                p * (p / (pa[a] * pb[b])).ln()
            })
            .sum();
        let (ha, hb) = (h(&pa), h(&pb));
        if ha + hb == 0.0 {
            1.0
        } else {
            2.0 * mi / (ha + hb)
        }
    }

    #[test]
    #[ignore]
    fn live_ab() {
        let _ = env_logger::builder().is_test(true).try_init();
        let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        let sessions = var("DAISY_AB_SESSIONS").expect("DAISY_AB_SESSIONS");
        let out = std::path::PathBuf::from(var("DAISY_AB_OUT").expect("DAISY_AB_OUT"));
        let work = var("DAISY_AB_WORK").map(std::path::PathBuf::from).unwrap_or_else(std::env::temp_dir);
        let mut report = Vec::new();
        for src in sessions.split(':').map(std::path::PathBuf::from) {
            let sid = src.file_name().and_then(|s| s.to_str()).unwrap().to_string();
            let scratch = tempfile::Builder::new().prefix("live-ab-").tempdir_in(&work).unwrap();
            let side = |name: &str| -> (AppState, std::path::PathBuf) {
                let app = AppState::new(ProfileDir::at(scratch.path().join(name)).unwrap());
                let root = app.profile.session_path(&sid);
                copy_session(&src, &root);
                crate::commands::pipeline::rehydrate_from_opus(&root).unwrap();
                (app, root)
            };
            let (a_app, a_root) = side("a");
            let (b_app, b_root) = side("b");

            let t = Instant::now();
            let a = crate::commands::voiceprints::diarize_session_impl(&a_app, &sid, None, None, &[]);
            let a_s = t.elapsed().as_secs_f64();

            let t = Instant::now();
            let live = super::run(&b_root, &AtomicBool::new(true), &AtomicBool::new(false));
            let live_s = t.elapsed().as_secs_f64();
            let live = live.filter(|l| l.matches_disk(&b_root)).map(super::LiveTurns::supplied);
            let t = Instant::now();
            let b = match &live {
                Some(turns) => {
                    crate::commands::voiceprints::diarize_session_with_turns_impl(&b_app, &sid, None, None, &[], turns)
                }
                None => crate::commands::voiceprints::diarize_session_impl(&b_app, &sid, None, None, &[]),
            };
            let b_s = t.elapsed().as_secs_f64();

            let (sa, sb) = (segments(&a_root), segments(&b_root));
            let c = contingency(&sa, &sb);
            let row = serde_json::json!({
                "session": sid,
                "a_speakers": a.as_ref().ok().map(|r| r.speakers),
                "b_speakers": b.as_ref().ok().map(|r| r.speakers),
                "a_error": a.as_ref().err().map(|e| e.to_string()),
                "b_error": b.as_ref().err().map(|e| e.to_string()),
                "live_used": live.is_some(),
                "identical": sa == sb,
                "segments": sa.len(),
                "agreement": agreement(&c),
                "nmi": nmi(&c),
                "a_diarize_s": a_s,
                "b_live_pass_s": live_s,
                "b_diarize_s": b_s,
            });
            eprintln!("live_ab: {row}");
            report.push(row);
            std::fs::write(&out, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
        }
    }
}
