// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! `notes.json`: the page a person typed on the phone while recording or
//! listening back, and the annotations they pinned to transcript lines.
//! Phone-owned; the desktop reads it into Ask and search.
use std::path::Path;

use serde::Deserialize;

pub const FILE_NAME: &str = "notes.json";

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct NoteDocument {
    #[serde(default)]
    pub runs: Vec<Run>,
    #[serde(default)]
    pub annotations: Vec<Annotation>,
}

/// One span of the body. Style flags are the phone's business.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct Run {
    #[serde(default)]
    pub text: String,
    /// The session clock when the text was typed; absent when no clock was
    /// available.
    pub stamp_ms: Option<u32>,
}

/// A note pinned to a transcript line.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct Annotation {
    pub at_ms: u32,
    /// The speaker the line showed when it was pinned.
    pub speaker: Option<String>,
    #[serde(default)]
    pub text: String,
}

/// One line of the notes as Ask and search read it.
#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    pub start_ms: Option<u32>,
    pub text: String,
}

impl NoteDocument {
    pub fn parse(bytes: &[u8]) -> Option<NoteDocument> {
        serde_json::from_slice(bytes).ok()
    }

    /// The notes beside a session's transcript, or `None` when the session
    /// has none.
    pub fn load(session_dir: &Path) -> Option<NoteDocument> {
        let bytes = syncsafe::read(session_dir.join(FILE_NAME)).ok()?;
        NoteDocument::parse(&bytes)
    }

    /// The body split at newlines, blank lines dropped. A paragraph's stamp
    /// is the earliest stamp among its runs.
    pub fn paragraphs(&self) -> Vec<Line> {
        let mut out = Vec::new();
        let mut text = String::new();
        let mut stamp: Option<u32> = None;
        let mut flush = |text: &mut String, stamp: &mut Option<u32>| {
            let trimmed = text.trim_matches([' ', '\t']);
            if !trimmed.is_empty() {
                out.push(Line { start_ms: *stamp, text: trimmed.to_string() });
            }
            text.clear();
            *stamp = None;
        };
        for run in &self.runs {
            for c in run.text.chars() {
                if c == '\n' {
                    flush(&mut text, &mut stamp);
                } else {
                    text.push(c);
                    if let Some(s) = run.stamp_ms {
                        stamp = Some(stamp.map_or(s, |held| held.min(s)));
                    }
                }
            }
        }
        flush(&mut text, &mut stamp);
        out
    }

    /// Every paragraph and every annotation in the shape the transcript
    /// chunks take, so an answer can cite them: `[m:ss] Note: …` and
    /// `[m:ss] Note on <speaker>: …`.
    pub fn lines(&self) -> Vec<Line> {
        let mut out: Vec<Line> = self
            .paragraphs()
            .into_iter()
            .map(|p| Line {
                start_ms: p.start_ms,
                text: match p.start_ms {
                    Some(ms) => format!("[{}] Note: {}", timecode(ms), p.text),
                    None => format!("Note: {}", p.text),
                },
            })
            .collect();
        out.extend(self.annotations.iter().map(|a| Line {
            start_ms: Some(a.at_ms),
            text: format!(
                "[{}] Note on {}: {}",
                timecode(a.at_ms),
                a.speaker.as_deref().unwrap_or("the transcript"),
                a.text
            ),
        }));
        out
    }
}

/// `m:ss`, with hours in front once there are any: the phone's timecode.
fn timecode(ms: u32) -> String {
    let total = ms / 1000;
    let (h, m, s) = (total / 3600, (total / 60) % 60, total % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(json: &str) -> NoteDocument {
        NoteDocument::parse(json.as_bytes()).expect("parses")
    }

    #[test]
    fn the_phones_shape_reads_with_its_style_flags_left_alone() {
        let d = doc(
            r#"{"v":1,"runs":[{"text":"Harbour budget question ","stamp_ms":174000},
                {"text":"for Alex","stamp_ms":176500,"bold":true,"style":"heading"}],
                "annotations":[{"id":"u1","at_ms":5000,"speaker":"Alex",
                "quote":"The survey came back on Tuesday.","text":"Check this figure",
                "created":"2026-09-06T21:40:12Z"}]}"#,
        );
        assert_eq!(d.runs.len(), 2);
        assert_eq!(d.annotations[0].speaker.as_deref(), Some("Alex"));
    }

    #[test]
    fn paragraphs_split_at_newlines_and_take_the_earliest_stamp() {
        let d = doc(
            r#"{"runs":[{"text":"Harbour budget question ","stamp_ms":174000},
                {"text":"for Alex\n","stamp_ms":176500},
                {"text":"\n  \n","stamp_ms":180000},
                {"text":"typed with ","stamp_ms":200000},{"text":"an earlier run","stamp_ms":190000},
                {"text":"\nno clock"}]}"#,
        );
        assert_eq!(
            d.paragraphs(),
            vec![
                Line { start_ms: Some(174000), text: "Harbour budget question for Alex".into() },
                Line { start_ms: Some(190000), text: "typed with an earlier run".into() },
                Line { start_ms: None, text: "no clock".into() },
            ]
        );
    }

    #[test]
    fn lines_take_the_phones_shape_so_both_indexes_agree() {
        let d = doc(
            r#"{"runs":[{"text":"Budget question\n","stamp_ms":754000},{"text":"typed with no clock\n"}],
                "annotations":[{"at_ms":12000,"speaker":"Alex","quote":"q","text":"check this"},
                               {"at_ms":20000,"quote":"q","text":"and that"},
                               {"at_ms":3723000,"speaker":"Kim","quote":"q","text":"an hour in"}]}"#,
        );
        let lines = d.lines();
        let texts: Vec<&str> = lines.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(
            texts,
            [
                "[12:34] Note: Budget question",
                "Note: typed with no clock",
                "[0:12] Note on Alex: check this",
                "[0:20] Note on the transcript: and that",
                "[1:02:03] Note on Kim: an hour in",
            ]
        );
        let starts: Vec<Option<u32>> = lines.iter().map(|l| l.start_ms).collect();
        assert_eq!(starts, [Some(754000), None, Some(12000), Some(20000), Some(3723000)]);
    }

    #[test]
    fn an_empty_page_has_no_lines() {
        assert!(doc(r#"{"v":1,"runs":[],"annotations":[]}"#).lines().is_empty());
        assert!(doc(r#"{"runs":[{"text":"\n\n"}]}"#).lines().is_empty());
    }

    #[test]
    fn a_session_without_the_file_has_no_notes() {
        let td = tempfile::tempdir().unwrap();
        assert_eq!(NoteDocument::load(td.path()), None);
        std::fs::write(td.path().join(FILE_NAME), br#"{"runs":[{"text":"hi","stamp_ms":1000}]}"#).unwrap();
        assert_eq!(NoteDocument::load(td.path()).unwrap().paragraphs().len(), 1);
        std::fs::write(td.path().join(FILE_NAME), b"not json").unwrap();
        assert_eq!(NoteDocument::load(td.path()), None, "a file this build cannot read is no notes");
    }
}
