//! Public types for meeting summaries.
use serde::{Deserialize, Serialize};

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
            let text = o
                .get("text")
                .or_else(|| o.get("item"))
                .or_else(|| o.get("description"))
                .and_then(value_to_string)?;
            if text.is_empty() {
                return None;
            }
            let owner = o.get("owner").and_then(value_to_string);
            let due = o.get("due").and_then(value_to_string);
            Some(super::ActionItem { text, owner, due })
        };
        Ok(match v {
            Value::Array(arr) => arr
                .iter()
                .filter_map(|e| match e {
                    Value::Object(o) => from_obj(o),
                    other => value_to_string(other).map(|t| super::ActionItem {
                        text: t,
                        owner: None,
                        due: None,
                    }),
                })
                .collect(),
            Value::String(s) => split_blob(&s)
                .into_iter()
                .map(|t| super::ActionItem {
                    text: t,
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
    pub text: String,
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
            .all(|a| a.text.chars().count() <= Self::MAX_STR_CHARS)
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
            .all(|a| a.text.chars().count() <= SummaryStructured::MAX_STR_CHARS);
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSummary {
    pub schema_version: u32,
    pub session_id: String,
    pub provider: String,
    pub model: String,
    pub generated_at_unix_seconds: i64,
    pub source_inputs_hash: String,
    pub structured: SummaryStructured,
    pub markdown: String,
    pub user_edited: bool,
}
impl SessionSummary {
    pub const SCHEMA: u32 = 1;
}

#[cfg(test)]
mod tests {
    use super::*;
    fn full() -> SummaryStructured {
        SummaryStructured {
            tldr: "x".into(),
            action_items: vec![ActionItem {
                text: "a".into(),
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
                text: "a".into(),
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
        assert_eq!(s.action_items[0].text, "plain task");
        assert_eq!(s.action_items[1].text, "tagged task");
        assert_eq!(s.action_items[1].owner.as_deref(), Some("B"));
    }
}
