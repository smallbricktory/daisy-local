//! Cross-platform session search: a sequential scan over the profile's
//! session directories.
//!
//! Searched per session:
//!   - Title (manifest)
//!   - Attendee display names (manifest)
//!   - Tag names (manifest tag ids → tags.json display names)
//!   - Notes (notes.md)
//!   - Transcript (transcript.md)
//!   - Summary TL;DR, action items, decisions, open questions, key topics
//!     (summary.json; falls back to summary.md text)
//!
//! Query semantics:
//!   - Multi-token AND: every space-separated token must appear at least
//!     once across the haystack. Tokens match case-insensitively as
//!     substrings.
//!   - Phrases in double quotes match contiguously.
//!   - Tag-id filter and date range apply on top (AND).

use crate::error::Result;
use crate::state::AppState;
use recording::manifest::SessionManifest;
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
pub struct SearchRequest {
    pub query: Option<String>,
    pub tag_ids: Option<Vec<String>>,
    #[serde(default)]
    pub contact_ids: Option<Vec<String>>,
    pub date_from: Option<i64>,
    pub date_to: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct SessionHit {
    pub session_id: String,
    pub title: Option<String>,
    pub created_at_unix_seconds: i64,
    pub tag_ids: Vec<String>,
    /// First-matching snippet.
    pub snippet: Option<String>,
    /// Source of `snippet` (title / transcript / summary / notes / action /
    /// attendee / tag / metadata).
    pub match_source: String,
    /// Up to `MAX_SNIPPETS_PER_HIT` labelled snippets across all matched
    /// sources.
    pub matches: Vec<MatchSnippet>,
}

#[derive(Debug, Serialize)]
pub struct MatchSnippet {
    pub source: String,
    pub snippet: String,
}

const MAX_SNIPPETS_PER_HIT: usize = 4;
const SNIPPET_PAD_CHARS: usize = 60;

pub fn search_sessions_impl(app: &AppState, req: SearchRequest) -> Result<Vec<SessionHit>> {
    let tokens = parse_tokens(req.query.as_deref().unwrap_or(""));
    let want: Option<std::collections::HashSet<String>> =
        req.tag_ids.map(|v| v.into_iter().collect());
    let want_contacts: Option<std::collections::HashSet<String>> =
        req.contact_ids.map(|v| v.into_iter().collect());
    // Contacts are loaded only when filtering by person.
    let contacts = if want_contacts.is_some() {
        crate::commands::contacts::load_contacts(app)?
    } else {
        vec![]
    };

    // Map of tag-id -> name for matching the query against tag names.
    let tag_names: std::collections::HashMap<String, String> = read_tag_names(app);

    let root = app.profile.sessions_dir();
    let mut hits = Vec::new();
    let Ok(rd) = std::fs::read_dir(&root) else {
        return Ok(hits);
    };

    for e in rd.flatten() {
        if !e.path().is_dir() {
            continue;
        }
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let Ok(mb) = syncsafe::read(e.path().join("manifest.json")) else {
            continue;
        };
        let Ok(m) = serde_json::from_slice::<SessionManifest>(&mb) else {
            continue;
        };
        if let Some(f) = req.date_from {
            if m.created_at_unix_seconds < f {
                continue;
            }
        }
        if let Some(t) = req.date_to {
            if m.created_at_unix_seconds > t {
                continue;
            }
        }
        if let Some(w) = &want {
            if !w.iter().all(|t| m.tag_ids.iter().any(|x| x == t)) {
                continue;
            }
        }
        if let Some(wc) = &want_contacts {
            let have = crate::commands::contacts::session_contact_ids(&m, &contacts);
            if !wc.iter().all(|c| have.contains(c)) {
                continue;
            }
        }

        // Build the per-source haystack. Empty strings stay empty and
        // contribute nothing to the match logic.
        let title = m.title.clone().unwrap_or_default();
        let attendees: String = m
            .attendees
            .iter()
            .map(|a| a.display_name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        let tag_label = m
            .tag_ids
            .iter()
            .filter_map(|id| tag_names.get(id).cloned())
            .collect::<Vec<_>>()
            .join(", ");
        let notes = syncsafe::read_to_string(e.path().join("notes.md")).unwrap_or_default();
        let transcript =
            syncsafe::read_to_string(e.path().join("transcript.md")).unwrap_or_default();
        let (summary_text, action_items) = load_summary(&e.path());

        // Sections in display priority order; the first matching section
        // becomes `match_source`.
        let sections: &[(&str, &str)] = &[
            ("title", title.as_str()),
            ("transcript", transcript.as_str()),
            ("summary", summary_text.as_str()),
            ("notes", notes.as_str()),
            ("action", action_items.as_str()),
            ("attendee", attendees.as_str()),
            ("tag", tag_label.as_str()),
        ];

        let keep = if tokens.is_empty() {
            true
        } else {
            all_tokens_match(&tokens, sections)
        };
        if !keep {
            continue;
        }

        let mut matches: Vec<MatchSnippet> = Vec::new();
        if !tokens.is_empty() {
            for (source, body) in sections {
                if matches.len() >= MAX_SNIPPETS_PER_HIT {
                    break;
                }
                if body.is_empty() {
                    continue;
                }
                if let Some(snippet) = first_token_snippet(body, &tokens) {
                    matches.push(MatchSnippet {
                        source: (*source).to_string(),
                        snippet,
                    });
                }
            }
        }
        let (primary_source, primary_snippet) = match matches.first() {
            Some(m) => (m.source.clone(), Some(m.snippet.clone())),
            None => ("metadata".to_string(), None),
        };

        hits.push(SessionHit {
            session_id: name,
            title: m.title.clone(),
            created_at_unix_seconds: m.created_at_unix_seconds,
            tag_ids: m.tag_ids.clone(),
            snippet: primary_snippet,
            match_source: primary_source,
            matches,
        });
    }
    hits.sort_by(|a, b| b.created_at_unix_seconds.cmp(&a.created_at_unix_seconds));
    Ok(hits)
}

/// Read `<profile>/tags.json` and return id → display name.
fn read_tag_names(app: &AppState) -> std::collections::HashMap<String, String> {
    let path = app.profile.root().join("tags.json");
    let Ok(bytes) = syncsafe::read(&path) else {
        return Default::default();
    };
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return Default::default();
    };
    v.get("tags")
        .and_then(|t| t.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|e| {
                    let id = e.get("id").and_then(|x| x.as_str())?;
                    let name = e.get("name").and_then(|x| x.as_str())?;
                    Some((id.to_string(), name.to_string()))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Pull a flattened searchable blob from summary.json (preferred) or
/// summary.md (fallback). Returns (summary_body, action_items_only).
fn load_summary(session_dir: &std::path::Path) -> (String, String) {
    if let Ok(b) = syncsafe::read(session_dir.join("summary.json")) {
        if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&b) {
            let s = v.get("structured");
            let tldr = s.and_then(|x| x.get("tldr")).and_then(|x| x.as_str()).unwrap_or("");
            let decisions = join_str_array(s.and_then(|x| x.get("decisions")));
            let open = join_str_array(s.and_then(|x| x.get("open_questions")));
            let topics = join_str_array(s.and_then(|x| x.get("key_topics")));
            let actions = s
                .and_then(|x| x.get("action_items"))
                .and_then(|a| a.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|e| e.get("text").and_then(|t| t.as_str()).map(String::from))
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_default();
            let summary_body =
                [tldr.to_string(), decisions, open, topics].join("\n");
            return (summary_body, actions);
        }
    }
    let md = syncsafe::read_to_string(session_dir.join("summary.md")).unwrap_or_default();
    (md, String::new())
}

fn join_str_array(v: Option<&serde_json::Value>) -> String {
    v.and_then(|x| x.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|e| e.as_str().map(String::from))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

/// Parse "foo bar" + '"a phrase"' into a list of lowercase tokens.
fn parse_tokens(raw: &str) -> Vec<String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Vec::new();
    }
    let mut out: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut in_quote = false;
    for ch in raw.chars() {
        if ch == '"' {
            if in_quote {
                if !current.trim().is_empty() {
                    out.push(current.trim().to_lowercase());
                }
                current.clear();
                in_quote = false;
            } else {
                if !current.trim().is_empty() {
                    out.push(current.trim().to_lowercase());
                }
                current.clear();
                in_quote = true;
            }
            continue;
        }
        if ch.is_whitespace() && !in_quote {
            if !current.trim().is_empty() {
                out.push(current.trim().to_lowercase());
            }
            current.clear();
            continue;
        }
        current.push(ch);
    }
    if !current.trim().is_empty() {
        out.push(current.trim().to_lowercase());
    }
    out
}

/// True iff every token appears at least once across the sections.
fn all_tokens_match(tokens: &[String], sections: &[(&str, &str)]) -> bool {
    tokens.iter().all(|tok| {
        sections
            .iter()
            .any(|(_, body)| !body.is_empty() && body.to_lowercase().contains(tok))
    })
}

/// Find the first token that appears in `body` (case-insensitive) and
/// return a ±SNIPPET_PAD_CHARS window around it. Returns None if no
/// token matches.
fn first_token_snippet(body: &str, tokens: &[String]) -> Option<String> {
    let lc = body.to_lowercase();
    let mut earliest: Option<(usize, usize)> = None;
    for tok in tokens {
        if let Some(pos) = lc.find(tok) {
            if earliest.map(|(p, _)| pos < p).unwrap_or(true) {
                earliest = Some((pos, tok.chars().count()));
            }
        }
    }
    let (pos, tok_len_chars) = earliest?;

    // Convert byte index `pos` into a char index, then build a char-window.
    let mut chars: Vec<(usize, char)> = body.char_indices().collect();
    chars.push((body.len(), '\0')); // sentinel for end-of-string
    let center_char_idx = chars
        .iter()
        .position(|(b, _)| *b >= pos)
        .unwrap_or(chars.len() - 1);
    let start = center_char_idx.saturating_sub(SNIPPET_PAD_CHARS);
    let end = (center_char_idx + tok_len_chars + SNIPPET_PAD_CHARS).min(chars.len() - 1);
    let s_byte = chars[start].0;
    let e_byte = chars[end].0;

    let prefix = if start == 0 { "" } else { "…" };
    let suffix = if end == chars.len() - 1 { "" } else { "…" };
    // Newlines are collapsed to spaces.
    let raw: String = body[s_byte..e_byte]
        .chars()
        .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
        .collect();
    Some(format!("{prefix}{}{suffix}", raw.trim()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> (AppState, tempfile::TempDir) {
        let tmp = tempfile::tempdir().unwrap();
        let profile = crate::profile::ProfileDir::at(tmp.path()).unwrap();
        (AppState::new(profile), tmp)
    }

    fn write_session(app: &AppState, sid: &str, attendee_names: &[&str]) {
        use recording::manifest::{AecMode, Attendee, AttendeeRole};
        let dir = app.profile.sessions_dir().join(sid);
        syncsafe::create_dir_all(&dir).unwrap();
        let m = SessionManifest {
            schema_version: 2, session_id: sid.into(), created_at_unix_seconds: 0,
            sample_rate: 16000, channels: 1, mic_source_id: 1,
            mic_source_node_name: "m".into(), mic_source_description: "m".into(),
            system_source_id: 2, system_source_node_name: "s".into(),
            system_source_description: "s".into(), aec_mode: AecMode::Disabled,
            chunks: vec![], finalized_at_unix_seconds: None, title: None,
            meeting_id: format!("mid-{sid}"), tag_ids: vec![], notes_md_relative: None,
            attendees: attendee_names.iter().map(|n| Attendee { display_name: (*n).into(), role: AttendeeRole::Other }).collect(),
            calendar: None, recording_segments: vec![],
            speaker_map: vec![], language: None, diarization_unavailable: false,
            single_local_speaker: true,
            expected_speakers: None,
            sent_integration_ids: vec![], cluster_sides: vec![], interrupted: false,
            denoise_applied: None,
        };
        syncsafe::write(dir.join("manifest.json"), serde_json::to_vec(&m).unwrap()).unwrap();
    }

    #[test]
    fn contact_ids_filter_is_and() {
        use crate::commands::contacts::{save_contacts, Contact};
        let (app, _t) = app();
        write_session(&app, "s_alice", &["Alice"]);
        write_session(&app, "s_both", &["Alice", "Bob"]);
        let alice = Contact { id: "a".into(), display_name: "Alice".into(), emails: vec![], created_at_unix_seconds: 1 };
        let bob = Contact { id: "b".into(), display_name: "Bob".into(), emails: vec![], created_at_unix_seconds: 1 };
        save_contacts(&app, &[alice, bob]).unwrap();

        let only_alice = search_sessions_impl(&app, SearchRequest {
            query: None, tag_ids: None, contact_ids: Some(vec!["a".into()]), date_from: None, date_to: None,
        }).unwrap();
        let ids: std::collections::HashSet<_> = only_alice.iter().map(|h| h.session_id.clone()).collect();
        assert_eq!(ids, ["s_alice".to_string(), "s_both".to_string()].into_iter().collect());

        let both = search_sessions_impl(&app, SearchRequest {
            query: None, tag_ids: None, contact_ids: Some(vec!["a".into(), "b".into()]), date_from: None, date_to: None,
        }).unwrap();
        let ids: Vec<_> = both.iter().map(|h| h.session_id.clone()).collect();
        assert_eq!(ids, vec!["s_both".to_string()]); // only the session with BOTH
    }

    #[test]
    fn tokenizer_handles_quoted_phrases() {
        let t = parse_tokens(r#"oracle "approval level" deadline"#);
        assert_eq!(t, vec!["oracle", "approval level", "deadline"]);
    }

    #[test]
    fn tokenizer_lowercases_and_trims() {
        let t = parse_tokens("  Hello  WORLD  ");
        assert_eq!(t, vec!["hello", "world"]);
    }

    #[test]
    fn all_tokens_must_match_across_sections() {
        let sections: &[(&str, &str)] = &[
            ("title", "Oracle migration"),
            ("transcript", "We talked about the deadline."),
        ];
        assert!(all_tokens_match(
            &["oracle".into(), "deadline".into()],
            sections
        ));
        assert!(!all_tokens_match(
            &["oracle".into(), "nonexistent".into()],
            sections
        ));
    }

    #[test]
    fn snippet_returns_window_with_ellipses() {
        let body = "a".repeat(120) + "needle" + &"b".repeat(120);
        let snippet = first_token_snippet(&body, &["needle".into()]).unwrap();
        assert!(snippet.contains("needle"));
        assert!(snippet.starts_with('…'));
        assert!(snippet.ends_with('…'));
    }
}
