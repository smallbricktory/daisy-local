// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! LocalAgreement-2: a token is committed once two consecutive decodes of the
//! (overlapping, growing) window agree on it as a prefix. Text is emitted as
//! final only after a later decode confirms it. When the two decodes read the
//! head of the window differently, a run of words they share at the same
//! place behind it settles the head (see [`anchored_agreement`]).

use transcript::text::norm_word;

/// A decoded unit (a word, in practice) with its time span (ms, relative to
/// the current window origin).
#[derive(Debug, Clone, PartialEq)]
pub struct StreamToken {
    pub text: String,
    pub start_ms: i64,
    pub end_ms: i64,
}

/// Longest common prefix length of two token slices, comparing words by
/// [`norm_word`]: case and surrounding punctuation do not count.
pub fn agreed_prefix_len(prev: &[StreamToken], curr: &[StreamToken]) -> usize {
    let mut n = 0;
    while n < prev.len() && n < curr.len() && norm_word(&prev[n].text) == norm_word(&curr[n].text)
    {
        n += 1;
    }
    n
}

/// Words in a row two decodes must share, at the same place, before the
/// words in front of them count as settled.
const ANCHOR_MIN_WORDS: usize = 3;
/// How far apart (ms) the two decodes may stamp a shared word and still be
/// reading the same audio.
const ANCHOR_MAX_SHIFT_MS: i64 = 1000;

/// A run of words two decodes share past a head they read differently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Anchor {
    /// End of the run in `curr`.
    pub end: usize,
    /// End of the run in `prev`.
    pub prev_end: usize,
}

/// Agreement past a head that the two decodes read differently: a run of
/// [`ANCHOR_MIN_WORDS`] or more words, starting at or after `from` in both,
/// that the two decodes share at the same place — first and last word
/// stamped within [`ANCHOR_MAX_SHIFT_MS`] of each other. Of all such runs the
/// one aligned closest in time wins, then the longer. `None` when they share
/// no such run.
///
/// The run settles the head in front of it, in `curr`'s reading, when both
/// decodes heard speech over that span or only `prev` did. A head only
/// `curr` heard is a single reading: `None`, and the next decode decides it.
pub fn anchored_agreement(prev: &[StreamToken], curr: &[StreamToken], from: usize) -> Option<Anchor> {
    let p: Vec<String> = prev.iter().map(|t| norm_word(&t.text)).collect();
    let c: Vec<String> = curr.iter().map(|t| norm_word(&t.text)).collect();
    let shift = |i: usize, j: usize| (prev[i].start_ms - curr[j].start_ms).abs();
    let mut best: Option<(i64, std::cmp::Reverse<usize>, usize, usize)> = None;
    for j in from..c.len() {
        for i in from..p.len() {
            if shift(i, j) > ANCHOR_MAX_SHIFT_MS {
                continue;
            }
            // A bare punctuation token is no evidence of shared speech.
            let mut run = p[i..]
                .iter()
                .zip(&c[j..])
                .take_while(|(a, b)| a == b && !a.is_empty())
                .count();
            while run > 0 && shift(i + run - 1, j + run - 1) > ANCHOR_MAX_SHIFT_MS {
                run -= 1;
            }
            let key = (shift(i, j), std::cmp::Reverse(run), j, i);
            if run >= ANCHOR_MIN_WORDS && best.is_none_or(|b| key < b) {
                best = Some(key);
            }
        }
    }
    let (_, std::cmp::Reverse(run), j, i) = best?;
    let settled = match (spoken_span(prev, &p, from..i), spoken_span(curr, &c, from..j)) {
        (_, None) => true,
        (Some((ps, pe)), Some((cs, ce))) => ps <= ce && cs <= pe,
        (None, Some(_)) => false,
    };
    settled.then_some(Anchor { end: j + run, prev_end: i + run })
}

/// Where a word stops in a decode's reading: its end, or the next word's
/// start when that comes first.
fn word_boundary(toks: &[StreamToken], k: usize) -> i64 {
    toks.get(k + 1).map_or(toks[k].end_ms, |n| n.start_ms.min(toks[k].end_ms))
}

/// The latest word both decodes read the same and stop at the same place: a
/// shared word whose boundaries (see [`word_boundary`]) lie within `tol_ms`
/// of each other and both inside `lo_ms..=hi_ms`. `None` when the decodes
/// share no such word.
pub fn shared_boundary(
    prev: &[StreamToken],
    curr: &[StreamToken],
    lo_ms: i64,
    hi_ms: i64,
    tol_ms: i64,
) -> Option<Anchor> {
    let p: Vec<String> = prev.iter().map(|t| norm_word(&t.text)).collect();
    (0..curr.len()).rev().find_map(|j| {
        let w = norm_word(&curr[j].text);
        let bc = word_boundary(curr, j);
        if w.is_empty() || bc < lo_ms || bc > hi_ms {
            return None;
        }
        (0..prev.len()).find_map(|i| {
            let bp = word_boundary(prev, i);
            (p[i] == w && (bc - bp).abs() <= tol_ms && (lo_ms..=hi_ms).contains(&bp))
                .then_some(Anchor { end: j + 1, prev_end: i + 1 })
        })
    })
}

/// Time span of the non-punctuation words in `range`; `None` when there are
/// none.
fn spoken_span(
    toks: &[StreamToken],
    words: &[String],
    range: std::ops::Range<usize>,
) -> Option<(i64, i64)> {
    let mut spoken = range.filter(|&k| !words[k].is_empty()).map(|k| &toks[k]);
    let first = spoken.next()?;
    let last = spoken.next_back().unwrap_or(first);
    Some((first.start_ms, last.end_ms))
}

/// Clamp segment spans to the decoded window's length.
pub fn clamp_to_window(mut segments: Vec<StreamToken>, window_ms: i64) -> Vec<StreamToken> {
    for s in &mut segments {
        s.start_ms = s.start_ms.clamp(0, window_ms);
        s.end_ms = s.end_ms.clamp(s.start_ms, window_ms);
    }
    segments
}

/// Split each multi-word token into one token per word, interpolating each
/// word's time span linearly across the token's span. A token of one word
/// passes through with its span.
pub fn split_into_words(segments: Vec<StreamToken>) -> Vec<StreamToken> {
    let mut out = Vec::new();
    for seg in &segments {
        // Strips CJK, kana and hangul characters.
        let cleaned = strip_cjk_hallucination(&seg.text);
        let words: Vec<&str> = cleaned.split_whitespace().collect();
        let k = words.len() as i64;
        if k == 0 {
            continue;
        }
        let span = seg.end_ms - seg.start_ms;
        for (i, w) in words.iter().enumerate() {
            let i = i as i64;
            let start = seg.start_ms + span * i / k;
            // The last word ends exactly at the segment end.
            let end = if i + 1 == k { seg.end_ms } else { seg.start_ms + span * (i + 1) / k };
            out.push(StreamToken { text: (*w).to_string(), start_ms: start, end_ms: end });
        }
    }
    out
}

/// Remove CJK ideographs, kana, and hangul. Pure; exported for tests.
pub fn strip_cjk_hallucination(text: &str) -> String {
    text.chars()
        .filter(|c| {
            !matches!(*c as u32,
                0x3040..=0x30FF   // hiragana + katakana
                | 0x3400..=0x4DBF // CJK ext A
                | 0x4E00..=0x9FFF // CJK unified
                | 0xAC00..=0xD7AF // hangul
                | 0xF900..=0xFAFF // CJK compat
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_bounds_inflated_end_to_window() {
        let out = clamp_to_window(vec![wtok2("and this is just a test", 0, 7500)], 3200);
        assert_eq!(out[0].start_ms, 0);
        assert_eq!(out[0].end_ms, 3200);
    }

    #[test]
    fn clamp_leaves_sane_spans_untouched() {
        let out = clamp_to_window(vec![wtok2("hello", 100, 900)], 3200);
        assert_eq!(out, vec![wtok2("hello", 100, 900)]);
    }

    #[test]
    fn clamp_handles_span_entirely_in_padding_and_negative_start() {
        let out = clamp_to_window(
            vec![wtok2("ghost", 4000, 9000), wtok2("early", -50, 200)],
            3200,
        );
        // Entirely-in-padding span collapses to a zero-length span at the
        // window edge; the text survives for agreement.
        assert_eq!((out[0].start_ms, out[0].end_ms), (3200, 3200));
        assert_eq!((out[1].start_ms, out[1].end_ms), (0, 200));
    }

    fn wtok2(t: &str, s: i64, e: i64) -> StreamToken {
        StreamToken { text: t.into(), start_ms: s, end_ms: e }
    }

    #[test]
    fn cjk_hallucinations_are_stripped() {
        assert_eq!(strip_cjk_hallucination("syndrome\u{5B50}s"), "syndromes");
        assert_eq!(strip_cjk_hallucination("plain english"), "plain english");
        assert_eq!(strip_cjk_hallucination("caf\u{00E9}"), "caf\u{00E9}");
    }

    fn tok(t: &str) -> StreamToken {
        StreamToken { text: t.into(), start_ms: 0, end_ms: 0 }
    }

    #[test]
    fn commits_common_prefix() {
        let prev = [tok("hello"), tok("world"), tok("foo")];
        let curr = [tok("hello"), tok("world"), tok("bar")];
        assert_eq!(agreed_prefix_len(&prev, &curr), 2);
    }

    #[test]
    fn no_agreement_commits_nothing() {
        assert_eq!(agreed_prefix_len(&[tok("a")], &[tok("b")]), 0);
    }

    #[test]
    fn full_agreement_commits_all_of_shorter() {
        let prev = [tok("a"), tok("b")];
        let curr = [tok("a"), tok("b"), tok("c")];
        assert_eq!(agreed_prefix_len(&prev, &curr), 2);
    }

    #[test]
    fn normalizes_case_and_space() {
        assert_eq!(agreed_prefix_len(&[tok(" Hello")], &[tok("hello ")]), 1);
    }

    #[test]
    fn punctuation_does_not_break_agreement() {
        assert_eq!(agreed_prefix_len(&[tok("across,"), tok("like.")], &[tok("Across"), tok("like...")]), 2);
    }

    #[test]
    fn a_head_only_the_newer_decode_heard_is_not_settled() {
        let prev = [wtok("right?", 30, 1360), wtok("Both", 1360, 2040), wtok("AWS", 2440, 2550),
                    wtok("and...", 2550, 4630)];
        let curr = [wtok("for", 30, 430), wtok("us", 440, 730), wtok("right?", 730, 1910),
                    wtok("Both", 2430, 2500), wtok("AWS", 2500, 2780), wtok("and", 2960, 3380),
                    wtok("they're", 3380, 4410)];
        assert_eq!(agreed_prefix_len(&prev, &curr), 0);
        assert_eq!(
            anchored_agreement(&prev, &curr, 0),
            None,
            "\"for us\" was heard by the newer decode only; the next decode decides it"
        );
    }

    #[test]
    fn a_head_both_decodes_heard_is_settled_by_the_run() {
        let prev = [wtok("across", 30, 740), wtok("right?", 820, 1960), wtok("Both", 2440, 2520),
                    wtok("AWS", 2520, 2770), wtok("and", 2960, 3350)];
        let curr = [wtok("for", 30, 430), wtok("us", 440, 730), wtok("right?", 730, 1910),
                    wtok("Both", 2430, 2500), wtok("AWS", 2500, 2780), wtok("and", 2960, 3380)];
        assert_eq!(
            anchored_agreement(&prev, &curr, 0),
            Some(Anchor { end: 6, prev_end: 5 })
        );
    }

    #[test]
    fn repeated_words_align_at_the_closest_place() {
        // Repeated words align at the closest place in time.
        let nos = |n: i64, t0: i64| (0..n).map(move |k| wtok("no", t0 + k * 250, t0 + (k + 1) * 250));
        let prev: Vec<_> = nos(6, 0).chain([wtok("yes", 1500, 1800)]).collect();
        let curr: Vec<_> =
            [wtok("hm", 0, 500)].into_iter().chain(nos(4, 500)).chain([wtok("yes", 1500, 1800)]).collect();
        assert_eq!(
            anchored_agreement(&prev, &curr, 0),
            Some(Anchor { end: 6, prev_end: 7 })
        );
    }

    #[test]
    fn the_closest_alignment_wins_over_an_earlier_one() {
        // Equal stamps line up word for word.
        let prev = [wtok("x", 0, 250), wtok("no", 250, 500), wtok("no", 500, 750),
                    wtok("no", 750, 1000), wtok("yes", 1000, 1250)];
        let curr = [wtok("no", 0, 250), wtok("no", 250, 500), wtok("no", 500, 750),
                    wtok("no", 750, 1000), wtok("yes", 1000, 1250), wtok("ok", 1400, 1700)];
        assert_eq!(anchored_agreement(&prev, &curr, 0), Some(Anchor { end: 5, prev_end: 5 }));
    }

    #[test]
    fn a_run_that_drifts_apart_settles_nothing() {
        // First words line up; the run's end does not.
        let prev = [wtok("yeah", 0, 300), wtok("yeah", 300, 600), wtok("yeah", 600, 900)];
        let curr = [wtok("oh", 0, 200), wtok("yeah", 200, 2000), wtok("yeah", 2000, 5000),
                    wtok("yeah", 5000, 9000)];
        assert_eq!(anchored_agreement(&prev, &curr, 0), None);
    }

    #[test]
    fn a_shared_run_somewhere_else_settles_nothing() {
        // The same phrase placed seconds apart by the two decodes.
        let prev = [wtok("he's", 0, 300), wtok("in", 300, 500), wtok("both", 500, 800),
                    wtok("environments", 800, 1500)];
        let curr = [wtok("yeah", 0, 300), wtok("um", 300, 2900), wtok("he's", 3000, 3300),
                    wtok("in", 3300, 3500), wtok("both", 3500, 3800)];
        assert_eq!(anchored_agreement(&prev, &curr, 0), None);
    }

    #[test]
    fn a_short_or_punctuation_run_settles_nothing() {
        let prev = [wtok("azdo", 0, 400), wtok("created", 400, 800), wtok("it", 800, 1000)];
        let curr = [wtok("azdul", 0, 400), wtok("created", 400, 800), wtok("it", 800, 1000)];
        assert_eq!(anchored_agreement(&prev, &curr, 0), None, "two shared words are too few");
        let prev = [wtok("a", 0, 100), wtok(".", 100, 200), wtok("...", 200, 300), wtok("?", 300, 400)];
        let curr = [wtok("b", 0, 100), wtok(".", 100, 200), wtok(".", 200, 300), wtok(".", 300, 400)];
        assert_eq!(anchored_agreement(&prev, &curr, 0), None, "punctuation is not speech");
    }

    #[test]
    fn empty_inputs() {
        assert_eq!(agreed_prefix_len(&[], &[tok("a")]), 0);
        assert_eq!(agreed_prefix_len(&[tok("a")], &[]), 0);
    }

    fn wtok(t: &str, s: i64, e: i64) -> StreamToken {
        StreamToken { text: t.into(), start_ms: s, end_ms: e }
    }

    #[test]
    fn splits_multiword_segment_with_interpolated_spans() {
        // 3 words across a 900ms span → equal thirds, monotonic, non-overlapping,
        // contained within the segment span.
        let out = split_into_words(vec![wtok("hello there world", 100, 1000)]);
        assert_eq!(out.len(), 3);
        assert_eq!(out.iter().map(|t| t.text.as_str()).collect::<Vec<_>>(), vec!["hello", "there", "world"]);
        assert_eq!(out[0].start_ms, 100);
        assert_eq!(out[0].end_ms, 400);
        assert_eq!(out[1].start_ms, 400);
        assert_eq!(out[1].end_ms, 700);
        assert_eq!(out[2].start_ms, 700);
        assert_eq!(out[2].end_ms, 1000); // last word ends exactly at segment end
    }

    #[test]
    fn single_word_segment_unchanged() {
        let out = split_into_words(vec![wtok("hello", 200, 800)]);
        assert_eq!(out, vec![wtok("hello", 200, 800)]);
    }

    #[test]
    fn multiple_segments_concatenate_in_order() {
        let out = split_into_words(vec![wtok("a b", 0, 200), wtok("c d", 200, 600)]);
        assert_eq!(out.iter().map(|t| t.text.as_str()).collect::<Vec<_>>(), vec!["a", "b", "c", "d"]);
        // Second segment's words keep its own span, after the first segment's.
        assert_eq!(out[2].start_ms, 200);
        assert_eq!(out[3].end_ms, 600);
    }

    #[test]
    fn collapses_internal_whitespace_no_empty_tokens() {
        let out = split_into_words(vec![wtok("  foo   bar  ", 0, 200)]);
        assert_eq!(out.iter().map(|t| t.text.as_str()).collect::<Vec<_>>(), vec!["foo", "bar"]);
    }

    #[test]
    fn empty_input_empty_output() {
        assert!(split_into_words(vec![]).is_empty());
    }

    #[test]
    fn whitespace_only_segment_yields_nothing() {
        assert!(split_into_words(vec![wtok("   ", 0, 200)]).is_empty());
    }
}
