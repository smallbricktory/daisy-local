//! Text normalization + bigram-Jaccard similarity for cross-track dedup.

use std::collections::HashSet;

/// Normalize a transcript snippet for similarity comparison:
/// - Lowercase
/// - Strip Whisper noise tokens like `[BLANK_AUDIO]`, `♪`, `(applause)`
/// - Replace non-alphanumeric chars with spaces
/// - Collapse whitespace
pub fn normalize(text: &str) -> String {
    let lowered = text.to_lowercase();
    let stripped = strip_noise_tokens(&lowered);
    // Replace any char that isn't ASCII alphanumeric or whitespace with a space.
    let cleaned: String = stripped
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c.is_whitespace() { c } else { ' ' })
        .collect();
    cleaned.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn strip_noise_tokens(s: &str) -> String {
    const TOKENS: &[&str] = &[
        "[blank_audio]",
        "[silence]",
        "[music]",
        "(applause)",
        "(laughter)",
        "♪",
        "♫",
    ];
    let mut out = s.to_string();
    for t in TOKENS {
        out = out.replace(t, " ");
    }
    out
}

/// Word-bigram Jaccard similarity in [0.0, 1.0]. Both strings are normalized
/// internally; callers can pass raw text. Single-word strings fall back to a
/// unigram match (1.0 if equal, 0.0 otherwise).
pub fn bigram_jaccard(a: &str, b: &str) -> f32 {
    let na = normalize(a);
    let nb = normalize(b);
    if na.is_empty() || nb.is_empty() {
        return 0.0;
    }
    let words_a: Vec<&str> = na.split_whitespace().collect();
    let words_b: Vec<&str> = nb.split_whitespace().collect();
    if words_a.len() < 2 || words_b.len() < 2 {
        // Unigram fallback for trivially short inputs.
        let set_a: HashSet<&str> = words_a.iter().copied().collect();
        let set_b: HashSet<&str> = words_b.iter().copied().collect();
        return jaccard_set(&set_a, &set_b);
    }
    let bigrams_a: HashSet<(String, String)> = words_a
        .windows(2)
        .map(|w| (w[0].to_string(), w[1].to_string()))
        .collect();
    let bigrams_b: HashSet<(String, String)> = words_b
        .windows(2)
        .map(|w| (w[0].to_string(), w[1].to_string()))
        .collect();
    jaccard_set(&bigrams_a, &bigrams_b)
}

fn jaccard_set<T: std::hash::Hash + Eq>(a: &HashSet<T>, b: &HashSet<T>) -> f32 {
    if a.is_empty() && b.is_empty() {
        return 0.0;
    }
    let inter = a.intersection(b).count() as f32;
    let union = a.union(b).count() as f32;
    if union == 0.0 {
        0.0
    } else {
        inter / union
    }
}

/// Profanity roots (base words only); inflections are handled by SUFFIXES.
const PROFANITY: &[&str] = &[
    // 4-letter
    "fuck", "shit", "damn", "crap", "piss", "cunt", "dick", "cock", "twat", "arse", "turd",
    // 5-letter bases (plurals are covered by SUFFIXES)
    "bitch", "prick", "shite", "whore",
];

/// Inflection suffixes a root may carry and still be masked (empty = the bare
/// root). Covers fuck→f***, fucks→f***s, fucked→f***ed, fucking→f***ing,
/// fuckin→f***in, fucker(s)→f***er(s), shitty→s***ty. A word whose remainder
/// after the root is not a listed suffix (e.g. "cockpit" → "pit") is left
/// unmasked.
const SUFFIXES: &[&str] = &["", "s", "es", "ed", "ing", "in", "er", "ers", "y", "ty"];

/// Mask whole-word profanity, keeping the first character (original case) and
/// replacing the rest with `*` — e.g. `fuck` → `f***`, `Shit` → `S***`.
/// Matching is whole-word + ASCII-case-insensitive: words are runs of
/// alphabetic chars; substrings never match (`cockpit` is one run and is left
/// alone). Everything else passes through unchanged. Applied to both live and
/// finalized transcript text.
pub fn mask_profanity(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut word = String::new();
    for c in text.chars() {
        if c.is_alphabetic() {
            word.push(c);
        } else {
            mask_word_into(&mut word, &mut out);
            out.push(c);
        }
    }
    mask_word_into(&mut word, &mut out);
    out
}

/// Flush `word` into `out`, masking it if it's a profanity root + a known
/// inflection suffix. The root's chars become first-char + `*`s; the suffix is
/// kept verbatim (fucking → f***ing). Clears `word`.
fn mask_word_into(word: &mut String, out: &mut String) {
    if word.is_empty() {
        return;
    }
    let lower = word.to_ascii_lowercase();
    let masked = PROFANITY.iter().find_map(|root| {
        let rest = lower.strip_prefix(root)?;
        if !SUFFIXES.contains(&rest) {
            return None;
        }
        // Mask the root (keep its first char, original case); append the
        // suffix verbatim from the original word.
        let mut m = String::with_capacity(word.len());
        let mut src = word.chars();
        m.push(src.next()?); // first char, original case
        for _ in 1..root.chars().count() {
            m.push('*');
        }
        m.extend(word.chars().skip(root.chars().count())); // suffix, original case
        Some(m)
    });
    match masked {
        Some(m) => out.push_str(&m),
        None => out.push_str(word),
    }
    word.clear();
}

#[cfg(test)]
mod profanity_tests {
    use super::mask_profanity;

    #[test]
    fn masks_keeping_first_char_and_length() {
        assert_eq!(mask_profanity("fuck"), "f***");
        assert_eq!(mask_profanity("bitch"), "b****");
    }

    #[test]
    fn case_insensitive_preserves_first_case() {
        assert_eq!(mask_profanity("Shit"), "S***");
        assert_eq!(mask_profanity("FUCK"), "F***");
    }

    #[test]
    fn whole_word_only_no_scunthorpe() {
        // Remainders that aren't inflection suffixes are left alone.
        assert_eq!(mask_profanity("cockpit assassin classic"), "cockpit assassin classic");
        assert_eq!(mask_profanity("Scunthorpe dickens damnation"), "Scunthorpe dickens damnation");
    }

    #[test]
    fn masks_inflections_keeping_suffix() {
        assert_eq!(mask_profanity("fucking"), "f***ing");
        assert_eq!(mask_profanity("fucked"), "f***ed");
        assert_eq!(mask_profanity("shits"), "s***s");
        assert_eq!(mask_profanity("pissed off"), "p***ed off");
        assert_eq!(mask_profanity("Fucking hell"), "F***ing hell");
    }

    #[test]
    fn masks_in_context_keeps_punctuation_and_clean_words() {
        assert_eq!(mask_profanity("oh shit, that's crap!"), "oh s***, that's c***!");
        assert_eq!(mask_profanity("no swearing here"), "no swearing here");
    }
}
