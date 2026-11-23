// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! How transcript lines read as turns: sentence ends, sentence counts, and
//! the rule that joins a line onto the turn before it. The phone groups by
//! the same rule, so a session reads as the same turns on every device.

/// Two lines of one voice this close read as one turn.
pub const MERGE_GAP_MS: u32 = 2_000;
/// A turn holds at most this many sentences, so scrubbing by transcript
/// lands within a short stretch of audio. The next sentence starts a new
/// turn under the same label.
pub const MAX_TURN_SENTENCES: usize = 3;

/// Sentence ends in scripts that put a space after them.
const SPACED_ENDS: &[char] = &['.', '?', '!', '؟', '۔', '।', '॥'];
/// Full-width sentence ends, in scripts written without spaces.
const CLOSED_ENDS: &[char] = &['。', '？', '！', '｡'];
/// What may follow a sentence end and still belong to its sentence.
const CLOSERS: &[char] = &['"', '\'', ')', ']', '”', '’', '」', '』', '）', '》', '〉'];

/// Words a full stop shortens: titles, and the Latin abbreviations English
/// writes inside a sentence. Lowercased, without their last stop.
const ABBREVIATIONS: &[&str] = &[
    "mr", "mrs", "ms", "dr", "prof", "st", "mt", "jr", "sr", "rev", "fr", "gen", "col", "lt", "sgt",
    "capt", "gov", "sen", "rep", "vs", "e.g", "i.e", "cf", "approx",
];

/// Whether `text` ends on a sentence end: a full stop that closes no
/// abbreviation or initial, a question or exclamation mark, or a
/// full-width end.
pub fn ends_sentence(text: &str) -> bool {
    let core = text.trim_end_matches(|c: char| c.is_whitespace() || CLOSERS.contains(&c));
    let Some(end) = core.chars().last() else { return false };
    if CLOSED_ENDS.contains(&end) {
        return true;
    }
    if !SPACED_ENDS.contains(&end) {
        return false;
    }
    if end != '.' {
        return true;
    }
    let stem = &core[..core.len() - end.len_utf8()];
    let word = stem.rsplit(char::is_whitespace).next().unwrap_or("");
    let word = word.trim_start_matches(|c: char| !c.is_alphabetic());
    // An initial: one capital before the stop, as in "John F. Kennedy".
    // "I" ends sentences in speech ("so did I.") far more often than it
    // stands for a name.
    let mut letters = word.chars();
    if let (Some(only), None) = (letters.next(), letters.next()) {
        if only.is_uppercase() && only != 'I' {
            return false;
        }
    }
    !ABBREVIATIONS.contains(&word.to_lowercase().as_str())
}

/// How many sentences `text` ends, by the rules of `ends_sentence`: a
/// full-width end counts where it stands, any other end where a space or
/// the text's end follows its word.
pub fn sentence_count(text: &str) -> usize {
    let mut count = 0;
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if CLOSED_ENDS.contains(&c) {
            let mut end = i + c.len_utf8();
            while let Some(&(j, n)) = chars.peek() {
                if !CLOSERS.contains(&n) {
                    break;
                }
                end = j + n.len_utf8();
                chars.next();
            }
            count += 1;
            start = end;
        } else if c.is_whitespace() {
            if ends_sentence(&text[start..i]) {
                count += 1;
            }
            start = i;
        }
    }
    if ends_sentence(&text[start..]) {
        count += 1;
    }
    count
}

/// Whether `text` already holds `MAX_TURN_SENTENCES` whole sentences.
pub fn full(text: &str) -> bool {
    ends_sentence(text) && sentence_count(text) >= MAX_TURN_SENTENCES
}

/// Whether a line starting at `start_ms` reads on from the text before it,
/// which ends at `end_ms`; a voice is whoever the line is labelled as. A
/// line labelled as nobody sits under the label before it, so it joins. The
/// rest of an unfinished sentence joins however long the pause, unless
/// another voice says it. Whole sentences of one voice join within
/// `MERGE_GAP_MS`. The sentence cap is `joins`.
pub fn reads_on<V: PartialEq>(start_ms: u32, after: Option<V>, text: &str, end_ms: u32, before: Option<V>) -> bool {
    if !ends_sentence(text) {
        return before.is_none() || after.is_none() || before == after;
    }
    start_ms <= end_ms.saturating_add(MERGE_GAP_MS) && (after.is_none() || before == after)
}

/// `reads_on`, up to `MAX_TURN_SENTENCES` in a turn.
pub fn joins<V: PartialEq>(start_ms: u32, after: Option<V>, text: &str, end_ms: u32, before: Option<V>) -> bool {
    reads_on(start_ms, after, text, end_ms, before) && !full(text)
}

/// Two pieces of transcript as one line: a space between them, except where
/// either side is Chinese, Japanese, Thai, Lao, Khmer or Myanmar, which are
/// written without one between words.
pub fn join(left: &str, right: &str) -> String {
    match (left.chars().last(), right.chars().next()) {
        (Some(end), Some(start)) if !unspaced(end) && !unspaced(start) => format!("{left} {right}"),
        _ => format!("{left}{right}"),
    }
}

fn unspaced(c: char) -> bool {
    matches!(c as u32,
        0x3000..=0x303F     // CJK symbols and punctuation
        | 0x3040..=0x30FF   // hiragana, katakana
        | 0x3100..=0x312F   // bopomofo
        | 0x31F0..=0x31FF   // katakana extension
        | 0x3400..=0x4DBF   // CJK extension A
        | 0x4E00..=0x9FFF   // CJK unified ideographs
        | 0xF900..=0xFAFF   // CJK compatibility ideographs
        | 0xFF00..=0xFFEF   // full-width and half-width forms
        | 0x0E00..=0x0EFF   // Thai, Lao
        | 0x1000..=0x109F   // Myanmar
        | 0x1780..=0x17FF   // Khmer
        | 0x20000..=0x2FA1F // CJK extensions B and on
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sentence_count_follows_the_end_rules() {
        assert_eq!(sentence_count(""), 0);
        assert_eq!(sentence_count("and then we"), 0);
        assert_eq!(sentence_count("Hello there. How are you? Fine!"), 3);
        assert_eq!(sentence_count("Ask Dr. Baylor about version 3.5 with John F. Kennedy"), 0);
        assert_eq!(sentence_count("Ask Dr. Baylor. He knows."), 2);
        assert_eq!(sentence_count("He said \"go.\" Then left."), 2);
        assert_eq!(sentence_count("你好。我是小明。谢谢！"), 3);
        assert_eq!(sentence_count("今日は晴れです。散歩に行きますか？ Fine."), 3);
    }

    #[test]
    fn a_sentence_ends_at_a_stop_that_closes_no_title_or_initial() {
        assert!(ends_sentence("We shipped it."));
        assert!(ends_sentence("So did I."));
        assert!(ends_sentence("Really?\" "));
        assert!(!ends_sentence("Talk to Dr."));
        assert!(!ends_sentence("John F."));
        assert!(!ends_sentence("for example, e.g."));
        assert!(!ends_sentence("and then we"));
        assert!(!ends_sentence(""));
    }

    #[test]
    fn an_unfinished_sentence_joins_across_any_pause_unless_another_voice_says_it() {
        assert!(reads_on(90_000, Some(1), "and then we", 1_000, Some(1)));
        assert!(reads_on(90_000, None, "and then we", 1_000, Some(1)));
        assert!(reads_on(90_000, Some(1), "and then we", 1_000, None));
        assert!(!reads_on(1_100, Some(2), "and then we", 1_000, Some(1)));
    }

    #[test]
    fn whole_sentences_of_one_voice_join_within_the_gap() {
        assert!(reads_on(3_000, Some(1), "Done.", 1_000, Some(1)));
        assert!(!reads_on(3_001, Some(1), "Done.", 1_000, Some(1)));
        assert!(reads_on(3_000, None, "Done.", 1_000, Some(1)));
        assert!(!reads_on(1_100, Some(1), "Done.", 1_000, None));
        assert!(!reads_on(1_100, Some(2), "Done.", 1_000, Some(1)));
    }

    #[test]
    fn a_turn_closes_at_a_sentence_end_once_it_holds_three_sentences() {
        assert!(joins(1_100, Some(1), "One. Two.", 1_000, Some(1)));
        assert!(!joins(1_100, Some(1), "One. Two. Three.", 1_000, Some(1)));
        // Three sentences and an unfinished fourth: the fourth reads on.
        assert!(joins(1_100, Some(1), "One. Two. Three. And", 1_000, Some(1)));
    }

    #[test]
    fn join_spaces_words_except_in_scripts_written_without_spaces() {
        assert_eq!(join("Hello", "there"), "Hello there");
        assert_eq!(join("你好", "谢谢"), "你好谢谢");
        assert_eq!(join("Fine.", "今日"), "Fine.今日");
        assert_eq!(join("", "x"), "x");
    }
}
