// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Rolling window state for serial streaming-whisper. Audio accumulates as
//! 16 kHz mono f32; after each decode the LocalAgreement-2 prefix commits and
//! the audio + token history preceding the last committed token is dropped.
//! The next decode re-reads only the still-uncommitted tail plus new audio.
//! The window is capped at MAX_WINDOW_MS: past it, the front is trimmed at a
//! word two decodes end at the same place, or, when they share no such word,
//! at half the cap with the newer decode's words in front of it emitted.

use super::agreement::{agreed_prefix_len, anchored_agreement, shared_boundary, StreamToken};

pub const SAMPLE_RATE: usize = 16_000;
pub const MAX_WINDOW_MS: i64 = 30_000;
/// Kept in front of the first uncommitted word when trimming.
const TRIM_GUARD_MS: i64 = 150;
/// How many trailing words of an emission are remembered for the guard-overlap check.
const OVERLAP_TAIL_WORDS: usize = 12;
/// Shortest verbatim run stripped as a re-decode where one word alone does
/// not tell a re-decode from new speech (see `guard_overlap`).
const BOUNDARY_MIN_RUN: usize = 3;
/// Analysis frame (ms) for telling speech from quiet in the window audio.
const FRAME_MS: usize = 20;
/// A frame within this many dB of the window's loudest frame is speech.
const SPEECH_RANGE_DB: f32 = 30.0;
/// Frame RMS (normalized f32, ≈ -50 dBFS) below which a frame is never
/// speech.
const SPEECH_RMS_FLOOR: f32 = 0.003_16;
/// Quiet at least this long ends a stretch of speech.
const PAUSE_MS: usize = 400;

use transcript::text::norm_word;

/// What a trim left behind that the next decode can read again.
struct GuardOverlap {
    /// The last emitted words (normalized), each with whether its stamped
    /// span held speech when it was emitted (see [`heard_at`]).
    tail: Vec<(String, bool)>,
    /// Absolute end of the retained guard span.
    end_ms: i64,
    /// Shortest verbatim run that counts as a repeat: one word for a guarded
    /// cut, whose retained span holds the end of the last emitted word;
    /// [`BOUNDARY_MIN_RUN`] for a cut at the committed words' end, where a
    /// single matching word is as likely the next utterance opening on it.
    min_run: usize,
    /// Frame RMS that counted as speech when the words were emitted (see
    /// [`speech_floor`]); the retained head is judged against it.
    floor: f32,
}

/// Result of feeding one fresh decode into the window.
#[derive(Debug, Default, PartialEq)]
pub struct CommitResult {
    /// Tokens newly committed by THIS decode (to emit as Final), absolute time.
    pub committed: Vec<StreamToken>,
    /// The current uncommitted tail (to emit as Interim), absolute time.
    pub interim: Vec<StreamToken>,
}

pub struct StreamWindow {
    samples: Vec<f32>,             // 16 kHz mono, window origin = index 0
    origin_ms: i64,                // absolute stream time of samples[0]
    prev_tokens: Vec<StreamToken>, // last decode, window-relative
    committed_in_window: usize,    // count already committed from window origin
    /// Set when a trim retains audio for words already emitted; the next
    /// commit drops a leading run that repeats them, matched on text within
    /// the guard span.
    guard_overlap: Option<GuardOverlap>,
    /// The latest decode in absolute time, kept across trims; `None` before
    /// the first decode.
    last_reading: Option<Vec<StreamToken>>,
    /// Window cap (ms); [`MAX_WINDOW_MS`] unless overridden.
    cap_ms: i64,
}

impl Default for StreamWindow {
    fn default() -> Self { Self::new() }
}

impl StreamWindow {
    pub fn new() -> Self {
        Self {
            samples: Vec::new(),
            origin_ms: 0,
            prev_tokens: Vec::new(),
            committed_in_window: 0,
            guard_overlap: None,
            last_reading: None,
            cap_ms: MAX_WINDOW_MS,
        }
    }

    /// Override the window cap (ms); the default is [`MAX_WINDOW_MS`].
    pub fn with_cap_ms(mut self, cap_ms: i64) -> Self {
        self.cap_ms = cap_ms.max(1_000);
        self
    }

    /// Append freshly captured 16 kHz mono samples.
    pub fn push_audio(&mut self, pcm: &[f32]) {
        self.samples.extend_from_slice(pcm);
    }

    /// Current window audio (what the decoder should transcribe).
    pub fn window_samples(&self) -> &[f32] {
        &self.samples
    }

    pub fn window_len_ms(&self) -> i64 {
        (self.samples.len() as i64) * 1000 / SAMPLE_RATE as i64
    }

    pub fn origin_ms(&self) -> i64 { self.origin_ms }

    /// Feed the tokens from decoding `window_samples()` (window-relative ms).
    /// Commits the LocalAgreement-2 prefix beyond what's already committed,
    /// trims the audio + token history past the last committed token, and
    /// returns the newly-committed + current-interim tokens in ABSOLUTE time.
    pub fn ingest_decode(&mut self, curr: Vec<StreamToken>) -> CommitResult {
        let mut agreed = agreed_prefix_len(&self.prev_tokens, &curr);
        // Where the agreement ends in the previous decode.
        let mut agreed_prev = agreed;
        // Words both decodes place right behind a differently read head settle it.
        if agreed == self.committed_in_window {
            if let Some(a) = anchored_agreement(&self.prev_tokens, &curr, agreed) {
                (agreed, agreed_prev) = (a.end, a.prev_end);
            }
        }
        let origin = self.origin_ms;
        let len_ms = self.window_len_ms();
        // Where the previous decode puts the first word after the agreed
        // ones.
        let mut prev_next_ms = (agreed_prev > 0)
            .then(|| self.prev_tokens.get(agreed_prev).map(|t| t.start_ms))
            .flatten();
        // Past the cap, trims the front toward half the cap, behind the latest word both decodes end at the same place.
        let pending = (agreed > 0).then(|| commit_cut(&curr, agreed, prev_next_ms, len_ms)).flatten();
        let stays_over = pending.is_none_or(|(cut_ms, _)| cut_ms <= 0 || len_ms - cut_ms > self.cap_ms);
        let mut force_cut = false;
        if let (true, true, Some(reading)) = (len_ms > self.cap_ms, stays_over, &self.last_reading) {
            let before: Vec<StreamToken> = reading
                .iter()
                .map(|t| StreamToken { start_ms: t.start_ms - origin, end_ms: t.end_ms - origin, ..t.clone() })
                .collect();
            let (lo_ms, hi_ms) = (len_ms - self.cap_ms + TRIM_GUARD_MS, len_ms - self.cap_ms / 2);
            match shared_boundary(&before, &curr, lo_ms, hi_ms, TRIM_GUARD_MS) {
                Some(a) if a.end > agreed => {
                    agreed = a.end;
                    prev_next_ms = before.get(a.prev_end).map(|t| t.start_ms);
                }
                _ => force_cut = true,
            }
        }

        let to_abs = |t: &StreamToken| StreamToken {
            text: t.text.clone(),
            start_ms: t.start_ms + origin,
            end_ms: t.end_ms + origin,
        };
        // The emit-time level reference, kept for the next strip.
        let floor = speech_floor(&self.samples);
        let frames = speech_frames(&self.samples, floor);
        // Tail of what an emission holds, for the next trim to match.
        let tail_of = |emitted: &[StreamToken]| -> Vec<(String, bool)> {
            emitted
                .iter()
                .skip(emitted.len().saturating_sub(OVERLAP_TAIL_WORDS))
                .map(|t| (norm_word(&t.text), heard_at(&frames, t.start_ms - origin, t.end_ms - origin)))
                .collect()
        };

        if force_cut {
            // Trim the front down to half-cap.
            let cut_ms = len_ms - self.cap_ms / 2;
            // Before discarding the front audio, the words not yet committed
            // that cover the trimmed-away region are force-emitted as
            // committed (Final); words still in the retained window stay
            // interim.
            let fresh = curr.iter().skip(self.committed_in_window);
            let mut forced: Vec<StreamToken> =
                fresh.clone().filter(|t| t.end_ms <= cut_ms).map(to_abs).collect();
            let interim: Vec<StreamToken> =
                fresh.filter(|t| t.end_ms > cut_ms).map(to_abs).collect();
            self.strip_re_read(&mut forced);
            let forced_tail = tail_of(&forced);
            self.last_reading = Some(curr.iter().map(to_abs).collect());
            self.prev_tokens = curr;
            self.trim_front(cut_ms);
            if !forced_tail.is_empty() {
                self.guard_overlap = Some(GuardOverlap {
                    tail: forced_tail,
                    end_ms: self.origin_ms + TRIM_GUARD_MS * 2,
                    min_run: 1,
                    floor,
                });
            }
            return CommitResult { committed: forced, interim };
        }

        let newly = agreed.saturating_sub(self.committed_in_window);
        let mut committed: Vec<StreamToken> =
            curr.iter().skip(self.committed_in_window).take(newly).map(to_abs).collect();
        self.strip_re_read(&mut committed);
        let interim: Vec<StreamToken> =
            curr.iter().skip(agreed).map(to_abs).collect();
        let emitted_tail = tail_of(&committed);
        let cut = (agreed > 0).then(|| commit_cut(&curr, agreed, prev_next_ms, len_ms)).flatten();
        self.committed_in_window = agreed;
        self.last_reading = Some(curr.iter().map(to_abs).collect());
        self.prev_tokens = curr;

        if let Some((cut_ms, all_committed)) = cut {
            let dropped = self.trim_front(cut_ms);
            // The retained span decodes again; the next commit is matched
            // against the emitted tail. A cut that drops nothing retains
            // nothing new.
            if dropped && !emitted_tail.is_empty() {
                let min_run = if all_committed { BOUNDARY_MIN_RUN } else { 1 };
                self.guard_overlap = Some(GuardOverlap {
                    tail: emitted_tail,
                    end_ms: self.origin_ms + TRIM_GUARD_MS * 2,
                    min_run,
                    floor,
                });
            }
        }

        CommitResult { committed, interim }
    }

    /// Strips a leading run of `words` that repeats the tail of the last emission.
    fn strip_re_read(&mut self, words: &mut Vec<StreamToken>) {
        if words.is_empty() {
            return;
        }
        let Some(g) = self.guard_overlap.take() else { return };
        if words[0].start_ms >= g.end_ms {
            return;
        }
        let tail = &g.tail;
        // Judges the head at the emit-time level.
        let room = head_room(&speech_frames(&self.samples, g.floor));
        let fits = |n: usize| {
            tail[tail.len().saturating_sub(n)..].iter().filter(|(_, heard)| *heard).count() <= room
        };
        // The repeat can hide behind one variant word (a re-decode that turns
        // "or" into "are"); the variant is stripped only together with a
        // ≥3-word verbatim run behind it.
        let strip = [0usize, 1]
            .into_iter()
            .filter_map(|skip| {
                let min_k = if skip == 0 { g.min_run } else { BOUNDARY_MIN_RUN };
                let max_k = tail.len().min(words.len().saturating_sub(skip));
                (min_k..=max_k)
                    .rev()
                    .find(|&k| {
                        fits(skip + k)
                            && tail[tail.len() - k..]
                                .iter()
                                .zip(&words[skip..skip + k])
                                .all(|((w, _), t)| *w == norm_word(&t.text))
                    })
                    .map(|k| skip + k)
            })
            .next();
        words.drain(..strip.unwrap_or(0));
    }

    /// Drop `cut_ms` of audio from the front, shift the origin, and reset
    /// token bookkeeping; the next decode starts fresh from the new origin.
    /// Returns whether any audio was dropped.
    fn trim_front(&mut self, cut_ms: i64) -> bool {
        let cut_ms = cut_ms.clamp(0, self.window_len_ms());
        let drop = (cut_ms as usize) * SAMPLE_RATE / 1000;
        if drop == 0 { return false; }
        self.samples.drain(0..drop.min(self.samples.len()));
        self.origin_ms += cut_ms;
        self.prev_tokens.clear();
        self.committed_in_window = 0;
        true
    }
}

/// Where committing `toks[..agreed]` cuts the window (ms), and whether that
/// commits every word. The cut is the last committed word's end, bounded by
/// the first uncommitted word's start, and held [`TRIM_GUARD_MS`] clear of
/// that word. `None` when the cut lands at or past the window's end with
/// words still uncommitted: the window is kept, and the cap bounds it.
fn commit_cut(
    toks: &[StreamToken],
    agreed: usize,
    prev_next_ms: Option<i64>,
    len_ms: i64,
) -> Option<(i64, bool)> {
    let last = &toks[agreed - 1];
    let mut base_ms = last.end_ms;
    if let Some(first_uncommitted) = toks.get(agreed) {
        base_ms = base_ms.min(first_uncommitted.start_ms);
    }
    // Stops the cut at the next word's start when that falls inside the committed word's span.
    let prev_bound = prev_next_ms.filter(|&n| n > last.start_ms && n < base_ms);
    if let Some(n) = prev_bound {
        base_ms = n;
    }
    let all_committed = agreed == toks.len() && prev_bound.is_none();
    // Trim safety is judged on the unguarded boundary.
    if !all_committed && base_ms >= len_ms {
        return None;
    }
    // Everything committed: no onset to protect, the cut is where the words
    // end.
    Some((if all_committed { base_ms } else { (base_ms - TRIM_GUARD_MS).max(0) }, all_committed))
}

/// RMS level of each [`FRAME_MS`] frame of `samples`.
fn frame_rms(samples: &[f32]) -> impl Iterator<Item = f32> + '_ {
    samples
        .chunks(SAMPLE_RATE * FRAME_MS / 1000)
        .map(|c| (c.iter().map(|s| s * s).sum::<f32>() / c.len() as f32).sqrt())
}

/// The frame RMS at or above which `samples` hold speech: within
/// [`SPEECH_RANGE_DB`] of the loudest frame, and never below
/// [`SPEECH_RMS_FLOOR`].
fn speech_floor(samples: &[f32]) -> f32 {
    let loudest = frame_rms(samples).fold(0.0f32, f32::max);
    (loudest * 10f32.powf(-SPEECH_RANGE_DB / 20.0)).max(SPEECH_RMS_FLOOR)
}

/// Per [`FRAME_MS`] frame of `samples`: whether its RMS reaches `floor`.
fn speech_frames(samples: &[f32], floor: f32) -> Vec<bool> {
    frame_rms(samples).map(|r| r >= floor).collect()
}

/// Whether the window-relative span `start_ms..end_ms` is mostly speech. An
/// empty span holds none.
fn heard_at(frames: &[bool], start_ms: i64, end_ms: i64) -> bool {
    let first = (start_ms.max(0) as usize / FRAME_MS).min(frames.len());
    let last = (end_ms.max(0) as usize).div_ceil(FRAME_MS).min(frames.len());
    let n = last.saturating_sub(first);
    n > 0 && 2 * frames[first..last].iter().filter(|&&s| s).count() >= n
}

/// How many emitted words, heard where they were stamped, the speech at the
/// window's start can hold again: none when the window opens on a pause, one
/// when its speech stops inside the guard span (the end of the last emitted
/// word), any number when it runs on past it.
fn head_room(frames: &[bool]) -> usize {
    let pause = PAUSE_MS / FRAME_MS;
    let (mut end, mut quiet) = (0, 0);
    for (i, &speech) in frames.iter().enumerate() {
        if speech {
            (end, quiet) = (i + 1, 0);
        } else {
            quiet += 1;
            if quiet >= pause {
                break;
            }
        }
    }
    match (end * FRAME_MS) as i64 {
        0 => 0,
        ms if ms <= TRIM_GUARD_MS * 2 => 1,
        _ => usize::MAX,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn tok(t: &str, s: i64, e: i64) -> StreamToken { StreamToken { text: t.into(), start_ms: s, end_ms: e } }

    #[test]
    fn first_decode_commits_nothing() {
        let mut w = StreamWindow::new();
        w.push_audio(&vec![0.0; SAMPLE_RATE]); // 1s
        let r = w.ingest_decode(vec![tok("hello", 0, 500)]);
        assert!(r.committed.is_empty());
        assert_eq!(r.interim.len(), 1);
    }

    #[test]
    fn second_agreeing_decode_commits_prefix_and_trims() {
        let mut w = StreamWindow::new();
        w.push_audio(&vec![0.1; SAMPLE_RATE * 2]); // 2s
        let _ = w.ingest_decode(vec![tok("hello", 0, 500), tok("world", 500, 1000)]);
        let r = w.ingest_decode(vec![tok("hello", 0, 500), tok("world", 500, 1000), tok("again", 1000, 1500)]);
        assert_eq!(r.committed.iter().map(|t| t.text.as_str()).collect::<Vec<_>>(), vec!["hello", "world"]);
        assert_eq!(r.interim.iter().map(|t| t.text.as_str()).collect::<Vec<_>>(), vec!["again"]);
        // Trim stays TRIM_GUARD_MS clear of the uncommitted "again" at 1000ms.
        assert_eq!(w.origin_ms(), 1000 - TRIM_GUARD_MS);
    }

    #[test]
    fn an_uncommitted_word_keeps_its_audio_through_the_trim() {
        // A word that never reaches agreement keeps its audio through the trim.
        let mut w = StreamWindow::new();
        w.push_audio(&vec![0.1; SAMPLE_RATE * 3]);
        let first = vec![
            tok("there's", 0, 400),
            tok("an", 400, 800),
            tok("azdo", 800, 1200),
            tok("created", 1200, 1600),
        ];
        let second = vec![
            tok("there's", 0, 400),
            tok("an", 400, 800),
            tok("azdul", 800, 1200),
            tok("created", 1200, 1600),
        ];
        let _ = w.ingest_decode(first);
        let r = w.ingest_decode(second);
        assert_eq!(
            r.committed.iter().map(|t| t.text.as_str()).collect::<Vec<_>>(),
            vec!["there's", "an"],
            "agreement must stop at the disagreeing word"
        );
        assert!(
            w.origin_ms() <= 800 - TRIM_GUARD_MS,
            "trim cut at {}ms must stay clear of the uncommitted word at 800ms",
            w.origin_ms()
        );
    }

    #[test]
    fn the_guard_span_does_not_re_emit_words_it_kept() {
        // The overlap check keeps words from committing twice after a guarded trim.
        let mut w = StreamWindow::new();
        w.push_audio(&vec![0.1; SAMPLE_RATE * 4]);
        let d1 = vec![tok("which", 0, 300), tok("I", 300, 600), tok("take", 600, 900),
                      tok("azdo", 900, 1200)];
        let d2 = vec![tok("which", 0, 300), tok("I", 300, 600), tok("take", 600, 900),
                      tok("azdul", 900, 1200)];
        let _ = w.ingest_decode(d1);
        let r = w.ingest_decode(d2);
        assert_eq!(
            r.committed.iter().map(|t| t.text.as_str()).collect::<Vec<_>>(),
            vec!["which", "I", "take"]
        );
        // The trim kept "take"'s tail; a decode of the new window repeats it.
        let origin = w.origin_ms();
        let d3 = vec![tok("take", 0, 100), tok("azdul", 100, 400), tok("two", 400, 700)];
        let _ = w.ingest_decode(d3.clone());
        let r = w.ingest_decode(d3);
        let texts: Vec<&str> = r.committed.iter().map(|t| t.text.as_str()).collect();
        assert!(
            !texts.starts_with(&["take"]),
            "re-emitted the word the guard span kept: {texts:?} (origin {origin}ms)"
        );
        assert!(texts.contains(&"azdul"), "lost the never-agreed word: {texts:?}");
    }

    #[test]
    fn a_long_repeat_across_the_guarded_trim_does_not_re_emit() {
        // The remembered tail covers a repeat run of any length.
        let mut w = StreamWindow::new();
        w.push_audio(&vec![0.1; SAMPLE_RATE * 4]);
        let d1 = vec![
            tok("please", 0, 300), tok("note", 300, 600), tok("that", 600, 900),
            tok("this", 900, 1200), tok("call", 1200, 1500), tok("will", 1500, 1800),
            tok("be", 1800, 2100), tok("azdo", 2100, 2400),
        ];
        let d2 = vec![
            tok("please", 0, 300), tok("note", 300, 600), tok("that", 600, 900),
            tok("this", 900, 1200), tok("call", 1200, 1500), tok("will", 1500, 1800),
            tok("be", 1800, 2100), tok("azdul", 2100, 2400),
        ];
        let _ = w.ingest_decode(d1);
        let r = w.ingest_decode(d2);
        assert_eq!(r.committed.len(), 7, "the agreed prefix must commit");
        // The retained span re-decodes with the repeat run in front.
        let d3 = vec![
            tok("this", 0, 60), tok("call", 60, 120), tok("will", 120, 180),
            tok("be", 180, 240), tok("azdul", 240, 500), tok("monitored", 500, 800),
        ];
        let _ = w.ingest_decode(d3.clone());
        let r = w.ingest_decode(d3);
        let texts: Vec<&str> = r.committed.iter().map(|t| t.text.as_str()).collect();
        assert!(
            !texts.starts_with(&["this"]),
            "re-emitted a 4-word run already committed: {texts:?}"
        );
        assert!(texts.contains(&"azdul"), "lost the never-agreed word: {texts:?}");
        assert!(texts.contains(&"monitored"), "lost the new word: {texts:?}");
    }

    #[test]
    fn a_leading_variant_word_does_not_defeat_the_guard_strip() {
        // A variant first word does not block the strip of the verbatim run behind it.
        let mut w = StreamWindow::new();
        w.push_audio(&vec![0.1; SAMPLE_RATE * 4]);
        let d1 = vec![
            tok("monitored", 0, 400), tok("or", 400, 800), tok("recorded", 800, 1200),
            tok("for", 1200, 1600), tok("quality", 1600, 2000), tok("azdo", 2000, 2400),
        ];
        let d2 = vec![
            tok("monitored", 0, 400), tok("or", 400, 800), tok("recorded", 800, 1200),
            tok("for", 1200, 1600), tok("quality", 1600, 2000), tok("azdul", 2000, 2400),
        ];
        let _ = w.ingest_decode(d1);
        let r = w.ingest_decode(d2);
        assert_eq!(r.committed.len(), 5, "the agreed prefix must commit");
        let d3 = vec![
            tok("are", 0, 60), tok("recorded", 60, 120), tok("for", 120, 180),
            tok("quality", 180, 240), tok("azdul", 240, 500), tok("training", 500, 800),
        ];
        let _ = w.ingest_decode(d3.clone());
        let r = w.ingest_decode(d3);
        let texts: Vec<&str> = r.committed.iter().map(|t| t.text.as_str()).collect();
        assert!(
            !texts.contains(&"recorded"),
            "re-emitted a repeat hiding behind one variant word: {texts:?}"
        );
        assert!(texts.contains(&"azdul"), "lost the never-agreed word: {texts:?}");
        assert!(texts.contains(&"training"), "lost the new word: {texts:?}");
    }

    #[test]
    fn a_head_word_that_never_agrees_does_not_stall_the_window() {
        // Words that repeat at the same times commit even when the agreed prefix is empty.
        let passes = [
            vec![tok("across", 30, 730), tok("right?", 730, 1700), tok("What's", 1700, 2430),
                 tok("it", 2440, 2670), tok("up", 2670, 2780), tok("here?", 2960, 3770)],
            vec![tok("right?", 30, 1360), tok("Both", 1360, 2040), tok("AWS", 2440, 2550),
                 tok("and...", 2550, 4630)],
            vec![tok("for", 30, 430), tok("us", 440, 730), tok("right?", 730, 1910),
                 tok("Both", 2430, 2500), tok("AWS", 2500, 2780), tok("and", 2960, 3380),
                 tok("they're", 3380, 4410), tok("like.", 4410, 5440)],
            vec![tok("right?", 30, 770), tok("Both", 770, 1680), tok("Adolph,", 1680, 2780),
                 tok("and", 2960, 3220), tok("they're", 3220, 4190), tok("like...", 4190, 5450),
                 tok("it's", 5910, 6580), tok("a", 6600, 6770)],
        ];
        let mut w = StreamWindow::new();
        w.push_audio(&vec![0.1; SAMPLE_RATE * 3780 / 1000]);
        let mut committed: Vec<String> = Vec::new();
        for (i, pass) in passes.into_iter().enumerate() {
            if i > 0 {
                w.push_audio(&vec![0.1; SAMPLE_RATE]);
            }
            let r = w.ingest_decode(pass);
            committed.extend(r.committed.iter().map(|t| t.text.clone()));
        }
        assert_eq!(
            committed,
            ["right?", "Both", "Adolph,", "and", "they're", "like..."],
            "a head both decodes heard settles behind the shared run"
        );
        // The window trims behind the commit, guarded clear of "it's".
        assert_eq!(w.origin_ms(), 5450 - TRIM_GUARD_MS);
    }

    #[test]
    fn a_word_stretched_over_skipped_speech_does_not_trim_it_away() {
        // Audio read as a sentence by the earlier decode stays in the window.
        let mut w = StreamWindow::new();
        w.push_audio(&vec![0.1; SAMPLE_RATE * 8630 / 1000]);
        let _ = w.ingest_decode(vec![
            tok("violence.", 0, 2030), tok("So", 2200, 2320), tok("my", 2320, 2450),
            tok("question", 2450, 2990), tok("now,", 3000, 3670),
        ]);
        w.push_audio(&vec![0.1; SAMPLE_RATE * 2]);
        let r = w.ingest_decode(vec![tok("violence.", 0, 10_240)]);
        assert_eq!(r.committed.iter().map(|t| t.text.as_str()).collect::<Vec<_>>(), ["violence."]);
        assert_eq!(w.origin_ms(), 2200 - TRIM_GUARD_MS, "the cut stays before the skipped sentence");
    }

    #[test]
    fn a_head_only_the_newer_decode_heard_waits_one_pass() {
        // A word heard by only one decode commits nothing and trims nothing yet.
        let we_need = |t0: i64| {
            vec![tok("we", t0, t0 + 300), tok("need", t0 + 300, t0 + 600),
                 tok("to", t0 + 600, t0 + 900), tok("go", t0 + 900, t0 + 1200)]
        };
        let texts = |r: &CommitResult| r.committed.iter().map(|t| t.text.clone()).collect::<Vec<_>>();

        // A fragment of the last emitted word ("…cloud" leaves "-oud") read
        // once as "out": the next decode misses it.
        let mut w = StreamWindow::new();
        w.push_audio(&vec![0.1; SAMPLE_RATE * 2]);
        let _ = w.ingest_decode(we_need(300));
        w.push_audio(&vec![0.1; SAMPLE_RATE]);
        let mut d = vec![tok("out", 0, 150)];
        d.extend(we_need(300));
        d.push(tok("now", 1600, 1900));
        let r = w.ingest_decode(d);
        assert!(r.committed.is_empty(), "committed on one reading: {:?}", texts(&r));
        assert_eq!(w.origin_ms(), 0, "the head's audio is kept for the next decode");
        w.push_audio(&vec![0.1; SAMPLE_RATE]);
        let mut d = we_need(300);
        d.extend([tok("now", 1600, 1900), tok("please", 1900, 2300)]);
        let r = w.ingest_decode(d);
        assert_eq!(texts(&r), ["we", "need", "to", "go", "now"]);

        // A real first word the earlier decode missed: the next decode hears
        // it, and it commits in front of the run.
        let mut w = StreamWindow::new();
        w.push_audio(&vec![0.1; SAMPLE_RATE * 2]);
        let _ = w.ingest_decode(we_need(300));
        w.push_audio(&vec![0.1; SAMPLE_RATE]);
        let mut d = vec![tok("Hey", 0, 250)];
        d.extend(we_need(300));
        let r = w.ingest_decode(d.clone());
        assert!(r.committed.is_empty(), "committed on one reading: {:?}", texts(&r));
        w.push_audio(&vec![0.1; SAMPLE_RATE]);
        d.push(tok("now", 1600, 1900));
        let r = w.ingest_decode(d);
        assert_eq!(texts(&r), ["Hey", "we", "need", "to", "go"]);
    }

    #[test]
    fn an_utterance_opening_on_the_last_word_keeps_it() {
        // "That's right." commits whole and the cut lands at its end. The
        // next utterance opens on the same word ("Right, so we go"), stamped
        // inside the span the cut remembers: one matching word is no re-decode.
        let mut w = StreamWindow::new();
        w.push_audio(&vec![0.1; SAMPLE_RATE * 2]);
        let d = || vec![tok("That's", 0, 400), tok("right.", 400, 1000)];
        let _ = w.ingest_decode(d());
        let _ = w.ingest_decode(d());
        assert_eq!(w.origin_ms(), 1000);
        w.push_audio(&vec![0.1; SAMPLE_RATE * 2]);
        let n = || {
            vec![tok("Right,", 200, 500), tok("so", 500, 700), tok("we", 700, 900),
                 tok("go", 900, 1200), tok("now", 1300, 1600)]
        };
        let _ = w.ingest_decode(n());
        let r = w.ingest_decode(n());
        assert_eq!(
            r.committed.iter().map(|t| t.text.as_str()).collect::<Vec<_>>(),
            ["Right,", "so", "we", "go", "now"]
        );
    }

    #[test]
    fn a_head_both_decodes_heard_commits_the_newer_reading() {
        // Both decodes hear speech in front of the shared run and read it differently: the newer reading commits.
        let mut w = StreamWindow::new();
        w.push_audio(&vec![0.1; SAMPLE_RATE * 3]);
        let _ = w.ingest_decode(vec![
            tok("uh", 0, 500), tok("we", 1000, 1300), tok("need", 1300, 1600), tok("to", 1600, 1900),
            tok("go", 1900, 2200), tok("home", 2400, 2800), tok("then", 2900, 3200),
        ]);
        w.push_audio(&vec![0.1; SAMPLE_RATE * 3 / 2]);
        let r = w.ingest_decode(vec![
            tok("so", 0, 500), tok("um", 600, 900), tok("we", 1800, 2100), tok("need", 2100, 2400),
            tok("to", 2400, 2700), tok("go", 2700, 3000), tok("home", 3200, 3600),
            tok("now", 3700, 4000),
        ]);
        assert_eq!(
            r.committed.iter().map(|t| t.text.as_str()).collect::<Vec<_>>(),
            ["so", "um", "we", "need", "to", "go", "home"]
        );
        assert_eq!(w.origin_ms(), 3600 - TRIM_GUARD_MS, "committed words kept past the guard");
    }

    #[test]
    fn words_stamped_short_are_not_committed_twice() {
        // Words whose stamps cover only part of their speech do not commit twice.
        let said = || vec![tok("That's", 0, 360), tok("right.", 400, 1000), tok("Sorry.", 1000, 1000)];
        let mut w = StreamWindow::new();
        w.push_audio(&vec![0.1; SAMPLE_RATE * 2]);
        let _ = w.ingest_decode(said());
        let r = w.ingest_decode(said());
        assert_eq!(r.committed.len(), 3);
        assert_eq!(w.origin_ms(), 1000);
        w.push_audio(&vec![0.1; SAMPLE_RATE]);
        let again = || vec![tok("That's", 190, 1060), tok("right.", 1060, 2000), tok("Sorry.", 2000, 2000)];
        let _ = w.ingest_decode(again());
        let r = w.ingest_decode(again());
        assert!(r.committed.is_empty(), "committed twice: {:?}", r.committed);
    }

    #[test]
    fn a_cut_that_drops_nothing_leaves_no_guard_behind() {
        // A cut at the window's start retains nothing; a repeat of the last word commits.
        let mut w = StreamWindow::new();
        w.push_audio(&vec![0.1; SAMPLE_RATE * 2]);
        let _ = w.ingest_decode(vec![tok("no", 0, 80), tok("so", 100, 300)]);
        let d = vec![tok("no", 0, 80), tok("no", 100, 300), tok("way", 300, 700)];
        let first = w.ingest_decode(d.clone());
        assert_eq!(first.committed.iter().map(|t| t.text.as_str()).collect::<Vec<_>>(), ["no"]);
        assert_eq!(w.origin_ms(), 0, "the guarded cut clamps to the window start");
        let r = w.ingest_decode(d);
        assert_eq!(
            r.committed.iter().map(|t| t.text.as_str()).collect::<Vec<_>>(),
            ["no", "way"],
            "the second \"no\" was stripped as a guard re-decode"
        );
    }

    fn speech(ms: usize) -> Vec<f32> { vec![0.1; SAMPLE_RATE * ms / 1000] }
    fn quiet(ms: usize) -> Vec<f32> { vec![0.0; SAMPLE_RATE * ms / 1000] }
    fn texts(r: &CommitResult) -> Vec<&str> { r.committed.iter().map(|t| t.text.as_str()).collect() }
    fn we_need_to_go(t0: i64) -> Vec<StreamToken> {
        vec![tok("we", t0, t0 + 300), tok("need", t0 + 300, t0 + 600),
             tok("to", t0 + 600, t0 + 900), tok("go", t0 + 900, t0 + 1200)]
    }

    #[test]
    fn a_phrase_said_again_after_a_pause_is_kept() {
        // "we need to go" commits whole, heard where it was stamped, and the
        // cut lands at its end. The speaker pauses and says it again; whisper
        // stamps the repeat at the window start, inside the pause. The window
        // opens on silence, so none of the emitted speech is left to decode
        // again: the repeat is new speech and commits.
        let mut w = StreamWindow::new();
        w.push_audio(&speech(1200));
        w.push_audio(&quiet(1100));
        let _ = w.ingest_decode(we_need_to_go(0));
        let r = w.ingest_decode(we_need_to_go(0));
        assert_eq!(texts(&r), ["we", "need", "to", "go"]);
        assert_eq!(w.origin_ms(), 1200);
        w.push_audio(&quiet(400));
        w.push_audio(&speech(1200));
        let _ = w.ingest_decode(we_need_to_go(0));
        let r = w.ingest_decode(we_need_to_go(0));
        assert_eq!(texts(&r), ["we", "need", "to", "go"], "the repeat was stripped as a re-decode");
    }

    #[test]
    fn a_phrase_said_again_after_the_guard_span_is_kept() {
        // The cut is guarded: the retained span holds the end of "go", then
        // the pause. That fragment can decode again as "go" and nothing more;
        // a 4-word repeat after the pause is new speech.
        let setup = || {
            let mut w = StreamWindow::new();
            w.push_audio(&speech(1200));
            w.push_audio(&quiet(1400));
            let mut d1 = we_need_to_go(0);
            d1.push(tok("so", 2000, 2200));
            let mut d2 = we_need_to_go(0);
            d2.push(tok("uh", 2000, 2200));
            let _ = w.ingest_decode(d1);
            let r = w.ingest_decode(d2);
            assert_eq!(texts(&r), ["we", "need", "to", "go"]);
            assert_eq!(w.origin_ms(), 1200 - TRIM_GUARD_MS);
            w.push_audio(&quiet(300));
            w.push_audio(&speech(1200));
            w
        };
        let mut w = setup();
        let _ = w.ingest_decode(we_need_to_go(0));
        let r = w.ingest_decode(we_need_to_go(0));
        assert_eq!(texts(&r), ["we", "need", "to", "go"], "the repeat was stripped as a re-decode");

        // The fragment read back as the last emitted word is still a re-decode.
        let mut w = setup();
        let mut d = vec![tok("go", 0, 150)];
        d.extend(we_need_to_go(1850));
        let _ = w.ingest_decode(d.clone());
        let r = w.ingest_decode(d);
        assert_eq!(texts(&r), ["we", "need", "to", "go"]);
    }

    #[test]
    fn words_stamped_over_the_pause_before_them_are_not_committed_twice() {
        // Words stamped over a quiet lead-in are still a re-decode.
        let mut w = StreamWindow::new();
        w.push_audio(&quiet(1700));
        w.push_audio(&speech(1300));
        let d = || vec![tok("Both", 0, 420), tok("the", 420, 730), tok("above", 730, 1250),
                        tok("beings", 1700, 2300)];
        let _ = w.ingest_decode(d());
        let mut d2 = d();
        d2[2] = tok("adobe", 730, 1250);
        let r = w.ingest_decode(d2);
        assert_eq!(texts(&r), ["Both", "the"]);
        assert_eq!(w.origin_ms(), 730 - TRIM_GUARD_MS);
        w.push_audio(&speech(1000));
        let again = || vec![tok("Both", 0, 820), tok("the", 1110, 1440), tok("creators", 1440, 2400),
                            tok("and", 2400, 2900), tok("they", 2900, 3300)];
        let _ = w.ingest_decode(again());
        let r = w.ingest_decode(again());
        assert_eq!(texts(&r), ["creators", "and", "they"]);
    }

    /// Two decodes over the window share a word only every third slot (no
    /// run long enough to anchor); the words are 500 ms apart.
    fn grid(n: usize, own: &str) -> Vec<StreamToken> {
        (0..n)
            .map(|i| {
                let t = if i % 3 == 1 { format!("s{i}") } else { format!("{own}{i}") };
                tok(&t, i as i64 * 500, (i as i64 + 1) * 500)
            })
            .collect()
    }

    #[test]
    fn the_cap_trim_does_not_cut_away_words_one_decode_missed() {
        // At the cap nothing agrees. The newer decode reads 10–20 s as one
        // stretched word; the decode before read twenty words there. The cut
        // stays before them, at a word both decodes end at the same place.
        let mut w = StreamWindow::new();
        w.push_audio(&speech(30_000));
        let _ = w.ingest_decode(grid(60, "p"));
        w.push_audio(&speech(1_000));
        let mut curr = grid(62, "c");
        curr.splice(20..40, [tok("river", 10_000, 20_000)]);
        let r = w.ingest_decode(curr);
        assert_eq!(r.committed.len(), 20, "{:?}", texts(&r));
        assert_eq!(r.committed.last().unwrap().text, "s19");
        assert_eq!(w.origin_ms(), 10_000 - TRIM_GUARD_MS, "audio of the missed words was cut away");
    }

    #[test]
    fn the_cap_trim_does_not_emit_a_word_whose_end_one_decode_misplaced() {
        // The newer decode ends "home" before the half-cap cut; the decode
        // before ends it after. Cutting at the half-cap point emits "home" and
        // keeps the rest of its audio, which the next decodes read again.
        let mut w = StreamWindow::new();
        w.push_audio(&speech(30_000));
        let mut prev = grid(60, "p");
        prev[31] = tok("home", 15_500, 16_400);
        prev[32] = tok("p32", 16_400, 16_500);
        let _ = w.ingest_decode(prev);
        w.push_audio(&speech(1_000));
        let mut curr = grid(62, "c");
        curr[31] = tok("home", 15_500, 15_950);
        let mut emitted: Vec<String> = Vec::new();
        let r = w.ingest_decode(curr);
        emitted.extend(r.committed.iter().map(|t| t.text.clone()));
        // The next decodes read the retained audio as it was spoken.
        let truth = [("x29", 14_500, 15_000), ("x30", 15_000, 15_500), ("home", 15_500, 16_400),
                     ("next", 16_400, 16_900), ("now", 16_900, 17_400)];
        let o = w.origin_ms();
        let read = || {
            truth
                .iter()
                .filter(|&&(_, _, e)| e > o)
                .map(|&(t, s, e)| tok(t, (s - o).max(0), e - o))
                .collect::<Vec<_>>()
        };
        w.push_audio(&speech(1_000));
        for _ in 0..2 {
            let r = w.ingest_decode(read());
            emitted.extend(r.committed.iter().map(|t| t.text.clone()));
        }
        assert_eq!(emitted.iter().filter(|t| *t == "home").count(), 1, "{emitted:?}");
        let at = emitted.iter().position(|t| t == "home").unwrap();
        assert_eq!(emitted[at + 1..], ["next", "now"], "{emitted:?}");
    }

    #[test]
    fn the_cap_trim_waits_for_a_second_decode() {
        // A window already past the cap on its first decode (a backlog
        // drained at once) has one reading only; the trim waits for the next.
        let mut w = StreamWindow::new();
        w.push_audio(&speech(31_000));
        let r = w.ingest_decode(grid(62, "p"));
        assert!(r.committed.is_empty());
        assert_eq!(w.origin_ms(), 0);
        w.push_audio(&speech(1_000));
        let _ = w.ingest_decode(grid(64, "c"));
        assert!(w.window_len_ms() <= MAX_WINDOW_MS);
    }

    #[test]
    fn a_small_commit_past_the_cap_extends_to_a_shared_word() {
        // Two agreed words commit on to the latest word both decodes end at the same place.
        let mut w = StreamWindow::new();
        w.push_audio(&speech(31_000));
        let mut d1 = grid(62, "p");
        d1[0] = tok("so", 0, 500);
        let _ = w.ingest_decode(d1);
        let mut d2 = grid(62, "c");
        d2[0] = tok("so", 0, 500);
        let r = w.ingest_decode(d2);
        assert_eq!(texts(&r)[..2], ["so", "s1"]);
        assert_eq!(texts(&r).last(), Some(&"s31"));
        assert_eq!(w.origin_ms(), 16_000 - TRIM_GUARD_MS);
    }

    #[test]
    fn the_cap_trim_after_a_trim_uses_the_reading_before_it() {
        // A commit trims the window; a backlog then takes it past the cap
        // before the next decode. That decode has no prefix history (the
        // trim reset it); the decode from before the trim is the second
        // reading of the audio, and the cap trims at once.
        let mut w = StreamWindow::new();
        w.push_audio(&speech(10_000));
        let mut d1 = grid(20, "p");
        d1[0] = tok("so", 0, 500);
        let _ = w.ingest_decode(d1);
        let mut d2 = grid(20, "c");
        d2[0] = tok("so", 0, 500);
        let r = w.ingest_decode(d2);
        assert_eq!(texts(&r), ["so", "s1"]);
        let o = w.origin_ms();
        w.push_audio(&speech(25_000));
        assert!(w.window_len_ms() > MAX_WINDOW_MS);
        let shifted: Vec<StreamToken> =
            grid(70, "q").into_iter().skip(2).map(|t| tok(&t.text, t.start_ms - o, t.end_ms - o)).collect();
        let _ = w.ingest_decode(shifted);
        assert!(w.window_len_ms() <= MAX_WINDOW_MS, "window {} ms", w.window_len_ms());
    }

    fn level(amp: f32, ms: usize) -> Vec<f32> { vec![amp; SAMPLE_RATE * ms / 1000] }

    #[test]
    fn a_louder_voice_after_the_cut_does_not_hide_the_retained_speech() {
        // A quiet talker (-45 dBFS) is cut mid-speech; the retained audio
        // holds the end of "c". A loud talker (-10 dBFS) joins afterwards.
        let quiet_talker = 0.0056;
        let mut w = StreamWindow::new();
        w.push_audio(&level(quiet_talker, 3_000));
        let abc = |last: &str| {
            vec![tok("a", 0, 500), tok("b", 500, 1000), tok("c", 1000, 1500), tok(last, 1600, 2000)]
        };
        let _ = w.ingest_decode(abc("x"));
        let r = w.ingest_decode(abc("y"));
        assert_eq!(texts(&r), ["a", "b", "c"]);
        assert_eq!(w.origin_ms(), 1500 - TRIM_GUARD_MS);
        w.push_audio(&level(0.316, 2_000));
        let d = || vec![tok("c", 0, 150), tok("d", 200, 600), tok("f", 1700, 2100)];
        let _ = w.ingest_decode(d());
        let r = w.ingest_decode(d());
        assert_eq!(texts(&r), ["d", "f"]);
    }

    #[test]
    fn a_quiet_talker_saying_a_phrase_again_after_a_pause_is_kept() {
        // As a_phrase_said_again_after_a_pause_is_kept, at a quiet talker's
        // level (-45 dBFS) over a room noise floor (-54 dBFS): the pause is
        // the noise, not silence, and still reads as a pause.
        let (talker, room) = (0.0056, 0.002);
        let mut w = StreamWindow::new();
        w.push_audio(&level(talker, 1200));
        w.push_audio(&level(room, 1100));
        let _ = w.ingest_decode(we_need_to_go(0));
        let r = w.ingest_decode(we_need_to_go(0));
        assert_eq!(texts(&r), ["we", "need", "to", "go"]);
        w.push_audio(&level(room, 400));
        w.push_audio(&level(talker, 1200));
        let _ = w.ingest_decode(we_need_to_go(0));
        let r = w.ingest_decode(we_need_to_go(0));
        assert_eq!(texts(&r), ["we", "need", "to", "go"], "the repeat was stripped as a re-decode");
    }

    #[test]
    fn a_head_word_agreed_at_the_window_start_does_not_stall_the_cap() {
        // Decodes agree only on a word stamped in the first 150 ms. Its
        // guarded cut lands at the window start and drops nothing, so the
        // word stays committed and every later decode agrees on it again.
        // The cap must still trim the window.
        let reading = |own: &str| {
            let mut d = vec![tok("um", 0, 100)];
            d.extend(grid(70, own).into_iter().map(|t| tok(&t.text, t.start_ms + 100, t.end_ms + 100)));
            d
        };
        let mut w = StreamWindow::new();
        w.push_audio(&speech(31_000));
        let _ = w.ingest_decode(reading("p"));
        for (i, own) in ["c", "q", "r", "t"].into_iter().enumerate() {
            w.push_audio(&speech(1_000));
            let _ = w.ingest_decode(reading(own));
            assert!(w.window_len_ms() <= MAX_WINDOW_MS, "pass {i}: window {} ms", w.window_len_ms());
        }
    }

    #[test]
    fn the_forced_cap_trim_strips_a_re_read_of_the_last_emission() {
        // A re-read word is not among the front words the forced cut emits.
        let mut w = StreamWindow::new();
        w.push_audio(&speech(2_000));
        let d = |last: &str| vec![tok("so", 0, 500), tok("well", 500, 1000), tok(last, 1200, 1500)];
        let _ = w.ingest_decode(d("x"));
        let r = w.ingest_decode(d("y"));
        assert_eq!(texts(&r), ["so", "well"]);
        w.push_audio(&speech(30_000));
        let r = w.ingest_decode(vec![tok("well", 0, 200), tok("q1", 300, 800), tok("q2", 800, 1300),
                                     tok("q3", 20_000, 31_000)]);
        assert_eq!(texts(&r), ["q1", "q2"]);
    }

    #[test]
    fn the_trim_still_makes_progress_on_agreeing_speech() {
        // Ordinary speech commits and the origin advances.
        let mut w = StreamWindow::new();
        w.push_audio(&vec![0.1; SAMPLE_RATE * 5]);
        let toks = |n: i64| {
            (0..n)
                .map(|i| tok("word", i * 500, (i + 1) * 500))
                .collect::<Vec<_>>()
        };
        let _ = w.ingest_decode(toks(4));
        let _ = w.ingest_decode(toks(5));
        assert!(w.origin_ms() > 0, "window never advanced");
    }

    #[test]
    fn committed_tokens_carry_absolute_time_after_trim() {
        let mut w = StreamWindow::new();
        w.push_audio(&vec![0.1; SAMPLE_RATE * 3]);
        let _ = w.ingest_decode(vec![tok("a", 0, 400), tok("b", 400, 800)]);
        let _ = w.ingest_decode(vec![tok("a", 0, 400), tok("b", 400, 800)]); // commits a,b; origin→800
        w.push_audio(&vec![0.1; SAMPLE_RATE]); // +1s
        let _ = w.ingest_decode(vec![tok("c", 0, 300)]);
        let r = w.ingest_decode(vec![tok("c", 0, 300)]);
        assert_eq!(r.committed[0].text, "c");
        assert_eq!(r.committed[0].start_ms, 800); // 0 (rel) + 800 (origin)
    }

    #[test]
    fn inflated_commit_end_does_not_wipe_uncommitted_audio() {
        // A committed prefix ending in the padding does not trim away uncommitted words.
        let mut w = StreamWindow::new();
        w.push_audio(&vec![0.1; SAMPLE_RATE * 32 / 10]); // 3.2s
        let d1 = vec![tok("and", 0, 100), tok("this", 100, 200), tok("is", 200, 3200), tok("justice", 3200, 3200)];
        let d2 = vec![tok("and", 0, 100), tok("this", 100, 200), tok("is", 200, 3200), tok("just", 3200, 3200), tok("a", 3200, 3200), tok("test", 3200, 3200)];
        let _ = w.ingest_decode(d1);
        let r = w.ingest_decode(d2);
        assert_eq!(
            r.committed.iter().map(|t| t.text.as_str()).collect::<Vec<_>>(),
            vec!["and", "this", "is"]
        );
        // Cut would be 3200 = whole window with words still uncommitted:
        // the window must be kept.
        assert_eq!(w.origin_ms(), 0);
        assert_eq!(w.window_len_ms(), 3200);
        // The kept window still commits the rest once decodes agree.
        let d3 = vec![tok("and", 0, 100), tok("this", 100, 200), tok("is", 200, 300), tok("just", 300, 400), tok("a", 400, 500), tok("test", 500, 600)];
        let r = w.ingest_decode(d3);
        assert_eq!(
            r.committed.iter().map(|t| t.text.as_str()).collect::<Vec<_>>(),
            vec!["just", "a", "test"]
        );
    }

    #[test]
    fn commit_cut_bounded_by_first_uncommitted_start() {
        // The committed token's end overshoots (bad stamp) but the first
        // uncommitted token's start is sane — the cut takes the minimum, so
        // the uncommitted word's audio survives the trim.
        let mut w = StreamWindow::new();
        w.push_audio(&vec![0.1; SAMPLE_RATE * 2]); // 2s
        let d1 = vec![tok("hello", 0, 900), tok("world", 900, 1900)];
        let d2 = vec![tok("hello", 0, 900), tok("world", 900, 1900), tok("again", 1200, 1500)];
        let _ = w.ingest_decode(d1);
        let _ = w.ingest_decode(d2);
        // "hello","world" commit; base = min(world.end 1900, again.start
        // 1200) = 1200 < 2000 → trim, held TRIM_GUARD_MS clear of "again".
        assert_eq!(w.origin_ms(), 1200 - TRIM_GUARD_MS);
    }

    #[test]
    fn all_tokens_committed_trims_even_at_window_end() {
        // Everything committed: a cut at the window's end discards nothing
        // that could still commit — trim proceeds.
        let mut w = StreamWindow::new();
        w.push_audio(&vec![0.1; SAMPLE_RATE]); // 1s
        let d = vec![tok("bye", 0, 1000)];
        let _ = w.ingest_decode(d.clone());
        let _ = w.ingest_decode(d);
        assert_eq!(w.origin_ms(), 1000);
        assert_eq!(w.window_len_ms(), 0);
    }

    #[test]
    fn force_trims_when_no_commit_and_over_cap() {
        let mut w = StreamWindow::new();
        w.push_audio(&vec![0.1; SAMPLE_RATE * 31]); // 31s > MAX_WINDOW_MS
        let _ = w.ingest_decode(vec![tok("x", 0, 100)]); // one reading → waits
        assert_eq!(w.origin_ms(), 0);
        // A second reading sharing no word with the first.
        let _ = w.ingest_decode(vec![tok("y", 0, 100)]);
        // Force-trim drops the front down to half-cap: a 31s window leaves
        // 15s, shifting the origin by 16s.
        assert_eq!(w.window_len_ms(), MAX_WINDOW_MS / 2); // 15_000 remaining
        assert_eq!(w.origin_ms(), 31_000 - MAX_WINDOW_MS / 2); // 16_000 shift
    }

    #[test]
    fn force_trim_stays_bounded_under_sustained_no_agreement_inflow() {
        // Decodes that never agree while audio keeps arriving faster than
        // 1s/decode. The window must stay bounded between trims.
        let mut w = StreamWindow::new();
        w.push_audio(&vec![0.1; SAMPLE_RATE * 31]); // start over cap
        let _ = w.ingest_decode(vec![tok("b", 0, 100)]); // first reading waits
        for i in 0..10 {
            w.push_audio(&vec![0.1; SAMPLE_RATE * 2]); // +2s inflow per decode
            let txt = if i % 2 == 0 { "a" } else { "b" }; // alternate → never agree
            let r = w.ingest_decode(vec![tok(txt, 0, 100)]);
            // Nothing agrees; the cap-trim force-emits the dropped front
            // token(s) as Final. Committed carries at most the single dropped
            // token.
            assert!(r.committed.len() <= 1, "force-emit bounded to dropped tokens (iter {i})");
            assert!(
                w.window_len_ms() <= MAX_WINDOW_MS,
                "window must stay bounded (iter {i}, len {}ms)",
                w.window_len_ms()
            );
        }
    }

    #[test]
    fn commit_resumes_after_a_force_trim() {
        // After a force-trim the window is back under cap; a fresh pair of
        // agreeing decodes commits again.
        let mut w = StreamWindow::new();
        w.push_audio(&vec![0.1; SAMPLE_RATE * 31]);
        let _ = w.ingest_decode(vec![tok("x", 0, 100)]);
        let _ = w.ingest_decode(vec![tok("y", 0, 100)]); // force-trim, prev cleared
        // Now under cap. Two agreeing decodes commit the agreed prefix.
        let _ = w.ingest_decode(vec![tok("hello", 0, 500)]);
        let r = w.ingest_decode(vec![tok("hello", 0, 500), tok("world", 500, 1000)]);
        assert_eq!(
            r.committed.iter().map(|t| t.text.as_str()).collect::<Vec<_>>(),
            vec!["hello"]
        );
    }

    #[test]
    fn word_level_commits_sustained_utterance_that_resegments() {
        // Word granularity commits the shared prefix across re-segmentation.
        use super::super::agreement::{agreed_prefix_len, split_into_words};

        // Two consecutive decodes of the same growing utterance, re-segmented
        // differently (one segment → two segments with a shifted boundary).
        let seg_d1 = vec![tok("the quick brown", 0, 1500)];
        let seg_d2 = vec![tok("the quick", 0, 1000), tok("brown fox", 1000, 2000)];

        // Segment granularity: the leading segment text differs ("the quick
        // brown" vs "the quick") → agreement is 0.
        assert_eq!(agreed_prefix_len(&seg_d1, &seg_d2), 0, "segment-level cannot agree");

        // Word granularity: feed the same two decodes split into words.
        let mut w = StreamWindow::new();
        w.push_audio(&vec![0.1; SAMPLE_RATE * 3]);
        let r1 = w.ingest_decode(split_into_words(seg_d1));
        assert!(r1.committed.is_empty(), "first decode has no prior to agree against");
        let r2 = w.ingest_decode(split_into_words(seg_d2));
        let committed: Vec<&str> = r2.committed.iter().map(|t| t.text.as_str()).collect();
        assert_eq!(committed, vec!["the", "quick", "brown"], "shared leading words commit");
    }

    #[test]
    fn cap_trim_force_emits_front_words_of_one_long_unagreed_segment() {
        // A single long segment is split into words; the front words end before the cut and are force-emitted.
        use super::super::agreement::split_into_words;
        let mut w = StreamWindow::new();
        w.push_audio(&vec![0.2; SAMPLE_RATE * 31]); // 31s > MAX_WINDOW_MS
        let _ = w.ingest_decode(vec![tok("hm", 0, 31_000)]); // shares no word
        // One segment covering the full 31s window (the un-agreed sustained case).
        let seg = vec![tok("one two three four five six seven eight", 0, 31_000)];
        let r = w.ingest_decode(split_into_words(seg));
        // cut_ms = 31_000 - 30_000/2 = 16_000. Words are ~3875ms each across
        // 8 words; the first ~4 words end at/under 16_000 → force-committed.
        assert!(!r.committed.is_empty(), "front words force-emitted, not silently dropped");
        let committed_texts: Vec<&str> = r.committed.iter().map(|t| t.text.as_str()).collect();
        assert_eq!(committed_texts.first(), Some(&"one"), "force-emit starts at the front");
        // Every committed word ends at/before the cut; later words stay interim.
        assert!(r.committed.iter().all(|t| t.end_ms <= 16_000), "committed = front of cut");
        assert!(r.interim.iter().any(|t| t.text == "eight"), "tail words stay interim, not lost");
        assert!(w.window_len_ms() <= MAX_WINDOW_MS, "window bounded after trim");
    }

    #[test]
    fn cap_forced_trim_emits_dropped_tokens_as_committed() {
        // Sustained speech, no stable agreement: the window is pushed past
        // MAX_WINDOW_MS with committed_in_window == 0. The cap-trim emits the
        // tokens covering the trimmed-away front region as committed (Final).
        let mut w = StreamWindow::new();
        w.push_audio(&vec![0.2; SAMPLE_RATE * 31]); // 31s > MAX_WINDOW_MS
        let _ = w.ingest_decode(vec![tok("hm", 0, 31_000)]);
        // A decode that shares no word with the one before → nothing commits
        // via LocalAgreement. Tokens span the full 31s window, window-relative.
        let curr = vec![
            tok("alpha", 0, 5_000),
            tok("bravo", 5_000, 12_000),
            tok("charlie", 12_000, 20_000),
            tok("delta", 20_000, 31_000),
        ];
        let r = w.ingest_decode(curr);
        // cut_ms = 31_000 - 30_000/2 = 16_000 → tokens ending <= 16_000 are
        // dropped from the window and force-committed: alpha, bravo.
        let committed_texts: Vec<&str> = r.committed.iter().map(|t| t.text.as_str()).collect();
        assert_eq!(committed_texts, vec!["alpha", "bravo"], "front tokens force-committed before trim");
        // charlie/delta straddle or follow the cut → stay interim, not lost.
        assert!(r.interim.iter().any(|t| t.text == "charlie"));
        // Bounding invariant still holds.
        assert!(w.window_len_ms() <= MAX_WINDOW_MS);
    }
}
