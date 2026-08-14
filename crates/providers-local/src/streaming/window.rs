//! Rolling window state for serial streaming-whisper. Audio accumulates as
//! 16 kHz mono f32; after each decode the LocalAgreement-2 prefix commits and
//! the audio + token history preceding the last committed token is dropped.
//! The next decode re-reads only the still-uncommitted tail plus new audio.
//! The window is hard-capped at MAX_WINDOW_MS.

use super::agreement::{agreed_prefix_len, StreamToken};

pub const SAMPLE_RATE: usize = 16_000;
pub const MAX_WINDOW_MS: i64 = 30_000;
/// Kept in front of the first uncommitted word when trimming.
///
/// Word spans come from `split_into_words`, which divides a whisper segment's
/// span evenly across its words — an interpolation, not a measurement. Cutting
/// exactly at that boundary can slice into the following word's onset, and the
/// next decode then sees a clipped fragment and drops it. A word that never
/// reaches agreement (jargon and names decode differently every pass, so the
/// prefix never matches) gets nibbled by every trim until nothing decodable
/// is left. The margin costs a little re-decoded audio and makes the clip
/// structurally impossible.
const TRIM_GUARD_MS: i64 = 150;
/// How many trailing words of an emission are remembered for the guard-overlap
/// check. Covers the word runs whisper re-commits when the retained guard
/// audio re-decodes (up to 10 words observed in field replays); a run longer
/// than this passes through live and is caught by the finalize
/// boundary-repeat trim. Stripping stays scoped to the guard span, so genuine
/// repetition later in the stream is never touched.
const OVERLAP_TAIL_WORDS: usize = 12;

use transcript::text::norm_word;

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
    /// Set when a guarded trim retains audio for words already emitted:
    /// `(normalized tail words, absolute end of the retained guard span)`.
    /// The retained audio decodes again, so without this the word commits a
    /// second time and the transcript stutters. Matching on text within the
    /// known guard span — rather than on a time high-water mark — keeps this
    /// safe when whisper re-segments and corrects a bogus stamp, which is
    /// exactly when a time-based rule would suppress real speech.
    guard_overlap: Option<(Vec<String>, i64)>,
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
        let agreed = agreed_prefix_len(&self.prev_tokens, &curr);
        let newly = agreed.saturating_sub(self.committed_in_window);

        let origin = self.origin_ms;
        let to_abs = |t: &StreamToken| StreamToken {
            text: t.text.clone(),
            start_ms: t.start_ms + origin,
            end_ms: t.end_ms + origin,
        };

        let mut committed: Vec<StreamToken> =
            curr.iter().skip(self.committed_in_window).take(newly).map(to_abs).collect();
        // Strip a leading run that repeats the tail of the last emission. The
        // decode right after a trim commits nothing (agreement needs two
        // passes), so the pending overlap is only consumed once something
        // actually commits — clearing it eagerly would let the repeat through.
        if !committed.is_empty() {
            if let Some((tail, guard_end_ms)) = self.guard_overlap.take() {
                if committed[0].start_ms < guard_end_ms {
                    // The repeat can hide behind one variant word (a re-decode
                    // that turns "or" into "are"); the variant is stripped
                    // only together with a ≥3-word verbatim run behind it.
                    let strip = [0usize, 1]
                        .into_iter()
                        .filter_map(|skip| {
                            let min_k = if skip == 0 { 1 } else { 3 };
                            let max_k =
                                tail.len().min(committed.len().saturating_sub(skip));
                            (min_k..=max_k)
                                .rev()
                                .find(|&k| {
                                    tail[tail.len() - k..]
                                        .iter()
                                        .zip(&committed[skip..skip + k])
                                        .all(|(w, t)| *w == norm_word(&t.text))
                                })
                                .map(|k| skip + k)
                        })
                        .next();
                    committed.drain(..strip.unwrap_or(0));
                }
            }
        }
        let interim: Vec<StreamToken> =
            curr.iter().skip(agreed).map(to_abs).collect();

        // Tail of what this decode emits, for the next guarded trim to match.
        let emitted_tail: Vec<String> = committed
            .iter()
            .rev()
            .take(OVERLAP_TAIL_WORDS)
            .rev()
            .map(|t| norm_word(&t.text))
            .collect();
        self.committed_in_window = agreed;
        self.prev_tokens = curr;

        // Trim audio + token history past the committed prefix. The cut is
        // the last committed token's end, bounded by the first uncommitted
        // token's start (whisper stamps either into its silence padding at
        // times). A cut at/past the window's end while uncommitted tokens
        // remain would discard their audio before they could ever commit —
        // the window is kept instead; the MAX_WINDOW_MS cap still bounds it.
        // When nothing is committed yet and the window exceeds MAX_WINDOW_MS,
        // the front is force-trimmed.
        if self.committed_in_window > 0 {
            let mut base_ms = self.prev_tokens[self.committed_in_window - 1].end_ms;
            if let Some(first_uncommitted) = self.prev_tokens.get(self.committed_in_window) {
                base_ms = base_ms.min(first_uncommitted.start_ms);
            }
            let all_committed = self.committed_in_window == self.prev_tokens.len();
            // Whether trimming is safe at all is judged on the unguarded
            // boundary: a cut at/past the window end with words still
            // uncommitted means the stamps are bogus, and the window is kept
            // rather than discarding audio those words need. Applying the
            // guard first would pull such a cut just under the end and defeat
            // that check.
            if all_committed || base_ms < self.window_len_ms() {
                // Everything committed: no onset to protect, cut where the
                // words end. Otherwise stay TRIM_GUARD_MS clear of the first
                // uncommitted word.
                let cut_ms = if all_committed {
                    base_ms
                } else {
                    base_ms.saturating_sub(TRIM_GUARD_MS).max(0)
                };
                self.trim_front(cut_ms);
                // The retained span re-decodes; remember which word it holds
                // so the next commit does not emit it twice.
                if !all_committed && !emitted_tail.is_empty() {
                    self.guard_overlap =
                        Some((emitted_tail, self.origin_ms + TRIM_GUARD_MS * 2));
                }
            }
        } else if self.window_len_ms() > self.cap_ms {
            // Trim the front down to half-cap.
            let cut_ms = self.window_len_ms() - self.cap_ms / 2;
            // Before discarding the front audio, the tokens that cover the
            // trimmed-away region are force-emitted as committed (Final);
            // tokens still in the retained window stay interim.
            let forced: Vec<StreamToken> = self
                .prev_tokens
                .iter()
                .filter(|t| t.end_ms <= cut_ms)
                .map(|t| to_abs(t))
                .collect();
            let interim_after: Vec<StreamToken> = self
                .prev_tokens
                .iter()
                .filter(|t| t.end_ms > cut_ms)
                .map(|t| to_abs(t))
                .collect();
            self.trim_front(cut_ms);
            return CommitResult { committed: forced, interim: interim_after };
        }

        CommitResult { committed, interim }
    }

    /// Drop `cut_ms` of audio from the front, shift the origin, and reset
    /// token bookkeeping; the next decode starts fresh from the new origin.
    fn trim_front(&mut self, cut_ms: i64) {
        let cut_ms = cut_ms.clamp(0, self.window_len_ms());
        let drop = (cut_ms as usize) * SAMPLE_RATE / 1000;
        if drop == 0 { return; }
        self.samples.drain(0..drop.min(self.samples.len()));
        self.origin_ms += cut_ms;
        self.prev_tokens.clear();
        self.committed_in_window = 0;
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
        // Jargon decodes differently every pass ("azdo" then "azdul"), so the
        // agreement prefix stops before it and it is never committed. Its
        // audio must survive the trim intact.
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
        // The guarded trim retains audio for words already emitted, so the
        // next decode sees them again. They must not commit twice — a live
        // transcript that stutters "I take take two" is the visible cost of
        // the guard, and the overlap check is what removes it.
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
        // A repeat run longer than a short remembered tail would never align
        // with the overlap strip; the tail length must cover it.
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
        // The re-decode's first word is a variant ("or"→"are"), so a strict
        // prefix match never fires even though the following 3-word run is a
        // verbatim repeat — the strip must look past one variant word.
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
    fn the_trim_still_makes_progress_on_agreeing_speech() {
        // The guard must not stall the window: ordinary speech commits and the
        // origin keeps advancing, or the window would grow to the cap.
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
        // The committed prefix can be a whole whisper segment whose end is
        // stamped into the silence padding (clamped to the window's end).
        // Trimming there would discard the audio of every uncommitted word.
        let mut w = StreamWindow::new();
        w.push_audio(&vec![0.1; SAMPLE_RATE * 32 / 10]); // 3.2s
        let d1 = vec![tok("and", 0, 100), tok("this", 100, 200), tok("is", 200, 3200), tok("just…", 3200, 3200)];
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
        let _ = w.ingest_decode(vec![tok("x", 0, 100)]); // no prior → no commit
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
        let _ = w.ingest_decode(vec![tok("x", 0, 100)]); // force-trim, prev cleared
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
        // A sustained solo utterance: as the window grows, whisper re-segments
        // it with different boundaries each decode, and segment-level
        // agreement never matches a prefix. At word granularity the leading
        // words are identical across re-segmentation and the shared word
        // prefix commits.
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
        // A sustained utterance that never agrees arrives as one long whisper
        // segment spanning the whole over-cap window. Split into words first,
        // the front words end before the cut and are force-emitted.
        use super::super::agreement::split_into_words;
        let mut w = StreamWindow::new();
        w.push_audio(&vec![0.2; SAMPLE_RATE * 31]); // 31s > MAX_WINDOW_MS, no prior
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
        // A decode that never agrees with the (empty) prior → nothing commits via
        // LocalAgreement. Tokens span the full 31s window, window-relative.
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
