// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Live Nemotron backend implementing `RealtimeTranscriber`. Each `run`
//! (one per track) loads its own model and decodes one continuous stream as
//! audio arrives. Closed words are final; the open utterance plus the open
//! word is the interim line; utterances close by the shared rule in
//! `streaming::emit`.

use std::path::PathBuf;
use std::time::Instant;

use providers_realtime::{EngineStat, RealtimeError, RealtimeEvent, RealtimeTranscriber, Result};
use tokio::sync::mpsc;
use transcript::Segment;

use super::features::{CHUNK_LOOKAHEAD_SAMPLES, CHUNK_MS, CHUNK_SAMPLES, SAMPLE_RATE};
use super::model::NemotronModel;
use super::stream::{NemotronStream, TimedToken};
use super::words::WordAssembler;
use super::{NemotronError, NemotronOpts};
use crate::streaming::agreement::StreamToken;
use crate::streaming::emit::{
    emit_final, extend_samples_f32, interim_event, samples_to_f32, FallingBehindGate, Utterances,
    FINAL_GAP_SPLIT_MS,
};
use crate::streaming::live_metrics;

/// Queued audio above this in one drain counts as behind: four chunks.
pub const BEHIND_BACKLOG_MS: i64 = 4 * CHUNK_MS;

pub struct NemotronRealtime {
    model_dir: PathBuf,
    label: String,
}

impl NemotronRealtime {
    pub fn new(model_dir: PathBuf, label: impl Into<String>) -> Self {
        Self { model_dir, label: label.into() }
    }
}

/// Tokens to words to utterances. Every method returns the utterances it
/// closed, oldest first.
#[derive(Default)]
struct Speech {
    words: WordAssembler,
    utts: Utterances,
}

impl Speech {
    fn feed(&mut self, tokens: &[TimedToken]) -> Vec<Vec<StreamToken>> {
        let mut closed = Vec::new();
        for t in tokens {
            if let Some(w) = self.words.push(t) {
                closed.extend(self.utts.push(w));
            }
        }
        closed
    }

    /// Closes what the stream clock `now_ms` has left behind: the open word
    /// joins its utterance first, so the last word of an utterance is never
    /// split into a Final of its own. `end` closes everything.
    fn settle(&mut self, now_ms: i64, end: bool) -> Vec<Vec<StreamToken>> {
        let mut closed = Vec::new();
        if let (Some(p), Some(l)) = (self.words.pending(), self.utts.open().last()) {
            if p.start_ms - l.end_ms >= FINAL_GAP_SPLIT_MS {
                closed.extend(self.utts.close());
            }
        }
        let word_done = self.words.pending().is_some_and(|w| now_ms - w.end_ms >= FINAL_GAP_SPLIT_MS);
        if end || word_done {
            if let Some(w) = self.words.take() {
                closed.extend(self.utts.push(w));
            }
        }
        let quiet = if end {
            self.utts.close()
        } else if self.words.pending().is_some() {
            None
        } else {
            self.utts.close_if_quiet(now_ms)
        };
        closed.extend(quiet);
        closed
    }

    /// Open utterance plus open word: the interim line.
    fn tail(&self) -> Vec<StreamToken> {
        self.utts.open().iter().cloned().chain(self.words.pending().cloned()).collect()
    }

    /// Closes the open word into its utterance after a decode failure; the
    /// utterance closes through `settle` as the clock advances.
    fn seal_word(&mut self) -> Vec<Vec<StreamToken>> {
        match self.words.take() {
            Some(w) => self.utts.push(w).into_iter().collect(),
            None => Vec::new(),
        }
    }
}

/// Consecutive failed chunks after which the run gives up and returns an error.
const MAX_CONSECUTIVE_DECODE_FAILURES: u32 = 3;

/// One continuous decode stream; a fresh one replaces it after an error.
trait Decoder: Send + 'static {
    fn push(&mut self, pcm: &[f32]) -> std::result::Result<Vec<TimedToken>, NemotronError>;
    fn finish(&mut self) -> std::result::Result<Vec<TimedToken>, NemotronError>;
    fn decoded_ms(&self) -> i64;
    /// Drops the stream for a fresh one.
    fn restart(&mut self);
}

struct ModelDecoder {
    model: NemotronModel,
    stream: NemotronStream,
}

impl Decoder for ModelDecoder {
    fn push(&mut self, pcm: &[f32]) -> std::result::Result<Vec<TimedToken>, NemotronError> {
        self.stream.push(&mut self.model, pcm)
    }
    fn finish(&mut self) -> std::result::Result<Vec<TimedToken>, NemotronError> {
        self.stream.finish(&mut self.model)
    }
    fn decoded_ms(&self) -> i64 {
        self.stream.decoded_ms()
    }
    fn restart(&mut self) {
        self.stream = NemotronStream::new();
    }
}

/// What one decoded block produced, in order.
enum Piece {
    Tokens(Vec<TimedToken>),
    /// The stream failed on one chunk and was replaced.
    Restart(String),
}

/// One `Session::decode` call's output.
struct Decoded {
    pieces: Vec<Piece>,
    /// Wall-clock ms of each chunk the stream decoded, in order.
    chunk_ms: Vec<u64>,
}

/// A decoder fed one chunk per push, each push cut where the mel front end
/// completes a chunk, with token times kept on the audio clock across
/// restarts.
struct Session<D: Decoder> {
    dec: D,
    /// Samples waiting for the next cut.
    carry: Vec<f32>,
    /// Audio-clock sample at which the current stream starts.
    origin: usize,
    /// Samples handed to the current stream.
    fed: usize,
    failures: u32,
}

impl<D: Decoder> Session<D> {
    fn new(dec: D) -> Self {
        Self { dec, carry: Vec::new(), origin: 0, fed: 0, failures: 0 }
    }

    fn now_ms(&self) -> i64 {
        (self.origin * 1000 / SAMPLE_RATE) as i64 + self.dec.decoded_ms()
    }

    /// Samples the current stream needs for its next chunk: the first push
    /// also carries the front end's lookahead.
    fn next_cut(&self) -> usize {
        if self.fed == 0 {
            CHUNK_SAMPLES + CHUNK_LOOKAHEAD_SAMPLES
        } else {
            CHUNK_SAMPLES
        }
    }

    fn buffer(&mut self, pcm: &[f32]) {
        self.carry.extend_from_slice(pcm);
    }

    /// The carry holds at least one cut.
    fn ready(&self) -> bool {
        self.carry.len() >= self.next_cut()
    }

    /// Handles one push's result. `regular` marks a push cut at a chunk
    /// boundary; `took` is its wall-clock cost and `before` the stream's
    /// decoded ms ahead of it. Returns how many trailing pushed samples go
    /// back to the carry.
    fn step(
        &mut self,
        result: std::result::Result<Vec<TimedToken>, NemotronError>,
        took: u64,
        before: i64,
        regular: bool,
        out: &mut Decoded,
    ) -> std::result::Result<usize, String> {
        match result {
            Ok(mut tokens) => {
                self.failures = 0;
                let origin_ms = (self.origin * 1000 / SAMPLE_RATE) as i64;
                for t in &mut tokens {
                    t.ms += origin_ms;
                }
                let chunks = ((self.dec.decoded_ms() - before) / CHUNK_MS).max(0) as u64;
                for _ in 0..chunks {
                    out.chunk_ms.push(took / chunks);
                }
                out.pieces.push(Piece::Tokens(tokens));
                Ok(0)
            }
            Err(e) => {
                self.failures += 1;
                log::warn!("nemotron live: decode failed ({} in a row): {e}", self.failures);
                if self.failures >= MAX_CONSECUTIVE_DECODE_FAILURES {
                    return Err(format!("decode failed {} times in a row: {e}", self.failures));
                }
                self.dec.restart();
                let back = if regular { CHUNK_LOOKAHEAD_SAMPLES } else { 0 };
                self.origin += self.fed - back;
                self.fed = 0;
                out.pieces.push(Piece::Restart(e.to_string()));
                Ok(back)
            }
        }
    }

    /// Decodes every whole cut in the carry, one chunk each; `end` also
    /// decodes the remainder and flushes. A failing chunk is the only audio
    /// lost.
    fn decode(&mut self, end: bool) -> std::result::Result<Decoded, String> {
        let mut out = Decoded { pieces: Vec::new(), chunk_ms: Vec::new() };
        let mut at = 0;
        while self.carry.len() - at >= self.next_cut() {
            let cut = self.next_cut();
            let before = self.dec.decoded_ms();
            let started = Instant::now();
            let r = self.dec.push(&self.carry[at..at + cut]);
            let took = started.elapsed().as_millis() as u64;
            at += cut;
            self.fed += cut;
            at -= self.step(r, took, before, true, &mut out)?;
        }
        self.carry.drain(..at);
        if end {
            let rest = std::mem::take(&mut self.carry);
            if !rest.is_empty() {
                let before = self.dec.decoded_ms();
                let started = Instant::now();
                let r = self.dec.push(&rest);
                self.fed += rest.len();
                self.step(r, started.elapsed().as_millis() as u64, before, false, &mut out)?;
            }
            let before = self.dec.decoded_ms();
            let started = Instant::now();
            let r = self.dec.finish();
            self.step(r, started.elapsed().as_millis() as u64, before, false, &mut out)?;
        }
        Ok(out)
    }
}

#[async_trait::async_trait]
impl RealtimeTranscriber for NemotronRealtime {
    fn name(&self) -> &'static str {
        "nemotron"
    }

    fn model(&self) -> &str {
        &self.label
    }

    async fn run(
        &self,
        _sample_rate: u32,
        audio_rx: mpsc::UnboundedReceiver<Vec<i16>>,
        events_tx: mpsc::Sender<RealtimeEvent>,
    ) -> Result<Vec<Segment>> {
        let dir = self.model_dir.clone();
        let model = tokio::task::spawn_blocking(move || NemotronModel::new(&dir, &NemotronOpts::live()))
            .await
            .map_err(|e| RealtimeError::Backend(format!("nemotron load join: {e}")))?
            .map_err(|e| RealtimeError::Backend(e.to_string()))?;
        let dec = ModelDecoder { model, stream: NemotronStream::new() };
        drive(dec, audio_rx, events_tx).await
    }
}

async fn drive<D: Decoder>(
    dec: D,
    mut audio_rx: mpsc::UnboundedReceiver<Vec<i16>>,
    events_tx: mpsc::Sender<RealtimeEvent>,
) -> Result<Vec<Segment>> {
    let mut session = Some(Session::new(dec));
    let mut speech = Speech::default();
    let mut accepted: Vec<Segment> = Vec::new();
    let mut falling = FallingBehindGate::new();
    let mut last_interim = String::new();
    let mut max_drained_ms = 0i64;

    loop {
        let (pcm, drained_ms, closed) = match audio_rx.recv().await {
            Some(frame) => {
                let mut pcm = samples_to_f32(frame);
                let mut drained_ms = 0i64;
                let mut drained = 0usize;
                while let Ok(more) = audio_rx.try_recv() {
                    drained_ms += (more.len() * 1000 / SAMPLE_RATE) as i64;
                    extend_samples_f32(&mut pcm, &more);
                    drained += 1;
                    if drained > 4096 {
                        break;
                    }
                }
                (pcm, drained_ms, false)
            }
            None => (Vec::new(), 0, true),
        };
        live_metrics::record_backlog(drained_ms as u64);
        max_drained_ms = max_drained_ms.max(drained_ms);
        let s = session.as_mut().expect("session is returned each turn");
        s.buffer(&pcm);
        if s.ready() || closed {
            let before_ms = s.now_ms();
            let mut s = session.take().expect("session is returned each turn");
            let (s, decoded) = tokio::task::spawn_blocking(move || {
                let r = s.decode(closed);
                (s, r)
            })
            .await
            .map_err(|e| RealtimeError::Backend(format!("nemotron decode join: {e}")))?;
            let decoded = decoded.map_err(|e| RealtimeError::Backend(format!("nemotron: {e}")))?;
            for ms in &decoded.chunk_ms {
                live_metrics::record_decode(*ms, CHUNK_MS as u64);
            }
            let now = s.now_ms();
            session = Some(s);
            if !decoded.chunk_ms.is_empty() {
                let _ = events_tx.try_send(RealtimeEvent::Stat(EngineStat::Decode {
                    decoded_ms: now.max(0) as u32,
                    step_ms: (now - before_ms).max(0) as u32,
                    cost_ms: decoded.chunk_ms.iter().map(|&ms| ms as u32).collect(),
                    backlog_ms: max_drained_ms.max(0) as u32,
                }));
                max_drained_ms = 0;
            }

            let mut utterances = Vec::new();
            for piece in decoded.pieces {
                match piece {
                    Piece::Tokens(tokens) => {
                        utterances.extend(speech.feed(&tokens));
                    }
                    Piece::Restart(e) => {
                        let _ = events_tx.send(RealtimeEvent::Error(format!("nemotron: {e}"))).await;
                        utterances.extend(speech.seal_word());
                    }
                }
            }
            utterances.extend(speech.settle(now, closed));
            for u in &utterances {
                emit_final(u, &events_tx, &mut accepted).await;
            }

            match interim_event(&speech.tail()) {
                Some(RealtimeEvent::Interim { segment }) if segment.text != last_interim => {
                    last_interim = segment.text.clone();
                    let _ = events_tx.send(RealtimeEvent::Interim { segment }).await;
                }
                Some(_) => {}
                None => last_interim.clear(),
            }
        }
        if falling.sustained(drained_ms > BEHIND_BACKLOG_MS) && falling.should_emit() {
            let _ = events_tx.send(RealtimeEvent::FallingBehind).await;
        }
        if closed {
            break;
        }
    }
    Ok(accepted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nemotron::features::MelStream;

    fn tok(text: &str, ms: i64) -> TimedToken {
        TimedToken { text: text.into(), ms }
    }

    fn texts(u: &[StreamToken]) -> Vec<&str> {
        u.iter().map(|w| w.text.as_str()).collect()
    }

    #[test]
    fn the_last_word_closes_in_the_same_final_as_its_utterance() {
        let mut s = Speech::default();
        assert!(s.feed(&[tok("▁see", 0), tok("▁you", 400), tok("▁later", 800)]).is_empty());
        assert!(s.settle(2_700, false).is_empty(), "an open recent word holds its utterance open");
        let closed = s.settle(2_880, false);
        assert_eq!(closed.len(), 1, "{closed:?}");
        assert_eq!(texts(&closed[0]), ["see", "you", "later"]);
        assert!(s.tail().is_empty());
    }

    #[test]
    fn the_stream_end_closes_the_open_word_with_its_utterance() {
        let mut s = Speech::default();
        s.feed(&[tok("▁thank", 0), tok("▁you", 400)]);
        let closed = s.settle(1_000, true);
        assert_eq!(closed.len(), 1);
        assert_eq!(texts(&closed[0]), ["thank", "you"]);
    }

    #[test]
    fn a_word_after_a_long_pause_opens_a_new_utterance() {
        let mut s = Speech::default();
        s.feed(&[tok("▁one", 0), tok("▁two", 400)]);
        s.feed(&[tok("▁three", 5_000)]);
        let closed = s.settle(5_100, false);
        assert_eq!(closed.len(), 1, "the earlier utterance closes without waiting for the new word");
        assert_eq!(texts(&closed[0]), ["one", "two"]);
        assert_eq!(texts(&s.tail()), ["three"]);
    }

    #[test]
    fn a_pending_word_that_appears_late_or_shrinks_only_moves_the_tail() {
        let mut s = Speech::default();
        s.feed(&[tok("▁so", 0), tok("▁um", 400)]);
        assert_eq!(texts(&s.tail()), ["so"], "an open filler stays hidden");
        s.feed(&[tok("▁yes", 800)]);
        assert_eq!(texts(&s.tail()), ["so", "yes"]);
        assert!(s.settle(1_000, false).is_empty());
    }

    /// Scripted decoder: call `n` answers `script(n)`; stream times restart
    /// at zero after `restart`.
    struct Scripted {
        calls: usize,
        chunks: i64,
        script: fn(usize) -> Option<Vec<TimedToken>>,
        tail: Vec<TimedToken>,
    }

    impl Decoder for Scripted {
        fn push(&mut self, _pcm: &[f32]) -> std::result::Result<Vec<TimedToken>, NemotronError> {
            let n = self.calls;
            self.calls += 1;
            match (self.script)(n) {
                Some(t) => {
                    self.chunks += 1;
                    Ok(t)
                }
                None => Err(NemotronError::Model("scripted failure".into())),
            }
        }
        fn finish(&mut self) -> std::result::Result<Vec<TimedToken>, NemotronError> {
            Ok(self.tail.clone())
        }
        fn decoded_ms(&self) -> i64 {
            self.chunks * CHUNK_MS
        }
        fn restart(&mut self) {
            self.chunks = 0;
        }
    }

    /// Serializes the tests that drive the process-global live metrics.
    static METRICS: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn run_scripted(script: fn(usize) -> Option<Vec<TimedToken>>, chunks: usize) -> (Result<Vec<Segment>>, Vec<RealtimeEvent>) {
        let _serial = METRICS.lock().unwrap_or_else(|e| e.into_inner());
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async {
            let (audio_tx, audio_rx) = mpsc::unbounded_channel();
            let (ev_tx, mut ev_rx) = mpsc::channel(256);
            for _ in 0..chunks {
                audio_tx.send(vec![0i16; CHUNK_SAMPLES]).unwrap();
                tokio::task::yield_now().await;
            }
            drop(audio_tx);
            let dec = Scripted { calls: 0, chunks: 0, script, tail: Vec::new() };
            let r = drive(dec, audio_rx, ev_tx).await;
            let mut events = Vec::new();
            while let Ok(e) = ev_rx.try_recv() {
                events.push(e);
            }
            (r, events)
        })
    }

    #[test]
    fn a_restart_keeps_times_monotonic_and_seals_the_open_word() {
        let (r, events) = run_scripted(
            |n| match n {
                0 => Some(vec![tok("▁hello", 0)]),
                1 => None,
                _ => Some(vec![tok("▁world", 0)]),
            },
            3,
        );
        let segs = r.expect("one failure does not end the run");
        assert_eq!(segs.len(), 1, "{segs:?}");
        assert_eq!(segs[0].text, "hello world", "the sealed word stays with its utterance");
        assert_eq!((segs[0].start_ms, segs[0].end_ms), (0, 1_200));
        let errors = events.iter().filter(|e| matches!(e, RealtimeEvent::Error(_))).count();
        assert_eq!(errors, 1);
    }

    #[test]
    fn decode_turns_report_their_chunks_and_finals_carry_words() {
        let (r, events) = run_scripted(
            |n| match n {
                0 => Some(vec![tok("▁hello", 0)]),
                1 => Some(vec![tok("▁world", 600)]),
                _ => Some(Vec::new()),
            },
            3,
        );
        r.expect("run");
        let stats: Vec<(u32, u32, usize)> = events
            .iter()
            .filter_map(|e| match e {
                RealtimeEvent::Stat(EngineStat::Decode { decoded_ms, step_ms, cost_ms, .. }) => {
                    Some((*decoded_ms, *step_ms, cost_ms.len()))
                }
                _ => None,
            })
            .collect();
        assert_eq!(stats.iter().map(|s| s.2).sum::<usize>(), 3, "one cost per decoded chunk: {stats:?}");
        assert_eq!(stats.iter().map(|s| s.1).sum::<u32>(), 3 * CHUNK_MS as u32);
        assert_eq!(stats.last().map(|s| s.0), Some(3 * CHUNK_MS as u32));
        let finals: Vec<&Vec<transcript::Word>> = events
            .iter()
            .filter_map(|e| match e {
                RealtimeEvent::Final { segment } => Some(&segment.words),
                _ => None,
            })
            .collect();
        assert_eq!(finals.len(), 1);
        let texts: Vec<(&str, u32)> = finals[0].iter().map(|w| (w.text.as_str(), w.start_ms)).collect();
        assert_eq!(texts, [("hello", 0), ("world", 600)]);
    }

    #[test]
    fn consecutive_failures_end_the_run_with_an_error() {
        let (r, _) = run_scripted(|_| None, 5);
        let e = r.expect_err("a persistent failure must not loop");
        assert!(e.to_string().contains("3 times in a row"), "{e}");
    }

    #[test]
    fn failures_separated_by_successes_keep_running() {
        let (r, _) = run_scripted(|n| if n % 2 == 1 { None } else { Some(Vec::new()) }, 8);
        assert!(r.is_ok());
    }

    /// Decoder over a real mel front end: records each push's first sample,
    /// length and the chunks it completed; push `fail_on` fails.
    struct MelCounter {
        mel: MelStream,
        chunks: i64,
        fail_on: Option<usize>,
        pushes: Vec<(f32, usize, usize)>,
    }

    impl MelCounter {
        fn new(fail_on: Option<usize>) -> Self {
            Self { mel: MelStream::new(), chunks: 0, fail_on, pushes: Vec::new() }
        }

        fn drain(&mut self) -> usize {
            let mut k = 0;
            while self.mel.next_chunk().is_some() {
                k += 1;
            }
            self.chunks += k as i64;
            k
        }
    }

    impl Decoder for MelCounter {
        fn push(&mut self, pcm: &[f32]) -> std::result::Result<Vec<TimedToken>, NemotronError> {
            if self.fail_on == Some(self.pushes.len()) {
                self.pushes.push((pcm[0], pcm.len(), 0));
                return Err(NemotronError::Model("scripted failure".into()));
            }
            self.mel.push(pcm);
            let k = self.drain();
            self.pushes.push((pcm[0], pcm.len(), k));
            Ok(Vec::new())
        }
        fn finish(&mut self) -> std::result::Result<Vec<TimedToken>, NemotronError> {
            self.mel.finish();
            self.drain();
            Ok(Vec::new())
        }
        fn decoded_ms(&self) -> i64 {
            self.chunks * CHUNK_MS
        }
        fn restart(&mut self) {
            self.mel = MelStream::new();
            self.chunks = 0;
        }
    }

    /// Sample `i` of the audio clock carries the value `i`.
    fn ramp(n: usize) -> Vec<f32> {
        (0..n).map(|i| i as f32).collect()
    }

    /// Feeds `pcm` in 20 ms frames the way `drive` does, decoding when a cut
    /// is ready; returns the per-chunk timings.
    fn feed_frames<D: Decoder>(s: &mut Session<D>, pcm: &[f32]) -> Vec<u64> {
        let mut chunk_ms = Vec::new();
        for frame in pcm.chunks(320) {
            s.buffer(frame);
            if s.ready() {
                chunk_ms.extend(s.decode(false).unwrap().chunk_ms);
            }
        }
        chunk_ms
    }

    #[test]
    fn pushes_are_cut_where_the_front_end_completes_a_chunk() {
        let mut s = Session::new(MelCounter::new(None));
        let timings = feed_frames(&mut s, &ramp(CHUNK_SAMPLES + CHUNK_LOOKAHEAD_SAMPLES + 3 * CHUNK_SAMPLES + 100));
        let lens: Vec<usize> = s.dec.pushes.iter().map(|p| p.1).collect();
        assert_eq!(lens, [9_000, 8_960, 8_960, 8_960]);
        assert!(s.dec.pushes.iter().all(|p| p.2 == 1), "each cut yields exactly one chunk: {:?}", s.dec.pushes);
        assert_eq!(timings.len(), 4, "one timing per decoded chunk");
        assert_eq!(s.carry.len(), 100);
        assert_eq!(s.now_ms(), 4 * CHUNK_MS);
    }

    #[test]
    fn a_failing_chunk_loses_only_its_own_audio() {
        let mut s = Session::new(MelCounter::new(Some(1)));
        let timings = feed_frames(&mut s, &ramp(5 * CHUNK_SAMPLES));
        let starts: Vec<(f32, usize, usize)> = s.dec.pushes.clone();
        assert_eq!(
            starts,
            [(0.0, 9_000, 1), (9_000.0, 8_960, 0), (17_920.0, 9_000, 1), (26_920.0, 8_960, 1)],
        );
        assert_eq!(timings.len(), 3, "the failed chunk records no decode");
        assert_eq!(s.now_ms(), 4 * CHUNK_MS, "the restarted stream keeps the audio clock");
        assert_eq!(s.carry.len(), 5 * CHUNK_SAMPLES - 35_880);
    }

    #[test]
    fn live_metrics_count_one_decode_per_chunk() {
        let _serial = METRICS.lock().unwrap_or_else(|e| e.into_inner());
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let before = live_metrics::snapshot().decodes;
        let segs = rt.block_on(async {
            let (audio_tx, audio_rx) = mpsc::unbounded_channel();
            let (ev_tx, _ev_rx) = mpsc::channel(256);
            let feeder = tokio::spawn(async move {
                let pcm = vec![0i16; CHUNK_SAMPLES + CHUNK_LOOKAHEAD_SAMPLES + 4 * CHUNK_SAMPLES];
                for frame in pcm.chunks(320) {
                    audio_tx.send(frame.to_vec()).unwrap();
                    tokio::task::yield_now().await;
                }
            });
            let dec = Scripted { calls: 0, chunks: 0, script: |_| Some(Vec::new()), tail: Vec::new() };
            let r = drive(dec, audio_rx, ev_tx).await;
            feeder.await.unwrap();
            r
        });
        assert!(segs.unwrap().is_empty());
        assert_eq!(live_metrics::snapshot().decodes - before, 5, "five chunks, five decodes");
    }

    #[test]
    fn the_stream_end_keeps_the_tokens_of_the_remainder_push_and_of_finish() {
        let _serial = METRICS.lock().unwrap_or_else(|e| e.into_inner());
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let segs = rt.block_on(async {
            let (audio_tx, audio_rx) = mpsc::unbounded_channel();
            let (ev_tx, _ev_rx) = mpsc::channel(64);
            audio_tx.send(vec![0i16; 1_000]).unwrap();
            drop(audio_tx);
            let dec = Scripted {
                calls: 0,
                chunks: 0,
                script: |_| Some(vec![tok("▁one", 0)]),
                tail: vec![tok("▁two", 400)],
            };
            drive(dec, audio_rx, ev_tx).await.unwrap()
        });
        assert_eq!(segs.len(), 1, "{segs:?}");
        assert_eq!(segs[0].text, "one two");
    }
}
