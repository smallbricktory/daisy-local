// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Tag suggestion matcher functions.

use crate::error::Result;
use crate::state::AppState;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};

/// Fold for matching: full Unicode lowercase.
pub fn fold(s: &str) -> String {
    s.to_lowercase()
}

/// A word char for boundary purposes: Alphabetic or numeric.
fn is_word_char(c: char) -> bool {
    c.is_alphabetic() || c.is_numeric()
}

/// Non-overlapping occurrences of `term` in `text` (both pre-folded).
/// Interior whitespace in the term matches any nonempty whitespace run.
/// A boundary is required on each side whose term edge char is a word char;
/// hyphens/apostrophes inside a term are literal.
pub fn count_term_hits(text: &str, term: &str) -> u32 {
    let t: Vec<char> = text.chars().collect();
    let p: Vec<char> = term.chars().collect();
    if p.is_empty() {
        return 0;
    }
    let mut count = 0u32;
    let mut i = 0usize;
    while i < t.len() {
        if let Some(end) = match_at(&t, i, &p) {
            let start_ok = !is_word_char(p[0]) || i == 0 || !is_word_char(t[i - 1]);
            let end_ok =
                !is_word_char(*p.last().unwrap()) || end >= t.len() || !is_word_char(t[end]);
            if start_ok && end_ok {
                count += 1;
                i = end.max(i + 1);
                continue;
            }
        }
        i += 1;
    }
    count
}

/// Try to match `pat` at `start`; returns the exclusive end index on success.
fn match_at(text: &[char], start: usize, pat: &[char]) -> Option<usize> {
    let (mut ti, mut pi) = (start, 0usize);
    while pi < pat.len() {
        if pat[pi].is_whitespace() {
            if ti >= text.len() || !text[ti].is_whitespace() {
                return None;
            }
            while ti < text.len() && text[ti].is_whitespace() {
                ti += 1;
            }
            while pi < pat.len() && pat[pi].is_whitespace() {
                pi += 1;
            }
            continue;
        }
        if ti >= text.len() || text[ti] != pat[pi] {
            return None;
        }
        ti += 1;
        pi += 1;
    }
    Some(ti)
}

pub const GENERIC_TITLE_DENYLIST: [&str; 9] = [
    "standup", "daily standup", "1:1", "weekly sync", "sync",
    "planning", "check-in", "retro", "all hands",
];

pub const MAX_VOCAB_SUGGESTIONS: usize = 3;
pub const TERM_DISTINCT_MIN: usize = 2;
pub const SINGLE_TERM_HITS_MIN: u32 = 2;
pub const NAME_ONLY_HITS_MIN: u32 = 3;

#[derive(Debug, Clone, serde::Serialize)]
pub struct TermEvidence {
    pub term: String,
    pub count: u32,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct VocabSuggestion {
    pub tag_id: String,
    pub total_hits: u32,
    pub evidence: Vec<TermEvidence>,
}

/// Trim, lowercase, collapse internal whitespace to single spaces.
pub fn normalize_title(t: &str) -> String {
    t.trim().to_lowercase().split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn title_is_generic(normalized: &str) -> bool {
    GENERIC_TITLE_DENYLIST.contains(&normalized)
}

/// All qualifying tags, unfiltered and uncapped, ordered by total hits desc
/// (tag_id asc breaks ties). Callers subtract applied/dismissed and cap.
pub fn vocab_suggestions(tags: &[crate::state::Tag], transcript: &str) -> Vec<VocabSuggestion> {
    let text = fold(transcript);
    let mut out = Vec::new();
    for tag in tags {
        let vocab_terms: Vec<String> = tag
            .vocab_md
            .as_deref()
            .map(crate::commands::transcribe_priming::parse_terms)
            .unwrap_or_default();
        let mut evidence: Vec<TermEvidence> = Vec::new();
        let mut distinct_vocab = 0usize;
        let mut max_single = 0u32;
        for term in &vocab_terms {
            let n = count_term_hits(&text, &fold(term));
            if n > 0 {
                distinct_vocab += 1;
                max_single = max_single.max(n);
                evidence.push(TermEvidence { term: term.clone(), count: n });
            }
        }
        let name_folded = fold(&tag.name);
        let name_is_vocab_term = vocab_terms.iter().any(|t| fold(t) == name_folded);
        let name_hits = if tag.name.chars().count() >= 3 && !name_is_vocab_term {
            count_term_hits(&text, &name_folded)
        } else {
            0
        };
        if name_hits > 0 {
            evidence.push(TermEvidence { term: tag.name.clone(), count: name_hits });
        }
        let qualifies = distinct_vocab >= TERM_DISTINCT_MIN
            || max_single >= SINGLE_TERM_HITS_MIN
            || name_hits >= NAME_ONLY_HITS_MIN;
        if qualifies {
            let total_hits = evidence.iter().map(|e| e.count).sum();
            out.push(VocabSuggestion { tag_id: tag.id.clone(), total_hits, evidence });
        }
    }
    out.sort_by(|a, b| {
        b.total_hits.cmp(&a.total_hits).then_with(|| a.tag_id.cmp(&b.tag_id))
    });
    out
}

/// Rule 1 candidate selection: the most recent finalized session (excluding
/// the current one) matching by calendar identity or non-generic normalized
/// title. Ties on the timestamp break by session_id (greater wins) so the
/// result is deterministic across platforms.
pub fn find_last_time<'a>(
    candidates: &'a [crate::commands::library::SessionListEntry],
    exclude_session_id: Option<&str>,
    title: Option<&str>,
    calendar: Option<&recording::manifest::CalendarLink>,
) -> Option<&'a crate::commands::library::SessionListEntry> {
    let norm = title
        .map(normalize_title)
        .filter(|t| !t.is_empty() && !title_is_generic(t));
    candidates
        .iter()
        .filter(|c| Some(c.session_id.as_str()) != exclude_session_id)
        .filter(|c| c.finalized_at_unix_seconds.is_some())
        .filter(|c| {
            let cal = match (calendar, c.calendar.as_ref()) {
                (Some(a), Some(b)) => a.provider == b.provider && a.event_id == b.event_id,
                _ => false,
            };
            let title = match (&norm, &c.title) {
                (Some(n), Some(ct)) => {
                    let cn = normalize_title(ct);
                    !title_is_generic(&cn) && cn == *n
                }
                _ => false,
            };
            cal || title
        })
        .max_by(|a, b| {
            a.finalized_at_unix_seconds
                .cmp(&b.finalized_at_unix_seconds)
                .then_with(|| a.session_id.cmp(&b.session_id))
        })
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct LastTimeSuggestion {
    pub source_session_id: String,
    pub tag_ids: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SessionSuggestions {
    pub last_time: Option<LastTimeSuggestion>,
    pub vocab: Vec<VocabSuggestion>,
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// A prior scan's evidence: the exact transcript + catalog content it was
/// computed from, keyed by session id. Reused as-is when both hashes still
/// match; discarded and replaced on any mismatch.
struct VocabMemo {
    transcript_sha256: String,
    catalog_sha256: String,
    suggestions: Vec<VocabSuggestion>,
}

fn vocab_memo() -> &'static Mutex<HashMap<String, VocabMemo>> {
    static MEMO: OnceLock<Mutex<HashMap<String, VocabMemo>>> = OnceLock::new();
    MEMO.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The unfiltered, uncapped rule-2 scan for `session_id`, served from the
/// process-global memo when the transcript and tag catalog are byte-for-byte
/// what produced the cached entry; recomputed and cached otherwise.
fn vocab_suggestions_memoized(
    session_id: &str,
    transcript: &str,
    tags: &[crate::state::Tag],
) -> Vec<VocabSuggestion> {
    let transcript_sha256 = sha256_hex(transcript.as_bytes());
    let catalog_bytes = serde_json::to_vec(tags).unwrap_or_default();
    let catalog_sha256 = sha256_hex(&catalog_bytes);

    let mut guard = vocab_memo().lock().unwrap();
    if let Some(entry) = guard.get(session_id) {
        if entry.transcript_sha256 == transcript_sha256 && entry.catalog_sha256 == catalog_sha256
        {
            return entry.suggestions.clone();
        }
    }
    let computed = vocab_suggestions(tags, transcript);
    guard.insert(
        session_id.to_string(),
        VocabMemo { transcript_sha256, catalog_sha256, suggestions: computed.clone() },
    );
    computed
}

/// Rule 1 for the pre-record screen: matches over the full session list (no
/// exclusion — there is no "current session" yet). `None` when both `title`
/// and `calendar` are empty/absent, when nothing matches, or when every
/// matched tag id has since been deleted from the catalog.
pub fn tag_suggest_prerecord_impl(
    app: &AppState,
    title: Option<String>,
    calendar: Option<recording::manifest::CalendarLink>,
) -> Result<Option<LastTimeSuggestion>> {
    let title = title.filter(|t| !t.trim().is_empty());
    if title.is_none() && calendar.is_none() {
        return Ok(None);
    }
    let sessions = crate::commands::library::list_sessions_impl(app)?;
    let Some(matched) = find_last_time(&sessions, None, title.as_deref(), calendar.as_ref())
    else {
        return Ok(None);
    };
    let tags_file = crate::commands::tags::load_tags_file(app)?;
    let existing: HashSet<&str> = tags_file.tags.iter().map(|t| t.id.as_str()).collect();
    let tag_ids: Vec<String> = matched
        .tag_ids
        .iter()
        .filter(|id| existing.contains(id.as_str()))
        .cloned()
        .collect();
    if tag_ids.is_empty() {
        return Ok(None);
    }
    Ok(Some(LastTimeSuggestion { source_session_id: matched.session_id.clone(), tag_ids }))
}

/// Both suggestion rules for an existing session: rule 1 excludes the
/// session itself and subtracts applied + dismissed + deleted tags; rule 2
/// runs only when the session has transcript text, subtracts applied +
/// dismissed, and caps at [`MAX_VOCAB_SUGGESTIONS`].
pub fn tag_suggest_for_session_impl(app: &AppState, session_id: &str) -> Result<SessionSuggestions> {
    let m = crate::commands::meeting::load_manifest(app, session_id)?;
    let tags_file = crate::commands::tags::load_tags_file(app)?;
    let existing: HashSet<&str> = tags_file.tags.iter().map(|t| t.id.as_str()).collect();
    let applied: HashSet<&str> = m.tag_ids.iter().map(String::as_str).collect();
    let dismissed: HashSet<&str> = m.dismissed_suggested_tag_ids.iter().map(String::as_str).collect();

    let sessions = crate::commands::library::list_sessions_impl(app)?;
    let last_time = find_last_time(&sessions, Some(session_id), m.title.as_deref(), m.calendar.as_ref())
        .and_then(|matched| {
            let tag_ids: Vec<String> = matched
                .tag_ids
                .iter()
                .filter(|id| {
                    existing.contains(id.as_str())
                        && !applied.contains(id.as_str())
                        && !dismissed.contains(id.as_str())
                })
                .cloned()
                .collect();
            if tag_ids.is_empty() {
                None
            } else {
                Some(LastTimeSuggestion { source_session_id: matched.session_id.clone(), tag_ids })
            }
        });

    let session_dir = app.profile.session_path(session_id);
    let vocab = match crate::commands::session_store::load_transcript_md(
        &session_dir,
        crate::commands::session_store::TranscriptSource::Optional,
    ) {
        None => vec![],
        Some(transcript) => {
            let unfiltered = vocab_suggestions_memoized(session_id, &transcript, &tags_file.tags);
            unfiltered
                .into_iter()
                .filter(|s| !applied.contains(s.tag_id.as_str()) && !dismissed.contains(s.tag_id.as_str()))
                .take(MAX_VOCAB_SUGGESTIONS)
                .collect()
        }
    };

    Ok(SessionSuggestions { last_time, vocab })
}

/// Appends `tag_ids` to the session's dismissed list (dedup, preserving
/// existing order), follows every id the catalog retired, and saves.
pub fn session_dismiss_tag_suggestions_impl(
    app: &AppState,
    session_id: &str,
    tag_ids: Vec<String>,
) -> Result<()> {
    let at = crate::now_unix();
    crate::commands::tags::edit_session_tags(app, &crate::commands::meeting::session_dir(app, session_id), |meta| {
        let mut dismissed = meta.dismissed_tag_ids();
        for id in tag_ids {
            if !dismissed.contains(&id) {
                dismissed.push(id);
            }
        }
        meta.set_dismissed_tag_ids(dismissed, at);
    })
}

/// Removes `tag_ids` from the session's dismissed list, follows every id the
/// catalog retired (as [`session_dismiss_tag_suggestions_impl`] does), and
/// saves.
pub fn session_undismiss_tag_suggestions_impl(
    app: &AppState,
    session_id: &str,
    tag_ids: Vec<String>,
) -> Result<()> {
    let at = crate::now_unix();
    let remove: HashSet<&str> = tag_ids.iter().map(String::as_str).collect();
    crate::commands::tags::edit_session_tags(app, &crate::commands::meeting::session_dir(app, session_id), |meta| {
        let kept: Vec<String> = meta
            .dismissed_tag_ids()
            .into_iter()
            .filter(|id| !remove.contains(id.as_str()))
            .collect();
        meta.set_dismissed_tag_ids(kept, at);
    })
}

/// Atomic accept for the last-time chip: sets the session's tag set to `tag_ids` and appends `dismiss_tag_ids` to the dismissed list in one manifest save.
pub fn session_apply_tag_suggestions_impl(
    app: &AppState,
    session_id: &str,
    tag_ids: Vec<String>,
    dismiss_tag_ids: Vec<String>,
) -> Result<()> {
    let at = crate::now_unix();
    crate::commands::tags::edit_session_tags(app, &crate::commands::meeting::session_dir(app, session_id), |meta| {
        meta.set_tag_ids(tag_ids, at);
        let mut dismissed = meta.dismissed_tag_ids();
        for id in dismiss_tag_ids {
            if !dismissed.contains(&id) {
                dismissed.push(id);
            }
        }
        meta.set_dismissed_tag_ids(dismissed, at);
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::library::SessionListEntry;
    use recording::manifest::CalendarLink;

    #[derive(serde::Deserialize)]
    struct Fixtures { cases: Vec<Case> }
    #[derive(serde::Deserialize)]
    struct Case { name: String, text: String, term: String, expected: u32 }

    #[test]
    fn matcher_fixtures() {
        let f: Fixtures = serde_json::from_str(include_str!(
            "../../tests/fixtures/tag_suggest/matcher.json"
        ))
        .unwrap();
        for c in f.cases {
            assert_eq!(
                count_term_hits(&fold(&c.text), &fold(&c.term)),
                c.expected,
                "case: {}",
                c.name
            );
        }
    }

    // Helper function to build minimal SessionListEntry for tests
    fn entry(id: &str, fin: Option<i64>, title: Option<&str>, tags: &[&str],
             cal: Option<(&str, &str)>) -> SessionListEntry {
        SessionListEntry {
            session_id: id.into(),
            created_at_unix_seconds: 0,
            finalized_at_unix_seconds: fin,
            duration_seconds: None,
            title: title.map(Into::into),
            tag_ids: tags.iter().map(|s| s.to_string()).collect(),
            has_transcript: false, has_dedup: false, has_summary: false,
            interrupted: false,
            recording_kind: None,
            calendar: cal.map(|(p, e)| CalendarLink {
                provider: p.into(), event_id: e.into(), recurrence_id: None,
                planned_start_unix_seconds: 0, planned_end_unix_seconds: 0,
            }),
        }
    }

    #[test]
    fn normalize_trims_lowers_collapses() {
        assert_eq!(normalize_title("  Weekly   SYNC \n"), "weekly sync");
    }

    #[test]
    fn calendar_match_beats_missing_title_and_ignores_recurrence_id() {
        let c = [entry("a", Some(10), None, &["t1"], Some(("s", "uid")))];
        let cal = CalendarLink { provider: "s".into(), event_id: "uid".into(),
            recurrence_id: Some("r9".into()), planned_start_unix_seconds: 0,
            planned_end_unix_seconds: 0 };
        assert_eq!(find_last_time(&c, None, None, Some(&cal)).unwrap().session_id, "a");
    }

    #[test]
    fn title_match_requires_non_generic() {
        let c = [entry("a", Some(10), Some("Standup"), &["t1"], None),
                 entry("b", Some(20), Some("Coastline review"), &["t2"], None)];
        assert!(find_last_time(&c, None, Some("standup"), None).is_none());
        assert_eq!(find_last_time(&c, None, Some(" Coastline   Review"), None)
            .unwrap().session_id, "b");
    }

    #[test]
    fn only_finalized_counts_and_current_session_excluded() {
        let c = [entry("cur", Some(30), Some("X"), &["t"], None),
                 entry("unfin", None, Some("X"), &["t"], None),
                 entry("old", Some(10), Some("X"), &["t"], None)];
        assert_eq!(find_last_time(&c, Some("cur"), Some("X"), None)
            .unwrap().session_id, "old");
    }

    #[test]
    fn most_recent_wins_session_id_breaks_ties() {
        let c = [entry("a", Some(10), Some("X"), &["t1"], None),
                 entry("b", Some(10), Some("X"), &["t2"], None)];
        assert_eq!(find_last_time(&c, None, Some("X"), None).unwrap().session_id, "b");
    }

    #[test]
    fn most_recent_match_wins_even_when_untagged() {
        let c = [entry("tagged-old", Some(10), Some("X"), &["t1"], None),
                 entry("untagged-new", Some(20), Some("X"), &[], None)];
        assert_eq!(find_last_time(&c, None, Some("X"), None).unwrap().session_id,
            "untagged-new");
    }

    // Helper function to build minimal Tag for vocab_suggestions tests
    fn tag(id: &str, name: &str, vocab: Option<&str>) -> crate::state::Tag {
        crate::state::Tag {
            id: id.into(),
            name: name.into(),
            color_hex: "#000000".into(),
            prompt_md: None,
            vocab_md: vocab.map(Into::into),
            created_at_unix_seconds: 0,
            use_count: 0,
            extra: Default::default(), updated_at_unix_seconds: None,
        }
    }

    #[test]
    fn two_distinct_vocab_terms_qualify() {
        let t = [tag("t1", "Brewhaven", Some("BH-Cloud\nbrew kettle"))];
        let out = vocab_suggestions(&t, "The BH-Cloud rollout needs a brew kettle check.");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].tag_id, "t1");
    }

    #[test]
    fn one_vocab_term_twice_qualifies() {
        let t = [tag("t1", "Ops", Some("runbook"))];
        let out = vocab_suggestions(&t, "runbook here, runbook there");
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn one_vocab_term_once_does_not_qualify() {
        let t = [tag("t1", "Ops", Some("runbook"))];
        assert!(vocab_suggestions(&t, "runbook once only").is_empty());
    }

    #[test]
    fn name_needs_three_hits() {
        let t = [tag("t1", "Design", None)];
        assert!(vocab_suggestions(&t, "design review of the design").is_empty());
        assert_eq!(vocab_suggestions(&t, "design design design").len(), 1);
    }

    #[test]
    fn short_names_never_match() {
        let t = [tag("t1", "QA", None)];
        assert!(vocab_suggestions(&t, "QA QA QA QA").is_empty());
    }

    #[test]
    fn name_hits_do_not_count_toward_distinct_vocab() {
        // one vocab term ×1 + name ×2 → no qualification path satisfied
        let t = [tag("t1", "Sales", Some("pipeline"))];
        assert!(vocab_suggestions(&t, "sales pipeline and sales").is_empty());
    }

    #[test]
    fn evidence_lists_every_matching_term_with_counts() {
        let t = [tag("t1", "Brewhaven", Some("BH-Cloud"))];
        let out = vocab_suggestions(
            &t,
            "Brewhaven Brewhaven Brewhaven BH-Cloud BH-Cloud",
        );
        assert_eq!(out.len(), 1);
        let ev = &out[0].evidence;
        assert!(ev.iter().any(|e| e.term == "BH-Cloud" && e.count == 2));
        assert!(ev.iter().any(|e| e.term == "Brewhaven" && e.count == 3));
        assert_eq!(out[0].total_hits, 5);
    }

    #[test]
    fn ordering_by_total_hits_desc() {
        let t = [tag("low", "Lowtag", Some("alpha")),
                 tag("high", "Hightag", Some("beta"))];
        let out = vocab_suggestions(&t, "alpha alpha beta beta beta");
        assert_eq!(out[0].tag_id, "high");
        assert_eq!(out[1].tag_id, "low");
    }

    #[test]
    fn name_equal_to_vocab_term_counts_once() {
        let t = [tag("t1", "Brewhaven", Some("Brewhaven"))];
        let out = vocab_suggestions(&t, "Brewhaven twice: Brewhaven");
        assert_eq!(out.len(), 1, "vocab-term path qualifies at 2 hits");
        assert_eq!(out[0].evidence.len(), 1, "no duplicate name evidence row");
    }

    #[test]
    fn constants_match_spec() {
        assert_eq!(MAX_VOCAB_SUGGESTIONS, 3);
        assert_eq!(TERM_DISTINCT_MIN, 2);
        assert_eq!(SINGLE_TERM_HITS_MIN, 2);
        assert_eq!(NAME_ONLY_HITS_MIN, 3);
        assert_eq!(GENERIC_TITLE_DENYLIST.len(), 9);
    }
}
