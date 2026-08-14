//! Shared derivation of transcription priming from a meeting's proper nouns —
//! title, attendee names, tag names — rendered as a Whisper `initial_prompt`
//! vocabulary sentence. Tag `prompt_md` prose is not included.

use crate::state::Tag;
use std::collections::HashSet;

/// Max characters stored in a tag's vocabulary field.
pub const MAX_VOCAB_LEN: usize = 2048;

/// Normalize a tag vocabulary field for storage: trim, treat blank as None,
/// and hard-cap length to [`MAX_VOCAB_LEN`] characters.
pub fn sanitize_vocab(vocab: Option<String>) -> Option<String> {
    let v = vocab?;
    let t = v.trim();
    if t.is_empty() {
        return None;
    }
    Some(t.chars().take(MAX_VOCAB_LEN).collect())
}

/// Split a vocabulary field into discrete terms: split on newlines AND commas,
/// trim, drop empties, case-insensitive dedup (keep first casing).
pub fn parse_terms(vocab: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for raw in vocab.split(['\n', ',']) {
        let t = raw.trim();
        if t.is_empty() {
            continue;
        }
        if seen.insert(t.to_lowercase()) {
            out.push(t.to_string());
        }
    }
    out
}

/// Gather the vocabulary terms for the tags attached to a session (by id),
/// in tag order, flattened and de-duplicated across tags.
pub fn collect_tag_vocab_terms(tags: &[Tag], tag_ids: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for id in tag_ids {
        let Some(tag) = tags.iter().find(|t| &t.id == id) else {
            continue;
        };
        let Some(vocab) = tag.vocab_md.as_deref() else {
            continue;
        };
        for term in parse_terms(vocab) {
            if seen.insert(term.to_lowercase()) {
                out.push(term);
            }
        }
    }
    out
}

/// Ordered, de-duplicated proper-noun terms: title, then attendee names, then
/// tag names. Trims, drops empties, case-insensitive dedup (keeps first casing).
pub fn meeting_terms(title: Option<&str>, attendees: &[String], tags: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let chained = title
        .into_iter()
        .chain(attendees.iter().map(|s| s.as_str()))
        .chain(tags.iter().map(|s| s.as_str()));
    for s in chained {
        let t = s.trim();
        if t.is_empty() {
            continue;
        }
        if seen.insert(t.to_lowercase()) {
            out.push(t.to_string());
        }
    }
    out
}

/// A Whisper `initial_prompt` sentence from the terms, or None if empty.
pub fn vocab_sentence(terms: &[String]) -> Option<String> {
    if terms.is_empty() {
        return None;
    }
    Some(format!("Meeting context — {}.", terms.join(", ")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vocab_trimmed_to_none_when_blank() {
        assert_eq!(sanitize_vocab(Some("   \n ".into())), None);
        assert_eq!(sanitize_vocab(Some("".into())), None);
        assert_eq!(sanitize_vocab(None), None);
    }

    #[test]
    fn vocab_kept_and_capped() {
        assert_eq!(sanitize_vocab(Some("Northwind, Zephyr".into())).as_deref(), Some("Northwind, Zephyr"));
        let long = "a,".repeat(5000); // > MAX_VOCAB_LEN
        let out = sanitize_vocab(Some(long)).unwrap();
        assert!(out.chars().count() <= MAX_VOCAB_LEN);
    }

    #[test]
    fn parse_terms_splits_commas_and_newlines() {
        let t = parse_terms("Northwind, Zephyr\nPriya Okonkwo ,\n, Borealis Freight\nnorthwind");
        assert_eq!(t, vec!["Northwind", "Zephyr", "Priya Okonkwo", "Borealis Freight"]);
    }

    #[test]
    fn parse_terms_empty_is_empty() {
        assert!(parse_terms("").is_empty());
        assert!(parse_terms("  ,\n , ").is_empty());
    }

    #[test]
    fn collect_tag_vocab_terms_gathers_session_tags() {
        let tags = vec![
            Tag {
                id: "a".into(),
                name: "Sales".into(),
                color_hex: "#fff".into(),
                prompt_md: None,
                vocab_md: Some("Zephyr, Aurora".into()),
                created_at_unix_seconds: 0,
                use_count: 0,
            },
            Tag {
                id: "b".into(),
                name: "Eng".into(),
                color_hex: "#fff".into(),
                prompt_md: Some("be terse".into()),
                vocab_md: None,
                created_at_unix_seconds: 0,
                use_count: 0,
            },
        ];
        let terms = collect_tag_vocab_terms(&tags, &["a".to_string(), "b".to_string()]);
        assert_eq!(terms, vec!["Zephyr", "Aurora"]);
    }

    #[test]
    fn terms_order_dedup_trim() {
        let terms = meeting_terms(
            Some("  Q3 Planning  "),
            &["Alice".into(), "  ".into(), "alice".into(), "Bob".into()],
            &["Sales".into(), "SALES".into()],
        );
        // title, attendees (deduped case-insensitively), tags (deduped); blanks dropped.
        assert_eq!(terms, vec!["Q3 Planning", "Alice", "Bob", "Sales"]);
    }

    #[test]
    fn empty_inputs_give_nothing() {
        assert!(meeting_terms(None, &[], &[]).is_empty());
        assert!(meeting_terms(Some("   "), &["".into()], &[]).is_empty());
        assert_eq!(vocab_sentence(&[]), None);
    }

    #[test]
    fn sentence_joins_terms() {
        let s = vocab_sentence(&["Acme".into(), "Siobhán".into()]).unwrap();
        assert_eq!(s, "Meeting context — Acme, Siobhán.");
    }

    #[test]
    fn boundary_no_tags_no_attendees_only_title() {
        // A title alone still yields a prompt.
        let terms = meeting_terms(Some("Recording 2026-06-10"), &[], &[]);
        assert_eq!(terms, vec!["Recording 2026-06-10"]);
        assert!(vocab_sentence(&terms).is_some());
    }

    #[test]
    fn boundary_all_blank_yields_no_priming() {
        // No title, blank attendees, blank tag names → no terms, no sentence.
        let terms = meeting_terms(None, &["".into(), "   ".into()], &["".into()]);
        assert!(terms.is_empty());
        assert_eq!(vocab_sentence(&terms), None);
    }

    #[test]
    fn boundary_blank_tags_dropped_real_kept() {
        // Blank tag names are dropped; real ones are kept.
        let terms = meeting_terms(None, &[], &["".into(), "Sales".into(), "  ".into()]);
        assert_eq!(terms, vec!["Sales"]);
    }
}
