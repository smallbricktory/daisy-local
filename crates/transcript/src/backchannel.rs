//! Detect "backchannel" / filler segments — short conversational
//! acknowledgements ("yeah", "mm-hmm", "oh", etc.). These are dropped from
//! the mic track when `DedupParams::drop_backchannels` is enabled.

const BACKCHANNEL_TOKENS: &[&str] = &[
    // affirmation grunts
    "yeah", "yea", "yep", "yup", "yes",
    // negation grunts
    "no", "nope", "nah",
    // openers / fillers
    "oh", "ah", "ha", "huh", "hey",
    // hesitation fillers
    "um", "umm", "ummm", "uhm", "uhmm", "uhh", "uhhh", "ahh", "er", "erm",
    "well", "so", "like", "anyway", "anyways",
    // hum patterns (hyphens preserved by normalize)
    "mm", "mmm", "mmmm", "mmmmm",
    "mhm", "mhmm",
    "mm-hmm", "mmhmm",
    "uh-huh", "uhhuh", "uh",
    "hm", "hmm", "hmmm",
    // short acks
    "ok", "okay", "right", "sure", "gotcha", "alright", "cool", "exactly", "totally",
    // "you"/"thank"/"thanks"/"bye" are not listed; they occur as real
    // one-word utterances ("Thank you", "Bye").
];

/// Words stripped from the *front* of a longer segment as pure filler.
/// Excludes "no"/"nah"/"nope".
const LEADING_FILLER: &[&str] = &[
    "um", "umm", "ummm", "uhm", "uhmm", "uh", "uhh", "uhhh",
    "er", "erm", "ah", "ahh", "oh", "ohh",
    "mm", "mmm", "mmmm", "mhm", "mhmm", "hm", "hmm", "hmmm",
    "well", "so", "like", "yeah", "yea", "yep", "yup", "yes",
    "okay", "ok", "right", "anyway", "anyways",
];

/// Strip leading conversational filler ("Um, ", "Yeah, so ") from a segment,
/// keeping the substantive remainder; re-capitalizes the new first letter.
/// Returns "" if nothing substantive is left. Non-filler-leading text is
/// returned trimmed but otherwise unchanged.
pub fn strip_leading_filler(text: &str) -> String {
    let mut rest = text.trim_start();
    let mut removed = 0usize;
    while removed < 4 {
        let word_end = rest
            .find(|c: char| !c.is_ascii_alphabetic())
            .unwrap_or(rest.len());
        if word_end == 0 {
            break; // starts with punctuation/digit — not a filler word
        }
        let word = rest[..word_end].to_ascii_lowercase();
        if !LEADING_FILLER.contains(&word.as_str()) {
            break;
        }
        // Skip the word plus any trailing punctuation/space before the next word.
        let after = &rest[word_end..];
        let next_alnum = after
            .find(|c: char| c.is_ascii_alphanumeric())
            .unwrap_or(after.len());
        rest = after[next_alnum..].trim_start();
        removed += 1;
        if rest.is_empty() {
            return String::new();
        }
    }
    if removed == 0 {
        return text.trim().to_string();
    }
    let mut chars = rest.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// Fillers stripped from the *middle* of a sentence: hesitation grunts only.
/// Excludes "well", "so", "like", "yeah".
const MID_FILLER: &[&str] = &[
    "um", "umm", "ummm", "uhm", "uhmm",
    "uh", "uhh", "uhhh",
    "er", "erm",
    "ah", "ahh",
];

/// Strip mid-sentence hesitation fillers ("um", "uh", "er", …) bounded by
/// non-letter chars; "Mumbai" / "umbrella" are untouched. Eats one trailing
/// comma if present and collapses resulting double-spaces.
pub fn strip_mid_filler(text: &str) -> String {
    // Walks the bytes with codepoint awareness: multi-byte UTF-8 sequences
    // (em-dashes, smart quotes, accented letters) are copied intact.
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    let mut at_word_boundary = true;
    while i < bytes.len() {
        let b = bytes[i];
        let is_letter = b.is_ascii_alphabetic();
        if is_letter && at_word_boundary {
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_alphabetic() {
                i += 1;
            }
            let word = &text[start..i];
            let next = bytes.get(i).copied();
            let after_is_boundary = match next {
                None => true,
                Some(c) => !c.is_ascii_alphabetic(),
            };
            let lower = word.to_ascii_lowercase();
            if after_is_boundary && MID_FILLER.contains(&lower.as_str()) {
                if matches!(next, Some(b',')) {
                    i += 1;
                }
                if matches!(bytes.get(i).copied(), Some(b' ')) {
                    i += 1;
                }
                while out.ends_with(' ') && out.len() > 1 {
                    out.pop();
                }
                if !out.is_empty() && !out.ends_with(' ') {
                    out.push(' ');
                }
                at_word_boundary = true;
                continue;
            }
            out.push_str(word);
            at_word_boundary = false;
            continue;
        }
        // Non-letter byte. If it's the start of a multi-byte UTF-8 sequence,
        // copy the entire codepoint, not just the leading byte.
        let ch_len = utf8_char_len(b);
        let end = (i + ch_len).min(bytes.len());
        out.push_str(&text[i..end]);
        at_word_boundary = !is_letter;
        i = end;
    }
    let mut collapsed = String::with_capacity(out.len());
    let mut last_space = true;
    for c in out.chars() {
        if c == ' ' {
            if !last_space {
                collapsed.push(' ');
            }
            last_space = true;
        } else {
            collapsed.push(c);
            last_space = false;
        }
    }
    let fixed = collapsed
        .replace(" ,", ",")
        .replace(" .", ".")
        .replace(" ?", "?")
        .replace(" !", "!")
        .replace(" ;", ";")
        .replace(" :", ":");
    let trimmed = fixed.trim().to_string();
    // Re-capitalize: if the strip left a lowercase first letter, fix it.
    let mut chars = trimmed.chars();
    match chars.next() {
        Some(c) if c.is_lowercase() => c.to_uppercase().collect::<String>() + chars.as_str(),
        _ => trimmed,
    }
}

/// Width in bytes of a UTF-8 codepoint whose leading byte is `b`. Returns 1
/// for ASCII and for stray continuation bytes.
fn utf8_char_len(b: u8) -> usize {
    match b {
        0x00..=0x7F => 1,
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF7 => 4,
        _ => 1,
    }
}

/// Max total tokens for a segment to qualify as backchannel.
const MAX_WORDS: usize = 6;

/// Returns true if `text` is a pure backchannel utterance.
pub fn is_backchannel(text: &str) -> bool {
    let norm = normalize(text);
    if norm.is_empty() {
        return true;
    }
    let words: Vec<&str> = norm.split_whitespace().collect();
    if words.is_empty() || words.len() > MAX_WORDS {
        return false;
    }
    words.iter().all(|w| BACKCHANNEL_TOKENS.contains(w))
}

fn normalize(text: &str) -> String {
    let lowered: String = text
        .chars()
        .map(|c| {
            let c = c.to_ascii_lowercase();
            if c.is_ascii_alphanumeric() || c == '-' || c == ' ' {
                c
            } else {
                ' '
            }
        })
        .collect();
    lowered
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drops_simple_grunts() {
        for s in [
            "Yeah.", "yeah", "Yep.", "Mm-hmm.", "mm-hmm", "Mm.", "Mmm",
            "Oh.", "OK.", "Okay.", "Right.", "Sure.", "Hmm.", "Uh-huh.",
        ] {
            assert!(is_backchannel(s), "should drop: {s:?}");
        }
    }

    #[test]
    fn keeps_short_real_utterances() {
        // Real one-word turns; kept.
        for s in ["Thank you.", "thanks", "Bye."] {
            assert!(!is_backchannel(s), "should keep: {s:?}");
        }
    }

    #[test]
    fn drops_repeated_grunts() {
        for s in [
            "Yeah yeah", "Yeah yeah yeah", "Yep Yep Yep", "Mm Mm", "Mm-hmm Mm-hmm",
            "Yeah yeah sure", "Oh yeah",
        ] {
            assert!(is_backchannel(s), "should drop: {s:?}");
        }
    }

    #[test]
    fn keeps_substantive() {
        for s in [
            "I keep getting stuff in my throat.",
            "let's look back here yeah yeah yeah",
            "Yeah, that sounds good to me",
            "weón Justin no stuff",
        ] {
            assert!(!is_backchannel(s), "should keep: {s:?}");
        }
    }

    #[test]
    fn empty_text_is_backchannel() {
        assert!(is_backchannel(""));
        assert!(is_backchannel("   "));
        assert!(is_backchannel("..."));
    }

    #[test]
    fn drops_um_family() {
        for s in ["Um.", "um", "Umm.", "Uhm.", "Er.", "Erm.", "Well.", "So.", "Cool.", "Exactly."] {
            assert!(is_backchannel(s), "should drop: {s:?}");
        }
    }

    #[test]
    fn strips_mid_filler_keeps_real_words() {
        assert_eq!(strip_mid_filler("I, um, think we should ship."), "I, think we should ship.");
        assert_eq!(strip_mid_filler("Then, uh, we go to step two."), "Then, we go to step two.");
        assert_eq!(strip_mid_filler("Um, I wanted to start off."), "I wanted to start off.");
        // does NOT mangle real words containing the filler letters
        assert_eq!(strip_mid_filler("I went to Mumbai with an umbrella."), "I went to Mumbai with an umbrella.");
        // multiple fillers
        assert_eq!(strip_mid_filler("Um, uh, the thing is, er, complicated."), "The thing is, complicated.");
        // capitalized mid-sentence
        assert_eq!(strip_mid_filler("So Uh I think yes."), "So I think yes.");
    }

    #[test]
    fn strips_leading_filler_keeps_content() {
        assert_eq!(strip_leading_filler("Um, the thing is we should ship."), "The thing is we should ship.");
        assert_eq!(strip_leading_filler("Yeah, so, like, the plan."), "The plan.");
        assert_eq!(strip_leading_filler("Well I think we should wait."), "I think we should wait.");
        // pure filler -> empty (caller drops)
        assert_eq!(strip_leading_filler("Um, yeah."), "");
        assert_eq!(strip_leading_filler("So."), "");
        // non-filler-leading: unchanged (just trimmed)
        assert_eq!(strip_leading_filler("  The deadline is Friday.  "), "The deadline is Friday.");
        // never strip negations
        assert_eq!(strip_leading_filler("No, I disagree with that."), "No, I disagree with that.");
        // don't over-strip: stop at first non-filler
        assert_eq!(strip_leading_filler("Right, okay, let's go."), "Let's go.");
    }
}
