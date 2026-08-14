//! Age-margin commit state machine for parakeet streaming: a word becomes
//! final once its audio is older than the margin (it was decoded with at
//! least that much right context); younger words are the volatile caption
//! tail. Boundary-anchored LocalAgreement fails on parakeet output
//! (window-head re-hearing instability) — age-margin is the strategy.

use crate::streaming::agreement::StreamToken;

/// Timestamp-drift repeat guard: a re-heard copy of the last committed word
/// starting within this of that word's committed end is dropped.
const REPEAT_GUARD_MS: i64 = 300;

pub struct AgeMarginCommitter {
    committed_end_ms: i64,
    last: Option<StreamToken>,
}

impl AgeMarginCommitter {
    pub fn new() -> Self {
        Self { committed_end_ms: 0, last: None }
    }

    /// Feed one decode's words (absolute-time spans) and the commit horizon
    /// (`window_end - margin`; `i64::MAX` at flush). Returns the newly
    /// committed words: midpoint past the committed watermark AND `end_ms`
    /// at or before the horizon.
    pub fn commit(&mut self, words: &[StreamToken], horizon_ms: i64) -> Vec<StreamToken> {
        let mut out = Vec::new();
        for w in words {
            let mid = (w.start_ms + w.end_ms) / 2;
            if mid <= self.committed_end_ms || w.end_ms > horizon_ms {
                continue;
            }
            if let Some(last) = &self.last {
                if norm(&last.text) == norm(&w.text) && w.start_ms < last.end_ms + REPEAT_GUARD_MS {
                    continue;
                }
            }
            self.committed_end_ms = w.end_ms;
            self.last = Some(w.clone());
            out.push(w.clone());
        }
        out
    }
}

impl Default for AgeMarginCommitter {
    fn default() -> Self {
        Self::new()
    }
}

/// Case/punctuation-insensitive word identity for the repeat guard.
fn norm(w: &str) -> String {
    w.chars()
        .filter(|c| c.is_alphanumeric() || *c == '\'')
        .collect::<String>()
        .to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tok(t: &str, s: i64, e: i64) -> StreamToken {
        StreamToken { text: t.into(), start_ms: s, end_ms: e }
    }

    fn texts(v: &[StreamToken]) -> Vec<String> {
        v.iter().map(|t| t.text.clone()).collect()
    }

    #[test]
    fn commits_only_words_older_than_horizon() {
        let mut c = AgeMarginCommitter::new();
        let words = vec![
            tok("alpha", 0, 400),
            tok("bravo", 500, 900),
            tok("charlie", 9_600, 9_900), // younger than horizon
        ];
        let out = c.commit(&words, 9_000);
        assert_eq!(texts(&out), ["alpha", "bravo"]);
        // Next decode re-hears the same words shifted slightly: no re-commit.
        let out2 = c.commit(&[tok("bravo", 510, 910), tok("charlie", 9_600, 9_900)], 20_000);
        assert_eq!(texts(&out2), ["charlie"]);
    }

    #[test]
    fn drops_drifted_repeat_of_last_committed_word() {
        let mut c = AgeMarginCommitter::new();
        c.commit(&[tok("delta", 0, 400)], 1_000);
        // Drift re-surfaces "delta" starting 200ms after its committed end.
        let out = c.commit(&[tok("Delta.", 600, 950)], 2_000);
        assert!(out.is_empty(), "drifted repeat must not double-commit");
    }

    #[test]
    fn flush_commits_the_pending_tail() {
        let mut c = AgeMarginCommitter::new();
        let none = c.commit(&[tok("echo", 0, 400)], 200); // nothing old enough
        assert!(none.is_empty());
        let out = c.commit(&[tok("echo", 0, 400), tok("foxtrot", 500, 900)], i64::MAX);
        assert_eq!(texts(&out), ["echo", "foxtrot"]);
    }

    #[test]
    fn distinct_word_close_behind_last_is_kept() {
        let mut c = AgeMarginCommitter::new();
        c.commit(&[tok("golf", 0, 400)], 1_000);
        // Different word 100ms later: the repeat guard must not eat it.
        let out = c.commit(&[tok("hotel", 500, 900)], 2_000);
        assert_eq!(texts(&out), ["hotel"]);
    }
}
