//! Rolling window state for serial streaming-whisper. Audio accumulates as
//! 16 kHz mono f32; after each decode the LocalAgreement-2 prefix commits and
//! the audio + token history preceding the last committed token is dropped.
//! The next decode re-reads only the still-uncommitted tail plus new audio.
//! The window is hard-capped at MAX_WINDOW_MS.

use super::agreement::{agreed_prefix_len, StreamToken};

pub const SAMPLE_RATE: usize = 16_000;
pub const MAX_WINDOW_MS: i64 = 30_000;

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
}

impl Default for StreamWindow {
    fn default() -> Self { Self::new() }
}

impl StreamWindow {
    pub fn new() -> Self {
        Self { samples: Vec::new(), origin_ms: 0, prev_tokens: Vec::new(), committed_in_window: 0 }
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

        let committed: Vec<StreamToken> =
            curr.iter().skip(self.committed_in_window).take(newly).map(to_abs).collect();
        let interim: Vec<StreamToken> =
            curr.iter().skip(agreed).map(to_abs).collect();

        self.committed_in_window = agreed;
        self.prev_tokens = curr;

        // Trim audio + token history past the last committed token's end.
        // When nothing is committed yet and the window exceeds MAX_WINDOW_MS,
        // the front is force-trimmed.
        if self.committed_in_window > 0 {
            let cut_ms = self.prev_tokens[self.committed_in_window - 1].end_ms;
            self.trim_front(cut_ms);
        } else if self.window_len_ms() > MAX_WINDOW_MS {
            // Trim the front down to half-cap.
            let cut_ms = self.window_len_ms() - MAX_WINDOW_MS / 2;
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
        assert_eq!(w.origin_ms(), 1000);
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
