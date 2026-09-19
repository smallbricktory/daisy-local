// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Public types for meeting summaries.
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Tolerant deserialization for LLM output: models sometimes ignore the tool
/// schema and stuff XML/HTML-ish text into array fields, or wrap items in
/// tags. These helpers coerce that back into clean string lists.
mod lenient {
    use super::SummaryStructured;
    use serde::Deserialize;
    use serde_json::Value;

    /// Strip `<...>` runs and decode the few HTML entities models commonly emit.
    pub(super) fn clean(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        let mut in_tag = false;
        for ch in s.chars() {
            match ch {
                '<' => in_tag = true,
                '>' => in_tag = false,
                _ if !in_tag => out.push(ch),
                _ => {}
            }
        }
        let out = out
            .replace("&amp;", "&")
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&quot;", "\"")
            .replace("&#39;", "'")
            .replace("&apos;", "'")
            .replace("&nbsp;", " ");
        let trimmed = out.trim();
        if trimmed.chars().count() > SummaryStructured::MAX_STR_CHARS {
            trimmed
                .chars()
                .take(SummaryStructured::MAX_STR_CHARS)
                .collect()
        } else {
            trimmed.to_string()
        }
    }

    /// Turn a free-form blob the model emitted in place of an array into a list
    /// of cleaned strings: prefer `<item>…</item>` chunks, else split on lines,
    /// else treat the whole thing as one entry.
    fn split_blob(s: &str) -> Vec<String> {
        if s.contains("<item") {
            let mut items = Vec::new();
            for part in s.split("</item>") {
                if let Some(idx) = part.find("<item") {
                    if let Some(gt) = part[idx..].find('>') {
                        let inner = &part[idx + gt + 1..];
                        let c = clean(inner);
                        if !c.is_empty() {
                            items.push(c);
                        }
                    }
                }
            }
            return items;
        }
        if s.contains('\n') {
            return s
                .lines()
                .map(|l| clean(l.trim_start_matches(['-', '*', '•', ' '])))
                .filter(|l| !l.is_empty())
                .collect();
        }
        let c = clean(s);
        if c.is_empty() { Vec::new() } else { vec![c] }
    }

    fn value_to_string(v: &Value) -> Option<String> {
        match v {
            Value::String(s) => {
                let c = clean(s);
                (!c.is_empty()).then_some(c)
            }
            Value::Object(o) => o
                .get("text")
                .or_else(|| o.get("item"))
                .or_else(|| o.get("value"))
                .and_then(value_to_string)
                .or_else(|| {
                    let c = clean(&v.to_string());
                    (!c.is_empty()).then_some(c)
                }),
            Value::Number(_) | Value::Bool(_) => Some(v.to_string()),
            _ => None,
        }
    }

    pub fn string<'de, D: serde::Deserializer<'de>>(d: D) -> Result<String, D::Error> {
        let v = Value::deserialize(d)?;
        Ok(match v {
            Value::String(s) => clean(&s),
            Value::Null => String::new(),
            other => clean(&other.to_string()),
        })
    }

    pub fn string_vec<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
        let v = Value::deserialize(d)?;
        Ok(match v {
            Value::Array(arr) => arr.iter().filter_map(value_to_string).collect(),
            Value::String(s) => split_blob(&s),
            Value::Null => Vec::new(),
            other => split_blob(&other.to_string()),
        })
    }

    pub fn action_items<'de, D: serde::Deserializer<'de>>(
        d: D,
    ) -> Result<Vec<super::ActionItem>, D::Error> {
        let v = Value::deserialize(d)?;
        let from_obj = |o: &serde_json::Map<String, Value>| -> Option<super::ActionItem> {
            let task = o
                .get("task")
                .or_else(|| o.get("text"))
                .or_else(|| o.get("item"))
                .or_else(|| o.get("description"))
                .and_then(value_to_string)?;
            if task.is_empty() {
                return None;
            }
            let owner = o
                .get("owner")
                .or_else(|| o.get("assignee"))
                .and_then(value_to_string);
            let due = o.get("due").and_then(value_to_string);
            Some(super::ActionItem { task, owner, due })
        };
        Ok(match v {
            Value::Array(arr) => arr
                .iter()
                .filter_map(|e| match e {
                    Value::Object(o) => from_obj(o),
                    other => value_to_string(other).map(|t| super::ActionItem {
                        task: t,
                        owner: None,
                        due: None,
                    }),
                })
                .collect(),
            Value::String(s) => split_blob(&s)
                .into_iter()
                .map(|t| super::ActionItem {
                    task: t,
                    owner: None,
                    due: None,
                })
                .collect(),
            _ => Vec::new(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionItem {
    #[serde(alias = "text")]
    pub task: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SummaryStructured {
    #[serde(deserialize_with = "lenient::string")]
    pub tldr: String,
    #[serde(default, deserialize_with = "lenient::action_items")]
    pub action_items: Vec<ActionItem>,
    #[serde(default, deserialize_with = "lenient::string_vec")]
    pub decisions: Vec<String>,
    #[serde(default, deserialize_with = "lenient::string_vec")]
    pub open_questions: Vec<String>,
    #[serde(default, deserialize_with = "lenient::string_vec")]
    pub key_topics: Vec<String>,
}
impl SummaryStructured {
    pub const MAX_TLDR_CHARS: usize = 1000;
    pub const MAX_ACTION_ITEMS: usize = 50;
    pub const MAX_STR_CHARS: usize = 2000;
    /// Trims what can be trimmed so an otherwise good summary is kept.
    pub fn clamp(&mut self) {
        if self.tldr.chars().count() > Self::MAX_TLDR_CHARS {
            let before = self.tldr.chars().count();
            self.tldr = clip_to(&self.tldr, Self::MAX_TLDR_CHARS);
            log::info!(
                "summary: tldr clipped from {before} to {} chars",
                self.tldr.chars().count()
            );
        }
        self.action_items.truncate(Self::MAX_ACTION_ITEMS);
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.tldr.chars().count() > Self::MAX_TLDR_CHARS {
            return Err(format!(
                "tldr too long: {} > {}",
                self.tldr.chars().count(),
                Self::MAX_TLDR_CHARS
            ));
        }
        if self.action_items.len() > Self::MAX_ACTION_ITEMS {
            return Err(format!("too many action items: {}", self.action_items.len()));
        }
        let strs_ok = self
            .action_items
            .iter()
            .all(|a| a.task.chars().count() <= Self::MAX_STR_CHARS)
            && self
                .decisions
                .iter()
                .chain(&self.open_questions)
                .chain(&self.key_topics)
                .all(|s| s.chars().count() <= Self::MAX_STR_CHARS);
        if !strs_ok {
            return Err("a string field exceeds the per-string cap".into());
        }
        let empty = self.tldr.trim().is_empty()
            && self.action_items.is_empty()
            && self.decisions.is_empty()
            && self.open_questions.is_empty()
            && self.key_topics.is_empty();
        if empty {
            return Err("summary is empty".into());
        }
        Ok(())
    }
}

/// A heading + body for one section of a sectioned-style summary. Transient:
/// produced at generation time, rendered to markdown, never persisted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Section {
    #[serde(deserialize_with = "lenient::string")]
    pub heading: String,
    #[serde(default)]
    pub bullets: bool,
    #[serde(default, deserialize_with = "lenient::string_vec")]
    pub content: Vec<String>,
}

/// Transient generation-time shape for the `Sectioned` envelope. Rendered to
/// markdown and folded into `SummaryStructured` (exec→tldr, action items carry
/// over); never serialized into summary.json.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SectionedSummary {
    #[serde(deserialize_with = "lenient::string")]
    pub exec_summary: String,
    #[serde(default, deserialize_with = "sections_lenient")]
    pub sections: Vec<Section>,
    #[serde(default, deserialize_with = "lenient::action_items")]
    pub action_items: Vec<ActionItem>,
}

/// Clips an over-long summary at the last sentence end inside the limit,
/// falling back to the last word break, then to a hard cut. Returns the input
/// unchanged when it already fits.
fn clip_to(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let head: String = text.chars().take(max_chars).collect();
    let cut = head
        .rfind(['.', '!', '?'])
        .map(|i| i + 1)
        .or_else(|| head.rfind(char::is_whitespace))
        .unwrap_or(head.len());
    head[..cut].trim_end().to_string()
}

/// Tolerant section-array deserialize: malformed entries are dropped.
fn sections_lenient<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<Section>, D::Error> {
    let v = serde_json::Value::deserialize(d)?;
    Ok(match v {
        serde_json::Value::Array(arr) => arr
            .into_iter()
            .filter_map(|e| serde_json::from_value::<Section>(e).ok())
            .filter(|s| !s.heading.trim().is_empty() || !s.content.is_empty())
            .collect(),
        _ => Vec::new(),
    })
}

impl SectionedSummary {
    pub const MAX_SECTIONS: usize = 12;
    /// Trims what can be trimmed so an otherwise good analysis is kept. The
    /// remaining checks in `validate` describe shapes that cannot be repaired.
    pub fn clamp(&mut self) {
        if self.exec_summary.chars().count() > SummaryStructured::MAX_TLDR_CHARS {
            let before = self.exec_summary.chars().count();
            self.exec_summary =
                clip_to(&self.exec_summary, SummaryStructured::MAX_TLDR_CHARS);
            log::info!(
                "analysis: exec_summary clipped from {before} to {} chars",
                self.exec_summary.chars().count()
            );
        }
        self.action_items.truncate(SummaryStructured::MAX_ACTION_ITEMS);
        self.sections.truncate(Self::MAX_SECTIONS);
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.exec_summary.chars().count() > SummaryStructured::MAX_TLDR_CHARS {
            return Err("exec_summary too long".into());
        }
        if self.sections.len() > Self::MAX_SECTIONS {
            return Err(format!("too many sections: {}", self.sections.len()));
        }
        if self.action_items.len() > SummaryStructured::MAX_ACTION_ITEMS {
            return Err(format!("too many action items: {}", self.action_items.len()));
        }
        let strs_ok = self.sections.iter().all(|s| {
            s.heading.chars().count() <= SummaryStructured::MAX_STR_CHARS
                && s.content
                    .iter()
                    .all(|c| c.chars().count() <= SummaryStructured::MAX_STR_CHARS)
        }) && self
            .action_items
            .iter()
            .all(|a| a.task.chars().count() <= SummaryStructured::MAX_STR_CHARS);
        if !strs_ok {
            return Err("a string field exceeds the per-string cap".into());
        }
        let empty = self.exec_summary.trim().is_empty()
            && self.sections.is_empty()
            && self.action_items.is_empty();
        if empty {
            return Err("summary is empty".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
pub struct TagPromptRef<'a> {
    pub name: &'a str,
    pub prompt_md: &'a str,
    pub terms: &'a str, // comma-joined vocabulary terms ("" when none)
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpeakerRole {
    Me,
    Them,
}
#[derive(Debug, Clone)]
pub struct AttendeeRef<'a> {
    pub display_name: &'a str,
    pub role: SpeakerRole,
}

pub struct SummaryInput<'a> {
    pub session_id: &'a str,
    pub title: Option<&'a str>,
    pub attendees: &'a [AttendeeRef<'a>],
    pub user_notes_md: &'a str, // raw, untrusted
    pub transcript_md: &'a str, // raw, untrusted
    pub tag_prompts: &'a [TagPromptRef<'a>],
    pub style_envelope: crate::prompts::Envelope,
    pub style_directive: &'a str, // user-authored, advisory; fenced like tags
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProducedBy {
    #[serde(default)]
    pub device: String,
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub model: String,
}

/// A chapter as the summary carries it. Reads a bare title or the object,
/// and `start_hms` from the desktop's own chapter extractor; writes `start_ms`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SummaryChapter {
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

impl<'de> Deserialize<'de> for SummaryChapter {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v = Value::deserialize(d)?;
        match v {
            Value::String(title) => Ok(Self {
                title,
                start_ms: None,
                summary: None,
            }),
            Value::Object(o) => {
                let title = o
                    .get("title")
                    .and_then(Value::as_str)
                    .ok_or_else(|| serde::de::Error::missing_field("title"))?
                    .to_string();
                let start_ms = o.get("start_ms").and_then(Value::as_u64).or_else(|| {
                    o.get("start_hms")
                        .and_then(Value::as_str)
                        .map(|h| crate::chapters::hms_to_secs(h) as u64 * 1000)
                });
                let summary = o.get("summary").and_then(Value::as_str).map(String::from);
                Ok(Self {
                    title,
                    start_ms,
                    summary,
                })
            }
            _ => Err(serde::de::Error::custom(
                "a chapter is a string or an object",
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkippedWindow {
    pub start_ms: u64,
    pub end_ms: u64,
}

/// `summary.json`, contract v2 (smbr-daisy-ios docs/sync/protocol.md §7).
/// The shape on disk is the shape on the wire; `decode` is the one reader.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSummary {
    pub v: u32,
    pub session_id: String,
    pub generated_at_unix_seconds: i64,
    #[serde(default)]
    pub produced_by: ProducedBy,
    #[serde(flatten)]
    pub structured: SummaryStructured,
    #[serde(default)]
    pub chapters: Vec<SummaryChapter>,
    #[serde(default)]
    pub markdown: String,
    #[serde(default)]
    pub user_edited: bool,
    #[serde(default)]
    pub skipped: Vec<SkippedWindow>,
    #[serde(default)]
    pub turn_count: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_inputs_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
}

/// What this desktop wrote before v2. Read by `decode`, never written.
#[derive(Debug, Deserialize)]
struct LegacySummaryV1 {
    session_id: String,
    #[serde(default)]
    provider: String,
    #[serde(default)]
    model: String,
    generated_at_unix_seconds: i64,
    #[serde(default)]
    source_inputs_hash: String,
    structured: SummaryStructured,
    #[serde(default)]
    markdown: String,
    #[serde(default)]
    user_edited: bool,
}

impl From<LegacySummaryV1> for SessionSummary {
    fn from(l: LegacySummaryV1) -> Self {
        SessionSummary {
            v: SessionSummary::V,
            session_id: l.session_id,
            generated_at_unix_seconds: l.generated_at_unix_seconds,
            produced_by: ProducedBy {
                device: String::new(),
                provider: l.provider,
                model: l.model,
            },
            structured: l.structured,
            chapters: Vec::new(),
            markdown: l.markdown,
            user_edited: l.user_edited,
            skipped: Vec::new(),
            turn_count: 0,
            source_inputs_hash: (!l.source_inputs_hash.is_empty()).then_some(l.source_inputs_hash),
            language: None,
        }
    }
}

impl SessionSummary {
    pub const V: u32 = 2;

    /// One arm per version this build reads: v2, and the `schema_version: 1`
    /// files this desktop wrote before. Anything else is an error, never a guess.
    pub fn decode(bytes: &[u8]) -> Result<Self, serde_json::Error> {
        let value: Value = serde_json::from_slice(bytes)?;
        match value.get("v").and_then(Value::as_u64) {
            Some(2) => serde_json::from_value(value),
            Some(other) => Err(serde::de::Error::custom(format!(
                "summary v{other} is newer than this build reads"
            ))),
            None if value.get("schema_version").and_then(Value::as_u64) == Some(1) => {
                Ok(serde_json::from_value::<LegacySummaryV1>(value)?.into())
            }
            None => Err(serde::de::Error::custom("summary without a version")),
        }
    }

    /// True when the bytes on disk are already the wire shape.
    pub fn is_v2_bytes(bytes: &[u8]) -> bool {
        serde_json::from_slice::<Value>(bytes)
            .ok()
            .and_then(|v| v.get("v").and_then(Value::as_u64))
            == Some(2)
    }

    pub fn encode(&self) -> Vec<u8> {
        serde_json::to_vec_pretty(self).expect("a summary serializes")
    }

    /// A summary written by hand: markdown only, empty structured fields,
    /// no source hash, marked user-edited.
    pub fn hand_written(session_id: &str, provider: &str, markdown: &str, now: i64) -> Self {
        SessionSummary {
            v: Self::V,
            session_id: session_id.to_string(),
            generated_at_unix_seconds: now,
            produced_by: ProducedBy {
                device: String::new(),
                provider: provider.to_string(),
                model: String::new(),
            },
            structured: SummaryStructured {
                tldr: String::new(),
                action_items: Vec::new(),
                decisions: Vec::new(),
                open_questions: Vec::new(),
                key_topics: Vec::new(),
            },
            chapters: Vec::new(),
            markdown: markdown.to_string(),
            user_edited: true,
            skipped: Vec::new(),
            turn_count: 0,
            source_inputs_hash: None,
            language: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn full() -> SummaryStructured {
        SummaryStructured {
            tldr: "x".into(),
            action_items: vec![ActionItem {
                task: "a".into(),
                owner: None,
                due: None,
            }],
            decisions: vec![],
            open_questions: vec![],
            key_topics: vec![],
        }
    }
    #[test]
    fn validate_accepts_normal() {
        assert!(full().validate().is_ok());
    }
    #[test]
    fn validate_rejects_empty() {
        let s = SummaryStructured {
            tldr: "  ".into(),
            action_items: vec![],
            decisions: vec![],
            open_questions: vec![],
            key_topics: vec![],
        };
        assert!(s.validate().is_err());
    }
    #[test]
    fn validate_rejects_oversized_tldr() {
        let mut s = full();
        s.tldr = "x".repeat(SummaryStructured::MAX_TLDR_CHARS + 1);
        assert!(s.validate().is_err());
    }
    #[test]
    fn validate_rejects_too_many_action_items() {
        let mut s = full();
        s.action_items = (0..SummaryStructured::MAX_ACTION_ITEMS + 1)
            .map(|_| ActionItem {
                task: "a".into(),
                owner: None,
                due: None,
            })
            .collect();
        assert!(s.validate().is_err());
    }

    #[test]
    fn lenient_accepts_proper_arrays() {
        let s: SummaryStructured = serde_json::from_str(
            r#"{"tldr":"hi","action_items":[{"text":"do x","owner":"A","due":null}],
                "decisions":["d1"],"open_questions":["q1","q2"],"key_topics":["t1"]}"#,
        )
        .unwrap();
        assert_eq!(s.tldr, "hi");
        assert_eq!(s.action_items.len(), 1);
        assert_eq!(s.action_items[0].owner.as_deref(), Some("A"));
        assert_eq!(s.open_questions, vec!["q1", "q2"]);
    }

    #[test]
    fn lenient_coerces_string_blob_into_list() {
        // Model emitted an XML-ish string in place of the array.
        let s: SummaryStructured = serde_json::from_str(
            r#"{"tldr":"<b>hi</b>","action_items":[],
                "decisions":[],
                "open_questions":"\n  <item>SSO mandate for <a href=\"x\">Northwind Logistics</a></item>\n  <item>second &amp; thing</item>\n</open_questions>\n",
                "key_topics":"first line\n- second line\n* third"}"#,
        )
        .unwrap();
        assert_eq!(s.tldr, "hi"); // tags stripped
        assert_eq!(
            s.open_questions,
            vec!["SSO mandate for Northwind Logistics", "second & thing"]
        );
        assert_eq!(s.key_topics, vec!["first line", "second line", "third"]);
    }

    #[test]
    fn sectioned_lenient_and_validate() {
        let s: SectionedSummary = serde_json::from_str(
            r#"{"exec_summary":"<b>hi</b>","sections":[
                {"heading":"Pricing","bullets":true,"content":["<i>per-seat</i>","tiered"]},
                {"heading":"Timeline","bullets":false,"content":["Beta in August."]}],
               "action_items":[{"text":"draft FAQ","owner":"you","due":"Fri"}]}"#,
        )
        .unwrap();
        assert_eq!(s.exec_summary, "hi");
        assert_eq!(s.sections.len(), 2);
        assert_eq!(s.sections[0].content, vec!["per-seat", "tiered"]);
        assert!(s.sections[0].bullets);
        assert_eq!(s.action_items.len(), 1);
        assert!(s.validate().is_ok());
    }
    #[test]
    fn sectioned_rejects_empty_and_too_many_sections() {
        let empty = SectionedSummary {
            exec_summary: "  ".into(),
            sections: vec![],
            action_items: vec![],
        };
        assert!(empty.validate().is_err());
        let mut many = SectionedSummary {
            exec_summary: "x".into(),
            sections: vec![],
            action_items: vec![],
        };
        many.sections = (0..SectionedSummary::MAX_SECTIONS + 1)
            .map(|_| Section {
                heading: "h".into(),
                bullets: false,
                content: vec!["c".into()],
            })
            .collect();
        assert!(many.validate().is_err());
    }
    #[test]
    fn lenient_action_items_from_strings_and_objects() {
        let s: SummaryStructured = serde_json::from_str(
            r#"{"tldr":"x","action_items":["plain task",{"text":"<i>tagged</i> task","owner":"B"}],
                "decisions":[],"open_questions":[],"key_topics":[]}"#,
        )
        .unwrap();
        assert_eq!(s.action_items.len(), 2);
        assert_eq!(s.action_items[0].task, "plain task");
        assert_eq!(s.action_items[1].task, "tagged task");
        assert_eq!(s.action_items[1].owner.as_deref(), Some("B"));
    }

    #[test]
    fn clip_to_prefers_a_sentence_end() {
        let t = "One. Two. Three that runs past the limit and keeps going.";
        let out = clip_to(t, 12);
        assert_eq!(out, "One. Two.");
    }

    #[test]
    fn clip_to_falls_back_to_a_word_break() {
        let out = clip_to("alpha beta gamma delta", 14);
        assert_eq!(out, "alpha beta");
    }

    #[test]
    fn clip_to_leaves_short_text_alone() {
        assert_eq!(clip_to("short", 100), "short");
    }

    #[test]
    fn an_over_long_exec_summary_is_clipped_not_rejected() {
        let mut s = SectionedSummary {
            exec_summary: "word ".repeat(400),
            sections: Vec::new(),
            action_items: Vec::new(),
        };
        assert!(s.validate().is_err(), "precondition: too long to validate");
        s.clamp();
        assert!(
            s.exec_summary.chars().count() <= SummaryStructured::MAX_TLDR_CHARS,
            "clamp left it over the limit"
        );
        assert!(s.validate().is_ok(), "clamped summary should validate");
        assert!(!s.exec_summary.is_empty(), "clamp must not empty it");
    }

    #[test]
    fn an_over_long_tldr_is_clipped_not_rejected() {
        let mut s = full();
        s.tldr = "word ".repeat(400);
        assert!(s.validate().is_err());
        s.clamp();
        assert!(s.validate().is_ok());
    }

}
