// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Folds the per-window summaries of a long meeting into one, with no model
//! call. Every sentence in the result was written by a window that had its
//! part of the transcript in view; this stage only drops repeats.
//!
//! Text comparison works in any script: letters and digits over NFC, character
//! bigrams where the script writes no spaces, and sentence ends at `.!?` and
//! `。！？`. Port of the iOS `NoteFold`; behaviour must stay in step with it.
use crate::model::{ActionItem, Section, SectionedSummary, SummaryStructured};
use std::collections::BTreeSet;
use unicode_normalization::UnicodeNormalization;

/// Two facts are one fact when either's words contain the other's, or when
/// they share this much of their vocabulary.
const JACCARD_THRESHOLD: f64 = 0.6;

const FILLER_EXACT: &[&str] = &["none", "n a", "na", "nothing", "no", "unknown", "not applicable"];
const FILLER_LEAD_INS: &[&str] = &["there were ", "there are ", "there was ", "there is "];
const FILLER_QUALIFIERS: &[&str] = &["specific ", "particular ", "explicit ", "new "];
const FILLER_NOUNS: &[&str] = &[
    "decisions", "action items", "actions", "commitments", "items", "tasks", "next steps",
];

/// Scripts written without spaces between words; text in these compares on
/// character bigrams.
fn is_unsegmented(c: char) -> bool {
    matches!(
        c as u32,
        0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF | 0x20000..=0x2A6DF
        | 0x3040..=0x309F | 0x30A0..=0x30FF | 0x31F0..=0x31FF
        | 0x0E00..=0x0E7F | 0x0E80..=0x0EFF | 0x1000..=0x109F | 0x1780..=0x17FF
    )
}

/// Lowercased, every mark that is not a letter or a digit turned to a space,
/// runs of space collapsed. Comparison only.
pub fn normalized(s: &str) -> String {
    let mut out = String::new();
    let mut last_was_space = true;
    for c in s.nfc().flat_map(char::to_lowercase) {
        if c.is_alphanumeric() {
            out.push(c);
            last_was_space = false;
        } else if !last_was_space {
            out.push(' ');
            last_was_space = true;
        }
    }
    out.trim_end().to_string()
}

/// The comparison tokens: whitespace-delimited words, or character bigrams
/// for tokens carrying an unsegmented script.
pub fn words(s: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for token in normalized(s).split(' ').filter(|t| !t.is_empty()) {
        if token.chars().any(is_unsegmented) {
            let cs: Vec<char> = token.chars().collect();
            let n = cs.len().saturating_sub(1).max(1);
            for i in 0..n {
                let mut gram = String::new();
                gram.push(cs[i]);
                if i + 1 < cs.len() {
                    gram.push(cs[i + 1]);
                }
                out.insert(gram);
            }
        } else {
            out.insert(token.to_string());
        }
    }
    out
}

pub fn same_fact(a: &str, b: &str) -> bool {
    let x = words(a);
    let y = words(b);
    if x.is_empty() || y.is_empty() {
        return false;
    }
    if x.is_subset(&y) || y.is_subset(&x) {
        return true;
    }
    let inter = x.intersection(&y).count() as f64;
    let union = x.union(&y).count() as f64;
    inter / union >= JACCARD_THRESHOLD
}

/// A field the model filled to satisfy the schema rather than because the
/// meeting held anything.
pub fn is_filler(s: &str) -> bool {
    let n = normalized(s);
    if n.is_empty() || FILLER_EXACT.contains(&n.as_str()) {
        return true;
    }
    let mut rest = n.as_str();
    if let Some(lead) = FILLER_LEAD_INS.iter().find(|l| rest.starts_with(**l)) {
        rest = &rest[lead.len()..];
    }
    let Some(r) = rest.strip_prefix("no ") else { return false };
    rest = r;
    if let Some(q) = FILLER_QUALIFIERS.iter().find(|q| rest.starts_with(**q)) {
        rest = &rest[q.len()..];
    }
    let names_noun = FILLER_NOUNS.iter().any(|noun| {
        let singular = &noun[..noun.len() - 1];
        [*noun, singular]
            .iter()
            .any(|form| rest == *form || rest.starts_with(&format!("{form} ")))
    });
    if !names_noun {
        return false;
    }
    // A clause hung off the negation carries a claim; the comma marks it.
    !s.contains(',')
}

/// Sentence boundaries: `.!?` followed by whitespace, and the CJK terminators
/// on their own.
pub fn sentences(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut pending_break = false;
    for c in s.trim().chars() {
        if pending_break && c.is_whitespace() {
            push_trimmed(&mut out, &mut current);
            pending_break = false;
            continue;
        }
        pending_break = matches!(c, '.' | '!' | '?');
        current.push(c);
        if matches!(c, '。' | '！' | '？') {
            push_trimmed(&mut out, &mut current);
            pending_break = false;
        }
    }
    push_trimmed(&mut out, &mut current);
    out
}

fn push_trimmed(out: &mut Vec<String>, current: &mut String) {
    let piece = current.trim();
    if !piece.is_empty() {
        out.push(piece.to_string());
    }
    current.clear();
}

/// Keeps the first appearance of each fact, and the wording that carries more
/// of it. Filler is dropped.
pub fn deduped<'a>(items: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut kept: Vec<String> = Vec::new();
    for raw in items {
        let item = raw.trim();
        if item.is_empty() || is_filler(item) {
            continue;
        }
        match kept.iter().position(|k| same_fact(item, k)) {
            Some(i) => {
                if words(item).len() > words(&kept[i]).len() {
                    kept[i] = item.to_string();
                }
            }
            None => kept.push(item.to_string()),
        }
    }
    kept
}

/// Action items across windows, one entry per task. Same task: the longer
/// wording wins, an unnamed owner or due date yields to a named one, and two
/// different named owners (or due dates) stay two items.
pub fn merge_action_items<'a>(items: impl IntoIterator<Item = &'a ActionItem>) -> Vec<ActionItem> {
    let mut out: Vec<ActionItem> = Vec::new();
    for item in items {
        let text = item.task.trim();
        if text.is_empty() || is_filler(text) {
            continue;
        }
        let owner = item.owner.as_deref().map(str::trim).filter(|s| !s.is_empty());
        let due = item.due.as_deref().map(str::trim).filter(|s| !s.is_empty());
        let compatible = |a: Option<&str>, b: Option<&str>| match (a, b) {
            (Some(x), Some(y)) => normalized(x) == normalized(y),
            _ => true,
        };
        let hit = out.iter().position(|k| {
            same_fact(text, &k.task)
                && compatible(k.owner.as_deref(), owner)
                && compatible(k.due.as_deref(), due)
        });
        match hit {
            Some(i) => {
                if words(text).len() > words(&out[i].task).len() {
                    out[i].task = text.to_string();
                }
                if out[i].owner.is_none() {
                    out[i].owner = owner.map(str::to_string);
                }
                if out[i].due.is_none() {
                    out[i].due = due.map(str::to_string);
                }
            }
            None => out.push(ActionItem {
                task: text.to_string(),
                owner: owner.map(str::to_string),
                due: due.map(str::to_string),
            }),
        }
    }
    out
}

/// Deduped sentences of every window's prose, in window order, as one text.
fn fold_prose<'a>(parts: impl IntoIterator<Item = &'a str>) -> String {
    let all: Vec<String> = parts.into_iter().flat_map(sentences).collect();
    deduped(all.iter().map(String::as_str)).join(" ")
}

pub fn merge_classic(parts: &[SummaryStructured]) -> SummaryStructured {
    SummaryStructured {
        tldr: fold_prose(parts.iter().map(|p| p.tldr.as_str())),
        action_items: merge_action_items(parts.iter().flat_map(|p| p.action_items.iter())),
        decisions: deduped(parts.iter().flat_map(|p| p.decisions.iter().map(String::as_str))),
        open_questions: deduped(
            parts.iter().flat_map(|p| p.open_questions.iter().map(String::as_str)),
        ),
        key_topics: deduped(parts.iter().flat_map(|p| p.key_topics.iter().map(String::as_str))),
    }
}

/// Sections merge on an identical normalized heading, in first-seen order,
/// keeping the first window's bullets flag; their content is deduped.
pub fn merge_sectioned(parts: &[SectionedSummary]) -> SectionedSummary {
    let mut sections: Vec<Section> = Vec::new();
    for s in parts.iter().flat_map(|p| p.sections.iter()) {
        let key = normalized(&s.heading);
        match sections.iter_mut().find(|k| normalized(&k.heading) == key) {
            Some(k) => k.content.extend(s.content.iter().cloned()),
            None => sections.push(s.clone()),
        }
    }
    for s in &mut sections {
        s.content = deduped(s.content.iter().map(String::as_str));
    }
    SectionedSummary {
        exec_summary: fold_prose(parts.iter().map(|p| p.exec_summary.as_str())),
        sections,
        action_items: merge_action_items(parts.iter().flat_map(|p| p.action_items.iter())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ai(text: &str, owner: Option<&str>, due: Option<&str>) -> ActionItem {
        ActionItem {
            task: text.into(),
            owner: owner.map(Into::into),
            due: due.map(Into::into),
        }
    }

    #[test]
    fn normalized_keeps_letters_and_digits_of_every_script() {
        assert_eq!(normalized("Hello, World! 42"), "hello world 42");
        assert_eq!(normalized("  Ünïcode—café  "), "ünïcode café");
        assert_eq!(normalized("決定：出荷は金曜日。"), "決定 出荷は金曜日");
        assert_eq!(normalized("한국어 회의, 결정!"), "한국어 회의 결정");
        // Decomposed é (e + combining acute) equals the composed form after NFC.
        assert_eq!(normalized("cafe\u{301}"), normalized("caf\u{e9}"));
    }

    #[test]
    fn words_are_tokens_for_spaced_scripts_and_bigrams_for_unsegmented() {
        let w = words("Ship on Friday");
        assert!(w.contains("ship") && w.contains("friday"));
        let j = words("金曜日に出荷");
        assert!(j.contains("金曜") && j.contains("曜日") && j.contains("出荷"));
        assert!(!j.contains("金曜日に出荷"));
        assert_eq!(words("日").len(), 1);
    }

    #[test]
    fn same_fact_by_containment_or_jaccard() {
        assert!(same_fact("Ship it Friday", "We will ship it Friday"));
        assert!(same_fact("Ship the release on Friday morning", "Ship the release on Friday"));
        assert!(!same_fact("Schedule a conversation with Sonia", "Schedule a conversation with the team"));
        assert!(!same_fact("", "anything"));
        // Japanese: the same fact in two windows compares on bigrams.
        assert!(same_fact("金曜日に出荷する", "金曜日に出荷"));
        assert!(!same_fact("予算を承認した", "金曜日に出荷"));
    }

    #[test]
    fn filler_is_recognized_and_a_clause_saves_it() {
        assert!(is_filler("None"));
        assert!(is_filler("No decisions were made in this part of the meeting."));
        assert!(is_filler("There were no specific action items."));
        assert!(is_filler("No decision reported"));
        assert!(!is_filler("No action items were assigned to Ana, who will follow up Monday"));
        assert!(!is_filler("No budget for Q3 was agreed"));
        assert!(!is_filler("Ship it Friday"));
        assert!(is_filler("…"));
    }

    #[test]
    fn sentences_split_on_latin_and_cjk_terminators() {
        assert_eq!(
            sentences("First one. Second one! Third? Last"),
            vec!["First one.", "Second one!", "Third?", "Last"]
        );
        assert_eq!(sentences("出荷は金曜日。予算は承認された！質問？"), vec!["出荷は金曜日。", "予算は承認された！", "質問？"]);
        assert_eq!(sentences("Version 2.0 shipped. Done."), vec!["Version 2.0 shipped.", "Done."]);
    }

    #[test]
    fn deduped_keeps_first_and_prefers_longer_wording() {
        let out = deduped(["Ship Friday", "None", "We ship on Friday", "Budget approved"]);
        assert_eq!(out, vec!["We ship on Friday", "Budget approved"]);
    }

    #[test]
    fn action_items_merge_with_named_owner_winning_and_conflicts_kept() {
        let items = [
            ai("Send the deck", None, None),
            ai("Send the deck to the client", Some("Ana"), None),
            ai("Send the deck", Some("Bo"), None),
            ai("Book the room", Some("Ana"), Some("Monday")),
            ai("Book the room", Some("Ana"), None),
            ai("No action items", None, None),
        ];
        let out = merge_action_items(items.iter());
        assert_eq!(out.len(), 3, "{out:?}");
        assert_eq!(out[0].task, "Send the deck to the client");
        assert_eq!(out[0].owner.as_deref(), Some("Ana"));
        assert_eq!(out[1].owner.as_deref(), Some("Bo"));
        assert_eq!(out[2].due.as_deref(), Some("Monday"));
    }

    #[test]
    fn classic_fold_dedups_across_windows() {
        let a = SummaryStructured {
            tldr: "We agreed to ship Friday. Budget is approved.".into(),
            action_items: vec![ai("Send deck", None, None)],
            decisions: vec!["Ship Friday".into()],
            open_questions: vec!["Who owns QA?".into()],
            key_topics: vec!["Release".into()],
        };
        let b = SummaryStructured {
            tldr: "Budget is approved. QA owner still open.".into(),
            action_items: vec![ai("Send deck", Some("Ana"), None)],
            decisions: vec!["No decisions were made.".into(), "Ship on Friday".into()],
            open_questions: vec!["Who owns QA?".into()],
            key_topics: vec!["QA".into()],
        };
        let f = merge_classic(&[a, b]);
        assert_eq!(f.tldr, "We agreed to ship Friday. Budget is approved. QA owner still open.");
        assert_eq!(f.action_items.len(), 1);
        assert_eq!(f.action_items[0].owner.as_deref(), Some("Ana"));
        assert_eq!(f.decisions, vec!["Ship on Friday"]);
        assert_eq!(f.open_questions, vec!["Who owns QA?"]);
        assert_eq!(f.key_topics, vec!["Release", "QA"]);
    }

    #[test]
    fn sectioned_fold_merges_identical_headings_only() {
        let sec = |h: &str, c: &[&str]| Section {
            heading: h.into(),
            bullets: true,
            content: c.iter().map(|s| s.to_string()).collect(),
        };
        let a = SectionedSummary {
            exec_summary: "Good sync.".into(),
            sections: vec![sec("Sales", &["Pipeline grew"]), sec("Budget", &["Approved"])],
            action_items: vec![],
        };
        let b = SectionedSummary {
            exec_summary: "Good sync. Risks noted.".into(),
            sections: vec![sec("Sales hiring", &["Two reqs open"]), sec("budget", &["Approved", "Q4 flat"])],
            action_items: vec![],
        };
        let f = merge_sectioned(&[a, b]);
        assert_eq!(f.exec_summary, "Good sync. Risks noted.");
        let heads: Vec<&str> = f.sections.iter().map(|s| s.heading.as_str()).collect();
        assert_eq!(heads, vec!["Sales", "Budget", "Sales hiring"]);
        assert_eq!(f.sections[1].content, vec!["Approved", "Q4 flat"]);
    }
}
