//! Sentence-case continuity across live ASR segments.
//!
//! Each decode window is transcribed cold, so the model capitalizes its first
//! word whether or not it starts a sentence. A sentence spanning two windows
//! comes back as "we looked at the numbers" + "And they were fine".
//!
//! A fragment that continues an unfinished sentence has its first word
//! lowercased, unless that word is one the speaker uses capitalized
//! mid-sentence — a name or an acronym. Those are learned from the fragments
//! themselves: any capitalized word that is NOT sentence-initial is a proper
//! noun by position, and is left alone wherever it appears afterwards.

use std::collections::HashSet;

/// Sentence-ending punctuation, ignoring any closing quote or bracket after it.
fn ends_sentence(text: &str) -> bool {
    let trimmed = text.trim_end_matches(['"', '\'', ')', ']', '}', '»', '”', '’']);
    matches!(
        trimmed.chars().last(),
        Some('.') | Some('!') | Some('?') | Some('…') | Some(':')
    )
}

/// A single leading capital over otherwise-lowercase letters: `Numbers`, but
/// not `API` (acronym), `McDonald` (internal capital) or `iPhone`.
fn is_plain_capitalized(word: &str) -> bool {
    let mut chars = word.chars();
    let Some(first) = chars.next() else { return false };
    if !first.is_uppercase() {
        return false;
    }
    chars
        .filter(|c| c.is_alphabetic())
        .all(|c| c.is_lowercase())
}

/// `I` and its contractions stay capitalized anywhere.
fn is_pronoun_i(word: &str) -> bool {
    let core = word.trim_end_matches(|c: char| !c.is_alphanumeric() && c != '\'');
    core == "I" || core.starts_with("I'")
}

fn first_word(text: &str) -> Option<&str> {
    text.split_whitespace().next()
}

/// Strips the punctuation a word carries so `numbers,` and `numbers` are one
/// entry in the learned set.
fn word_key(word: &str) -> String {
    word.trim_matches(|c: char| !c.is_alphanumeric() && c != '\'')
        .to_string()
}

/// Per-track state: what the track has already committed, and the proper nouns
/// seen in it so far.
#[derive(Debug, Default)]
pub struct CaseSmoother {
    proper_nouns: HashSet<String>,
    /// False until the first fragment lands: the opening fragment of a track
    /// is a sentence start and keeps its capital.
    committed_any: bool,
    prev_ends_sentence: bool,
}

impl CaseSmoother {
    pub fn new() -> Self {
        Self::default()
    }

    /// Seeds the proper nouns from the meeting's priming terms — the title,
    /// attendee names and tag vocabulary already handed to the decoder. Terms
    /// are phrases ("Priya Okonkwo", "Borealis Freight"), so each word counts
    /// on its own: a fragment opens with one word, not the phrase.
    pub fn with_known_names(terms: &[String]) -> Self {
        let mut s = Self::default();
        for term in terms {
            for word in term.split_whitespace() {
                if word.chars().next().is_some_and(|c| c.is_uppercase()) {
                    s.proper_nouns.insert(word_key(word));
                }
            }
        }
        s
    }

    /// Returns `text` recased as a continuation of what this track already
    /// committed, and learns from it for the fragments that follow.
    pub fn smooth(&mut self, text: &str) -> String {
        self.learn_proper_nouns(text);

        let continues = self.committed_any && !self.prev_ends_sentence;
        let out = match first_word(text) {
            Some(word)
                if continues
                    && is_plain_capitalized(word)
                    && !is_pronoun_i(word)
                    && !self.proper_nouns.contains(&word_key(word)) =>
            {
                lowercase_first(text)
            }
            _ => text.to_string(),
        };

        if !out.trim().is_empty() {
            self.committed_any = true;
            self.prev_ends_sentence = ends_sentence(out.trim_end());
        }
        out
    }

    /// Capitalized words that are not sentence-initial are names or acronyms.
    fn learn_proper_nouns(&mut self, text: &str) {
        let words: Vec<&str> = text.split_whitespace().collect();
        for (i, word) in words.iter().enumerate() {
            // Skip the fragment's own first word: whether it is sentence-initial
            // is exactly what is unknown here.
            if i == 0 {
                continue;
            }
            if ends_sentence(words[i - 1]) {
                continue;
            }
            let first = word.chars().next();
            if first.is_some_and(|c| c.is_uppercase()) && !is_pronoun_i(word) {
                self.proper_nouns.insert(word_key(word));
            }
        }
    }
}

fn lowercase_first(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_lowercase().chain(chars).collect(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(fragments: &[&str]) -> Vec<String> {
        let mut s = CaseSmoother::new();
        fragments.iter().map(|f| s.smooth(f)).collect()
    }

    #[test]
    fn continuation_of_an_open_sentence_is_lowercased() {
        assert_eq!(
            run(&["we looked at the numbers", "And they were fine"]),
            ["we looked at the numbers", "and they were fine"]
        );
    }

    #[test]
    fn first_fragment_keeps_its_capital() {
        assert_eq!(run(&["Hello there"]), ["Hello there"]);
    }

    #[test]
    fn a_real_sentence_start_keeps_its_capital() {
        assert_eq!(
            run(&["That is done.", "Next we ship it"]),
            ["That is done.", "Next we ship it"]
        );
    }

    #[test]
    fn closing_quote_after_the_period_still_ends_the_sentence() {
        assert_eq!(
            run(&["she said \"go.\"", "Then we left"]),
            ["she said \"go.\"", "Then we left"]
        );
    }

    #[test]
    fn pronoun_i_and_its_contractions_survive() {
        assert_eq!(
            run(&["and then", "I said no", "I'm sure"]),
            ["and then", "I said no", "I'm sure"]
        );
    }

    #[test]
    fn acronyms_and_internal_capitals_survive() {
        assert_eq!(
            run(&["we called the", "API for it"]),
            ["we called the", "API for it"]
        );
        assert_eq!(
            run(&["we ate at", "McDonald's today"]),
            ["we ate at", "McDonald's today"]
        );
    }

    #[test]
    fn a_name_seen_mid_sentence_is_left_alone_when_it_leads_later() {
        // "Priya" is learned from a mid-sentence use, so the later fragment
        // that opens with it keeps its capital.
        assert_eq!(
            run(&["I spoke to Priya about it", "Priya agreed"]),
            ["I spoke to Priya about it", "Priya agreed"]
        );
    }

    #[test]
    fn an_unseen_name_leading_a_continuation_is_lowercased() {
        // The residual cost when the name is in neither the priming terms nor
        // an earlier mid-sentence position.
        assert_eq!(
            run(&["we heard from", "Priya this morning"]),
            ["we heard from", "priya this morning"]
        );
    }

    #[test]
    fn a_primed_name_survives_without_ever_appearing_mid_sentence() {
        let terms = vec!["Priya Okonkwo".to_string(), "Borealis Freight".to_string()];
        let mut s = CaseSmoother::with_known_names(&terms);
        assert_eq!(s.smooth("we heard from"), "we heard from");
        assert_eq!(s.smooth("Priya this morning"), "Priya this morning");
        assert_eq!(s.smooth("about"), "about");
        assert_eq!(s.smooth("Borealis and the rest"), "Borealis and the rest");
    }

    #[test]
    fn priming_terms_do_not_protect_ordinary_words() {
        // A lowercase term contributes nothing: only capitalized words in the
        // priming list are names.
        let terms = vec!["quarterly review".to_string()];
        let mut s = CaseSmoother::with_known_names(&terms);
        assert_eq!(s.smooth("we ran the"), "we ran the");
        assert_eq!(s.smooth("Quarterly numbers"), "quarterly numbers");
    }

    #[test]
    fn empty_fragments_do_not_close_the_sentence() {
        assert_eq!(
            run(&["we looked at it", "   ", "And moved on"]),
            ["we looked at it", "   ", "and moved on"]
        );
    }
}
