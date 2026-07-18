//! LocalAgreement-2: a token is committed once two consecutive decodes of the
//! (overlapping, growing) window agree on it as a prefix. Text is emitted as
//! final only after a later decode confirms it.

/// A decoded unit (a whisper segment, in practice) with its time span (ms,
/// relative to the current window origin).
#[derive(Debug, Clone, PartialEq)]
pub struct StreamToken {
    pub text: String,
    pub start_ms: i64,
    pub end_ms: i64,
}

/// Longest common prefix length of two token slices, comparing by normalized
/// text (trimmed, lowercased).
pub fn agreed_prefix_len(prev: &[StreamToken], curr: &[StreamToken]) -> usize {
    let mut n = 0;
    while n < prev.len() && n < curr.len() && norm(&prev[n].text) == norm(&curr[n].text) {
        n += 1;
    }
    n
}

fn norm(s: &str) -> String {
    s.trim().to_lowercase()
}

/// Split each whisper-segment token into one token per word, interpolating
/// each word's time span linearly across the segment's span. Per-word timing
/// is approximate; agreement compares word text.
pub fn split_into_words(segments: Vec<StreamToken>) -> Vec<StreamToken> {
    let mut out = Vec::new();
    for seg in &segments {
        // The live models are English-only (*.en); any CJK/kana/hangul
        // output is a low-confidence hallucination artifact, not speech.
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

/// Remove CJK ideographs, kana, and hangul — an English-only model never
/// legitimately emits them. Pure; exported for tests.
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
