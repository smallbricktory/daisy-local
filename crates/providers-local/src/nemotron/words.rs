// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! SentencePiece tokens → words. A `▁`-prefixed piece starts a word; every
//! other piece, punctuation included, appends to the current one. A word
//! spans its first token's time to one encoder frame past its last.
//! Standalone fillers ("um", "uh", …) never leave the assembler.

use super::model::FRAME_MS;
use super::stream::TimedToken;
use crate::streaming::agreement::StreamToken;

/// Words dropped whole, compared case-insensitively with leading and
/// trailing punctuation trimmed.
const FILLERS: [&str; 8] = ["um", "uh", "umm", "uhh", "uhm", "erm", "hmm", "mm"];

fn is_filler(text: &str) -> bool {
    let core = text.trim_matches(|c: char| !c.is_alphanumeric());
    FILLERS.iter().any(|f| core.eq_ignore_ascii_case(f))
}

fn spoken(w: StreamToken) -> Option<StreamToken> {
    Some(w).filter(|w| !w.text.is_empty() && !is_filler(&w.text))
}

#[derive(Default)]
pub struct WordAssembler {
    cur: Option<StreamToken>,
}

impl WordAssembler {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a token; returns the word it closed, if it starts a new one.
    pub fn push(&mut self, t: &TimedToken) -> Option<StreamToken> {
        if t.text == "<unk>" {
            return None;
        }
        let end_ms = t.ms + FRAME_MS;
        if let Some(rest) = t.text.strip_prefix('▁') {
            let done = self.cur.replace(StreamToken { text: rest.to_string(), start_ms: t.ms, end_ms });
            return done.and_then(spoken);
        }
        match &mut self.cur {
            Some(w) => {
                w.text.push_str(&t.text);
                w.end_ms = end_ms;
            }
            None => self.cur = Some(StreamToken { text: t.text.clone(), start_ms: t.ms, end_ms }),
        }
        None
    }

    /// The word still open.
    pub fn pending(&self) -> Option<&StreamToken> {
        self.cur.as_ref().filter(|w| !w.text.is_empty() && !is_filler(&w.text))
    }

    /// Closes and returns the open word.
    pub fn take(&mut self) -> Option<StreamToken> {
        self.cur.take().and_then(spoken)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tok(text: &str, ms: i64) -> TimedToken {
        TimedToken { text: text.into(), ms }
    }

    #[test]
    fn pieces_join_into_words_with_frame_spans() {
        let mut w = WordAssembler::new();
        assert!(w.push(&tok("▁Hel", 560)).is_none());
        assert!(w.push(&tok("lo", 640)).is_none());
        assert!(w.push(&tok(",", 720)).is_none());
        let hello = w.push(&tok("▁world", 1_120)).expect("a new word closes the last");
        assert_eq!((hello.text.as_str(), hello.start_ms, hello.end_ms), ("Hello,", 560, 800));
        assert!(w.push(&tok(".", 1_200)).is_none());
        let world = w.take().unwrap();
        assert_eq!((world.text.as_str(), world.start_ms, world.end_ms), ("world.", 1_120, 1_280));
        assert!(w.take().is_none());
    }

    #[test]
    fn unknown_and_bare_space_pieces_emit_nothing() {
        let mut w = WordAssembler::new();
        assert!(w.push(&tok("<unk>", 0)).is_none());
        assert!(w.push(&tok("▁", 80)).is_none());
        assert!(w.push(&tok("▁ok", 160)).is_none(), "an empty word is dropped");
        assert_eq!(w.pending().unwrap().text, "ok");
    }

    #[test]
    fn leading_continuation_piece_opens_a_word() {
        let mut w = WordAssembler::new();
        assert!(w.push(&tok("s", 0)).is_none());
        assert_eq!(w.take().unwrap().text, "s");
    }

    fn assemble(toks: &[TimedToken]) -> Vec<StreamToken> {
        let mut w = WordAssembler::new();
        let mut out: Vec<StreamToken> = toks.iter().filter_map(|t| w.push(t)).collect();
        out.extend(w.take());
        out
    }

    #[test]
    fn standalone_fillers_are_dropped() {
        for filler in ["▁um", "▁Uh", "▁UMM", "▁uhh", "▁uhm", "▁erm", "▁hmm", "▁mm"] {
            let words = assemble(&[tok("▁so", 0), tok(filler, 400), tok("▁yes", 800)]);
            let text: Vec<&str> = words.iter().map(|w| w.text.as_str()).collect();
            assert_eq!(text, ["so", "yes"], "{filler}");
        }
    }

    #[test]
    fn fillers_with_attached_punctuation_are_dropped() {
        let words = assemble(&[tok("▁Um", 0), tok(",", 80), tok("▁right", 400), tok("▁uh", 800), tok(".", 880)]);
        let text: Vec<&str> = words.iter().map(|w| w.text.as_str()).collect();
        assert_eq!(text, ["right"]);
    }

    #[test]
    fn words_spelled_with_filler_letters_stay() {
        let words = assemble(&[tok("▁um", 0), tok("brella", 80), tok("▁U", 400), tok("hura", 480), tok("▁hmm", 800), tok("m", 880)]);
        let text: Vec<&str> = words.iter().map(|w| w.text.as_str()).collect();
        assert_eq!(text, ["umbrella", "Uhura", "hmmm"]);
    }

    #[test]
    fn a_dropped_filler_leaves_its_neighbours_timing_alone() {
        let words = assemble(&[tok("▁so", 0), tok("▁um", 400), tok("▁yes", 800), tok("!", 880)]);
        let spans: Vec<(&str, i64, i64)> = words.iter().map(|w| (w.text.as_str(), w.start_ms, w.end_ms)).collect();
        assert_eq!(spans, [("so", 0, 80), ("yes!", 800, 960)]);
    }

    #[test]
    fn an_open_filler_is_not_pending() {
        let mut w = WordAssembler::new();
        assert!(w.push(&tok("▁uh", 0)).is_none());
        assert!(w.pending().is_none());
        assert!(w.push(&tok("▁okay", 400)).is_none(), "the closed filler is dropped");
        assert_eq!(w.pending().unwrap().text, "okay");
        assert!(w.push(&tok("▁Hmm", 800)).unwrap().text == "okay");
        assert!(w.take().is_none());
    }
}
