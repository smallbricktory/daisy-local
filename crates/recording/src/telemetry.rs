// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Per-session caption telemetry: `<session>/telemetry.jsonl`, written for
//! every recording. One JSON object per line, each carrying `v` (schema
//! version), `kind` and `wall_ms` (Unix ms when the line was produced).
//! Audio times (`*_ms` other than `wall_ms`) are session time: ms of audio
//! since the session's first mic frame.
//!
//! Kinds:
//! - `header`: build, host, live engine and devices; first line.
//! - `anchor`: `{track, audio_ms}` — audio up to `audio_ms` had reached the
//!   live engine at `wall_ms`. Maps audio time to wall time.
//! - `words`: one committed final: `{track, engine, words: [[text, start_ms,
//!   end_ms, first_seen_wall_ms | null], …]}`; `wall_ms` is the final's
//!   receive time and `first_seen_wall_ms` the receive time of the first
//!   interim that showed the word with its final text.
//! - `decode`: one engine decode turn: `{track, engine, decoded_ms, step_ms,
//!   cost_ms: [..], backlog_ms}`.
//! - `result`: one result batch from an engine that decodes on its own
//!   threads: `{track, engine, audio_end_ms, fed_ms, final, words}`.
//! - `proc`: process sample: `{cpu_pct, rss_mb, threads, thread_cpu: [[name,
//!   pct], …]}`; percentages are of one core.
//! - `stage`: a finalize stage starts: `{stage}`; a stage ends where the next
//!   one starts, and `done`, `error` and `awaiting-labels` end a run.
//! - `end`: the recording stopped: `{dropped}` counts lines lost to a full
//!   queue.
//!
//! The file lives beside the session's other plaintext content and leaves
//! the machine only with the session folder.

use crate::jsonl_sink::JsonlSink;
use crate::live_transcript::LiveTrack;
use providers_realtime::EngineStat;
use transcript::Word;
use serde::Serialize;
use std::path::Path;

pub const TELEMETRY_FILE: &str = "telemetry.jsonl";
pub const SCHEMA_VERSION: u32 = 1;

/// Unix time in ms.
pub fn wall_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Build and machine facts for the header.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Host {
    pub app_version: String,
    pub build_sha: String,
    pub os: String,
    pub os_version: String,
    pub arch: String,
    pub cpu_model: String,
    pub logical_cores: usize,
    pub physical_cores: Option<usize>,
}

impl Host {
    /// This machine, for an app at `app_version` built from `build_sha`.
    pub fn detect(app_version: &str, build_sha: &str) -> Self {
        let mut sys = sysinfo::System::new();
        sys.refresh_cpu_list(sysinfo::CpuRefreshKind::nothing());
        Self {
            app_version: app_version.into(),
            build_sha: build_sha.into(),
            os: std::env::consts::OS.into(),
            os_version: sysinfo::System::long_os_version().unwrap_or_default(),
            arch: std::env::consts::ARCH.into(),
            cpu_model: sys.cpus().first().map(|c| c.brand().trim().to_string()).unwrap_or_default(),
            logical_cores: std::thread::available_parallelism().map(|n| n.get()).unwrap_or(0),
            physical_cores: sys.physical_core_count(),
        }
    }
}

/// A live engine as the header names it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EngineId {
    pub engine: String,
    pub model: String,
}

impl EngineId {
    pub fn of(t: &dyn providers_realtime::RealtimeTranscriber) -> Self {
        Self { engine: t.name().into(), model: t.model().into() }
    }
}

/// Everything the header records besides the host.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Header {
    pub session_id: String,
    pub session_start_wall_ms: i64,
    /// `"realtime"` or `"off"`.
    pub live_mode: String,
    pub engine: Option<EngineId>,
    pub fallbacks: Vec<EngineId>,
    pub mic_device: String,
    pub system_device: String,
    pub sample_rate: u32,
}

/// One committed word: `(text, start_ms, end_ms, first_seen_wall_ms)`.
pub type WordRow = (String, u32, u32, Option<i64>);

/// A process sample.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ProcRecord {
    pub cpu_pct: f32,
    pub rss_mb: f32,
    pub threads: Option<u32>,
    /// Per thread name, percent of one core, busiest first.
    pub thread_cpu: Vec<(String, f32)>,
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Record<'a> {
    Header {
        #[serde(flatten)]
        host: &'a Host,
        #[serde(flatten)]
        header: &'a Header,
    },
    Anchor {
        track: LiveTrack,
        audio_ms: u32,
    },
    Words {
        track: LiveTrack,
        engine: &'a str,
        words: &'a [WordRow],
    },
    Decode {
        track: LiveTrack,
        engine: &'a str,
        decoded_ms: u32,
        step_ms: u32,
        cost_ms: &'a [u32],
        backlog_ms: u32,
    },
    Result {
        track: LiveTrack,
        engine: &'a str,
        audio_end_ms: u32,
        fed_ms: u32,
        #[serde(rename = "final")]
        is_final: bool,
        words: u32,
    },
    Proc(&'a ProcRecord),
    Stage {
        stage: &'a str,
    },
    End {
        dropped: u64,
    },
}

#[derive(Serialize)]
struct Line<'a> {
    v: u32,
    wall_ms: i64,
    #[serde(flatten)]
    record: Record<'a>,
}

fn encode(wall_ms: i64, record: Record<'_>) -> String {
    serde_json::to_string(&Line { v: SCHEMA_VERSION, wall_ms, record }).unwrap_or_default()
}

/// The session's telemetry writer. Shared by `Arc`; the file is complete
/// once the last handle drops.
pub struct Telemetry {
    sink: JsonlSink,
}

impl Telemetry {
    /// Opens (appending) `<session_dir>/telemetry.jsonl`. Never fails: on
    /// I/O error the writer is inert with a log line.
    pub fn open(session_dir: &Path) -> Self {
        match JsonlSink::open(&session_dir.join(TELEMETRY_FILE), "daisy-telemetry") {
            Ok(sink) => Self { sink },
            Err(e) => {
                log::warn!("telemetry open failed ({e}); this session records none");
                Self::disabled()
            }
        }
    }

    /// A writer that discards everything.
    pub fn disabled() -> Self {
        Self { sink: JsonlSink::disabled() }
    }

    fn write(&self, wall_ms: i64, record: Record<'_>) {
        if self.sink.is_enabled() {
            self.sink.send(encode(wall_ms, record));
        }
    }

    pub fn header(&self, host: &Host, header: &Header) {
        self.write(wall_ms(), Record::Header { host, header });
    }

    pub fn anchor(&self, track: LiveTrack, audio_ms: u32, at_wall_ms: i64) {
        self.write(at_wall_ms, Record::Anchor { track, audio_ms });
    }

    /// One committed final, received at `final_wall_ms`.
    pub fn words(&self, track: LiveTrack, engine: &str, final_wall_ms: i64, words: &[WordRow]) {
        if !words.is_empty() {
            self.write(final_wall_ms, Record::Words { track, engine, words });
        }
    }

    /// One engine stat, received at `at_wall_ms`; times already in session
    /// time.
    pub fn stat(&self, track: LiveTrack, engine: &str, stat: &EngineStat, at_wall_ms: i64) {
        let record = match stat {
            EngineStat::Decode { decoded_ms, step_ms, cost_ms, backlog_ms } => Record::Decode {
                track,
                engine,
                decoded_ms: *decoded_ms,
                step_ms: *step_ms,
                cost_ms,
                backlog_ms: *backlog_ms,
            },
            EngineStat::Result { audio_end_ms, fed_ms, is_final, words } => Record::Result {
                track,
                engine,
                audio_end_ms: *audio_end_ms,
                fed_ms: *fed_ms,
                is_final: *is_final,
                words: *words,
            },
        };
        self.write(at_wall_ms, record);
    }

    pub fn proc(&self, sample: &ProcRecord) {
        self.write(wall_ms(), Record::Proc(sample));
    }

    /// Closes the recording's record with the count of lines lost so far.
    pub fn end(&self) {
        let dropped = self.sink.dropped();
        self.write(wall_ms(), Record::End { dropped });
    }
}

/// Appends one `stage` line to `<session_dir>/telemetry.jsonl` with a single
/// write. Best-effort.
pub fn append_stage(session_dir: &Path, stage: &str) {
    use std::io::Write;
    let line = encode(wall_ms(), Record::Stage { stage }) + "\n";
    let r = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(session_dir.join(TELEMETRY_FILE))
        .and_then(|mut f| f.write_all(line.as_bytes()));
    if let Err(e) = r {
        log::debug!("telemetry stage {stage}: {e}");
    }
}

/// Rewrites `<session_dir>/telemetry.jsonl` without the committed words that
/// start at or after `cut_ms` (session time). A `words` line left with no
/// words is removed, and so is a line that does not parse. Best-effort.
pub fn truncate_words(session_dir: &Path, cut_ms: u64) {
    let path = session_dir.join(TELEMETRY_FILE);
    let Ok(text) = std::fs::read_to_string(&path) else { return };
    let mut out = String::with_capacity(text.len());
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let Ok(mut v) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        if v["kind"] == "words" {
            let Some(words) = v.get_mut("words").and_then(|w| w.as_array_mut()) else { continue };
            words.retain(|w| w.get(1).and_then(|s| s.as_u64()).is_some_and(|s| s < cut_ms));
            if words.is_empty() {
                continue;
            }
            out.push_str(&v.to_string());
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }
    if let Err(e) = syncsafe::write_atomic(&path, out) {
        log::warn!("telemetry truncate: {e}");
    }
}

/// Interim words kept per track; a longer hypothesis keeps its newest
/// words.
const MAX_SHOWN: usize = 400;

/// Lowercase alphanumerics and apostrophes: the form two showings of a
/// word are compared in.
fn norm(text: &str) -> String {
    text.chars()
        .filter(|c| c.is_alphanumeric() || *c == '\'')
        .flat_map(char::to_lowercase)
        .collect()
}

/// For each item of `b`, the index of the item of `a` a longest common
/// subsequence pairs it with.
fn lcs_pairs(a: &[String], b: &[String]) -> Vec<Option<usize>> {
    let (n, m) = (a.len(), b.len());
    let mut len = vec![vec![0u16; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            len[i][j] = if a[i] == b[j] { len[i + 1][j + 1] + 1 } else { len[i + 1][j].max(len[i][j + 1]) };
        }
    }
    let mut pairs = vec![None; m];
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if a[i] == b[j] {
            pairs[j] = Some(i);
            i += 1;
            j += 1;
        } else if len[i + 1][j] >= len[i][j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    pairs
}

/// When each word first showed in an interim, per track. Each interim
/// replaces the shown word sequence; a word the previous interim also
/// showed (paired by longest common subsequence of the words) keeps its
/// first time. A final pairs its words with the shown sequence the same way
/// and consumes the shown words up to its last pair.
#[derive(Debug, Default)]
pub struct FirstSeen {
    shown: Vec<(String, i64)>,
}

impl FirstSeen {
    fn words(&self) -> Vec<String> {
        self.shown.iter().map(|(w, _)| w.clone()).collect()
    }

    /// Registers the text of an interim received at `wall_ms`.
    pub fn interim(&mut self, text: &str, wall_ms: i64) {
        let mut words: Vec<String> = text.split_whitespace().map(norm).filter(|w| !w.is_empty()).collect();
        if words.len() > MAX_SHOWN {
            words.drain(..words.len() - MAX_SHOWN);
        }
        let pairs = lcs_pairs(&self.words(), &words);
        self.shown = words
            .into_iter()
            .zip(pairs)
            .map(|(w, p)| {
                let t = p.map(|i| self.shown[i].1).unwrap_or(wall_ms);
                (w, t)
            })
            .collect();
    }

    /// Rows for the words of a committed final, each with the time the word
    /// first showed in an interim.
    pub fn commit(&mut self, words: &[Word]) -> Vec<WordRow> {
        let finals: Vec<String> = words.iter().map(|w| norm(&w.text)).collect();
        let pairs = lcs_pairs(&self.words(), &finals);
        let rows = words
            .iter()
            .zip(&pairs)
            .map(|(w, p)| (w.text.clone(), w.start_ms, w.end_ms, p.map(|i| self.shown[i].1)))
            .collect();
        if let Some(last) = pairs.iter().flatten().max() {
            self.shown.drain(..=*last);
        }
        rows
    }
}

/// Audio between anchors when nothing else forces one.
const ANCHOR_EVERY_MS: u32 = 5_000;
/// Arrival later than the window's earliest by more than this starts a new
/// anchor at once.
const ANCHOR_JUMP_MS: i64 = 250;

/// Turns frame arrivals at the live engine into `anchor` points. Within each
/// [`ANCHOR_EVERY_MS`] window it keeps the frame that arrived earliest
/// relative to its audio time; a frame arriving [`ANCHOR_JUMP_MS`] later
/// than that (a pause, a stall) closes the window and anchors itself.
#[derive(Debug, Default)]
pub struct FeedClock {
    samples: u64,
    window_start_ms: u32,
    best: Option<(u32, i64)>,
    best_sent: bool,
}

impl FeedClock {
    /// Records a frame of `n` samples at `rate` Hz arriving at `wall_ms`,
    /// whose stream starts `base_ms` into the session. Returns the anchors
    /// `(audio_ms, wall_ms)` to write, oldest first.
    pub fn frame(&mut self, n: usize, rate: u32, base_ms: u32, wall_ms: i64) -> Vec<(u32, i64)> {
        self.samples += n as u64;
        let audio_ms = base_ms + (self.samples * 1000 / rate.max(1) as u64) as u32;
        let offset = wall_ms - audio_ms as i64;
        let mut out = Vec::new();
        match self.best {
            None => {
                self.window_start_ms = audio_ms;
                self.best = Some((audio_ms, wall_ms));
                self.best_sent = true;
                out.push((audio_ms, wall_ms));
            }
            Some((b_audio, b_wall)) => {
                let best_offset = b_wall - b_audio as i64;
                if offset > best_offset + ANCHOR_JUMP_MS {
                    if !self.best_sent {
                        out.push((b_audio, b_wall));
                    }
                    self.window_start_ms = audio_ms;
                    self.best = Some((audio_ms, wall_ms));
                    self.best_sent = true;
                    out.push((audio_ms, wall_ms));
                } else {
                    if offset < best_offset {
                        self.best = Some((audio_ms, wall_ms));
                        self.best_sent = false;
                    }
                    if audio_ms.saturating_sub(self.window_start_ms) >= ANCHOR_EVERY_MS {
                        if let (Some(b), false) = (self.best, self.best_sent) {
                            out.push(b);
                        }
                        self.window_start_ms = audio_ms;
                        self.best = Some((audio_ms, wall_ms));
                        self.best_sent = false;
                    }
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(text: &str, s: u32, e: u32) -> Word {
        Word { text: text.into(), start_ms: s, end_ms: e }
    }

    fn parse(line: &str) -> serde_json::Value {
        serde_json::from_str(line).unwrap()
    }

    #[test]
    fn records_encode_with_version_kind_and_wall_time() {
        let host = Host { app_version: "1.2.3".into(), build_sha: "abc1234".into(), os: "linux".into(), logical_cores: 8, ..Default::default() };
        let header = Header {
            session_id: "daisy-1".into(),
            session_start_wall_ms: 1_000,
            live_mode: "realtime".into(),
            engine: Some(EngineId { engine: "nemotron".into(), model: "Nemotron 0.6B".into() }),
            fallbacks: vec![],
            mic_device: "Desk Mic".into(),
            system_device: "Speakers".into(),
            sample_rate: 16_000,
        };
        let h = parse(&encode(5, Record::Header { host: &host, header: &header }));
        assert_eq!(h["v"], SCHEMA_VERSION);
        assert_eq!(h["kind"], "header");
        assert_eq!(h["wall_ms"], 5);
        assert_eq!(h["app_version"], "1.2.3");
        assert_eq!(h["engine"]["engine"], "nemotron");
        assert_eq!(h["mic_device"], "Desk Mic");

        let rows: Vec<WordRow> = vec![("hello".into(), 100, 400, Some(900)), ("there".into(), 400, 700, None)];
        let words = parse(&encode(1_500, Record::Words { track: LiveTrack::Mic, engine: "nemotron", words: &rows }));
        assert_eq!(words["kind"], "words");
        assert_eq!(words["track"], "mic");
        assert_eq!(words["words"][0], serde_json::json!(["hello", 100, 400, 900]));
        assert_eq!(words["words"][1][3], serde_json::Value::Null);

        let d = parse(&encode(7, Record::Decode { track: LiveTrack::System, engine: "nemotron", decoded_ms: 1_120, step_ms: 560, cost_ms: &[41], backlog_ms: 20 }));
        assert_eq!(d["kind"], "decode");
        assert_eq!(d["cost_ms"], serde_json::json!([41]));

        let r = parse(&encode(8, Record::Result { track: LiveTrack::Mic, engine: "speechanalyzer", audio_end_ms: 900, fed_ms: 1_400, is_final: true, words: 3 }));
        assert_eq!(r["final"], true);

        let p = ProcRecord { cpu_pct: 12.5, rss_mb: 900.0, threads: Some(40), thread_cpu: vec![("asr".into(), 8.0)] };
        let p = parse(&encode(9, Record::Proc(&p)));
        assert_eq!(p["kind"], "proc");
        assert_eq!(p["thread_cpu"][0], serde_json::json!(["asr", 8.0]));

        assert_eq!(parse(&encode(10, Record::Stage { stage: "deduping" }))["stage"], "deduping");
        assert_eq!(parse(&encode(11, Record::End { dropped: 2 }))["dropped"], 2);
        assert_eq!(parse(&encode(12, Record::Anchor { track: LiveTrack::Mic, audio_ms: 5_000 }))["audio_ms"], 5_000);
    }

    #[test]
    fn the_writer_and_stage_appends_share_one_file() {
        let dir = tempfile::tempdir().unwrap();
        let t = Telemetry::open(dir.path());
        t.anchor(LiveTrack::Mic, 20, 1_000);
        t.words(LiveTrack::Mic, "nemotron", 2_000, &[("hi".into(), 0, 300, Some(1_500))]);
        t.words(LiveTrack::Mic, "nemotron", 2_100, &[]);
        t.end();
        drop(t);
        append_stage(dir.path(), "finalizing");
        let text = std::fs::read_to_string(dir.path().join(TELEMETRY_FILE)).unwrap();
        let kinds: Vec<String> = text.lines().map(|l| parse(l)["kind"].as_str().unwrap().to_string()).collect();
        assert_eq!(kinds, ["anchor", "words", "end", "stage"]);
    }

    #[test]
    fn truncating_drops_the_words_from_the_cut_on() {
        let dir = tempfile::tempdir().unwrap();
        let t = Telemetry::open(dir.path());
        t.anchor(LiveTrack::Mic, 20, 1_000);
        t.words(LiveTrack::Mic, "nemotron", 2_000, &[("kept".into(), 100, 400, None), ("straddles".into(), 400, 900, None)]);
        t.words(LiveTrack::Mic, "nemotron", 3_000, &[("starts".into(), 500, 800, None), ("later".into(), 900, 1_200, None)]);
        t.end();
        drop(t);
        truncate_words(dir.path(), 500);
        let text = std::fs::read_to_string(dir.path().join(TELEMETRY_FILE)).unwrap();
        let lines: Vec<serde_json::Value> = text.lines().map(parse).collect();
        let kinds: Vec<&str> = lines.iter().map(|l| l["kind"].as_str().unwrap()).collect();
        assert_eq!(kinds, ["anchor", "words", "end"]);
        assert_eq!(lines[1]["words"], serde_json::json!([["kept", 100, 400, null], ["straddles", 400, 900, null]]));
        assert!(!text.contains("later") && !text.contains("starts"), "{text}");
    }

    #[test]
    fn a_word_takes_the_time_of_the_first_interim_that_showed_it() {
        let mut fs = FirstSeen::default();
        fs.interim("Hel", 1_000);
        fs.interim("Hello", 1_200);
        fs.interim("Hello wor", 1_400);
        fs.interim("Hello, world", 1_600);
        let rows = fs.commit(&[w("Hello,", 0, 320), w("world.", 400, 800)]);
        assert_eq!(rows[0], ("Hello,".to_string(), 0, 320, Some(1_200)), "a partial word does not count");
        assert_eq!(rows[1].3, Some(1_600));
        assert!(fs.shown.is_empty(), "committed words are consumed");
    }

    #[test]
    fn a_revised_word_takes_the_time_of_its_corrected_text() {
        let mut fs = FirstSeen::default();
        fs.interim("their dog", 1_000);
        fs.interim("there dog runs", 1_500);
        let rows = fs.commit(&[w("there", 0, 300), w("dog", 300, 600), w("runs", 600, 900)]);
        let firsts: Vec<Option<i64>> = rows.iter().map(|r| r.3).collect();
        assert_eq!(firsts, [Some(1_500), Some(1_000), Some(1_500)], "the unchanged word keeps its time");
    }

    #[test]
    fn a_word_never_shown_in_an_interim_has_no_first_seen_time() {
        let mut fs = FirstSeen::default();
        fs.interim("see you", 1_000);
        let rows = fs.commit(&[w("see", 0, 300), w("you", 300, 500), w("later", 500, 900)]);
        assert_eq!(rows[2].3, None);
    }

    #[test]
    fn repeated_words_each_keep_their_own_time() {
        let mut fs = FirstSeen::default();
        fs.interim("no", 1_000);
        fs.interim("no no", 1_400);
        fs.interim("no no no", 1_700);
        let rows = fs.commit(&[w("no", 0, 200), w("no", 300, 500), w("no", 600, 800)]);
        let firsts: Vec<Option<i64>> = rows.iter().map(|r| r.3).collect();
        assert_eq!(firsts, [Some(1_000), Some(1_400), Some(1_700)]);
    }

    #[test]
    fn a_final_consumes_only_the_words_it_commits() {
        let mut fs = FirstSeen::default();
        fs.interim("thanks everyone so", 1_000);
        let rows = fs.commit(&[w("Thanks,", 0, 300), w("everyone.", 300, 900)]);
        assert_eq!(rows[1].3, Some(1_000));
        fs.interim("so the agenda", 2_000);
        let rows = fs.commit(&[w("So", 3_000, 3_200), w("the", 3_200, 3_300), w("agenda.", 3_300, 3_900)]);
        let firsts: Vec<Option<i64>> = rows.iter().map(|r| r.3).collect();
        assert_eq!(firsts, [Some(1_000), Some(2_000), Some(2_000)], "the uncommitted tail keeps its time");
    }

    #[test]
    fn a_long_hypothesis_keeps_its_newest_words() {
        let mut fs = FirstSeen::default();
        let text = (0..MAX_SHOWN + 50).map(|i| format!("w{i}")).collect::<Vec<_>>().join(" ");
        fs.interim(&text, 1);
        assert_eq!(fs.shown.len(), MAX_SHOWN);
        assert_eq!(fs.shown.last().unwrap().0, format!("w{}", MAX_SHOWN + 49));
    }

    #[test]
    fn the_feed_clock_anchors_the_earliest_arrival_of_each_window() {
        let mut c = FeedClock::default();
        let mut anchors = Vec::new();
        // 20 ms frames; arrival jitters 0-30 ms behind real time.
        for i in 0..600u32 {
            let audio_end = (i + 1) * 20;
            let jitter = [10, 30, 0, 20][i as usize % 4];
            anchors.extend(c.frame(320, 16_000, 0, 100_000 + audio_end as i64 + jitter));
        }
        assert_eq!(anchors[0], (20, 100_030), "the first frame anchors at once");
        assert!(anchors.len() >= 2);
        for (audio, wall) in &anchors[1..] {
            assert_eq!(wall - *audio as i64, 100_000, "each window keeps its zero-jitter frame");
        }
    }

    #[test]
    fn the_feed_clock_anchors_right_after_a_pause() {
        let mut c = FeedClock::default();
        let mut anchors = Vec::new();
        for i in 0..50u32 {
            anchors.extend(c.frame(320, 16_000, 0, 1_000 + ((i + 1) * 20) as i64));
        }
        // Ten seconds of pause: no frames, then the stream continues.
        let resumed = c.frame(320, 16_000, 0, 1_000 + 51 * 20 + 10_000);
        assert_eq!(resumed.last(), Some(&(1_020, 1_000 + 51 * 20 + 10_000)));
        anchors.extend(resumed);
        assert_eq!(anchors.first(), Some(&(20, 1_020)));
    }

    #[test]
    fn the_feed_clock_starts_at_the_stream_base() {
        let mut c = FeedClock::default();
        assert_eq!(c.frame(1_600, 16_000, 15_000, 50_000), vec![(15_100, 50_000)]);
    }
}
