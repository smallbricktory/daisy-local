// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Transcript data model — pure data, serde-only.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Track {
    /// Echo-cancelled mic. Preferred when present in the session.
    MicAec,
    /// Raw mic capture (no AEC).
    Mic,
    /// System audio (loopback).
    System,
}

impl Track {
    /// The local-mic side of a session: raw mic or its echo-cancelled twin.
    pub fn is_mic(self) -> bool {
        matches!(self, Track::Mic | Track::MicAec)
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Segment {
    pub start_ms: u32,
    pub end_ms: u32,
    pub text: String,
    pub confidence: Option<f32>,
    /// Diarized speaker cluster id, when the provider returns one. Stable
    /// within a single session; mapped to a display name via
    /// `SessionManifest::speaker_map`. `None` means the provider did not
    /// diarize (older transcripts, providers without diarization).
    #[serde(default)]
    pub speaker_id: Option<u32>,
    /// Per-word timing. When non-empty, the word texts joined by a single
    /// space are exactly `text` and every word lies within
    /// `[start_ms, end_ms]` (see [`Segment::words_consistent`]). Empty for
    /// transcripts from a source without word times (older sessions, phone
    /// imports, HTTP providers) and after a text edit.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub words: Vec<Word>,
    /// Marks a piece cut from one source segment at a speaker change; every
    /// piece of that segment carries the same value (see
    /// [`crate::split::split_by_turns`]). `None` on segments never split.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub split_group: Option<u32>,
}

/// One recognized word with its own span, in the same time base as the owning
/// segment. An engine that reports phrase-level runs stores each run as one
/// `Word`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Word {
    pub text: String,
    pub start_ms: u32,
    pub end_ms: u32,
}

impl Segment {
    /// The segment spanning `words` given as `(text, start_ms, end_ms)` on
    /// the stream clock. Text is the trimmed word texts joined by a single
    /// space; words that trim to nothing are dropped; the span runs from the
    /// first word's start to the last word's end (negative times floor at 0)
    /// and every word is clamped into it. `None` when no word has text.
    /// Every word-built segment comes from here.
    pub fn from_words(words: impl IntoIterator<Item = (String, i64, i64)>) -> Option<Segment> {
        let mut words: Vec<Word> = words
            .into_iter()
            .filter_map(|(text, start, end)| {
                let text = text.trim();
                (!text.is_empty()).then(|| Word {
                    text: text.to_string(),
                    start_ms: start.clamp(0, u32::MAX as i64) as u32,
                    end_ms: end.clamp(0, u32::MAX as i64) as u32,
                })
            })
            .collect();
        let start_ms = words.first()?.start_ms;
        let end_ms = words.last()?.end_ms.max(start_ms);
        for w in &mut words {
            w.start_ms = w.start_ms.clamp(start_ms, end_ms);
            w.end_ms = w.end_ms.clamp(w.start_ms, end_ms);
        }
        let text = words.iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" ");
        Some(Segment { start_ms, end_ms, text, confidence: None, speaker_id: None, words, split_group: None })
    }

    /// True when `words` is empty, or joins to `text` and lies within the span.
    pub fn words_consistent(&self) -> bool {
        self.words.is_empty()
            || (self.words.iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" ")
                == self.text
                && self.words.iter().all(|w| {
                    self.start_ms <= w.start_ms && w.start_ms <= w.end_ms && w.end_ms <= self.end_ms
                }))
    }

    /// Moves the segment and its words by `delta_ms`, flooring at 0.
    pub fn shift(&mut self, delta_ms: i64) {
        let mv = |t: u32| (t as i64 + delta_ms).clamp(0, u32::MAX as i64) as u32;
        self.start_ms = mv(self.start_ms);
        self.end_ms = mv(self.end_ms);
        for w in &mut self.words {
            w.start_ms = mv(w.start_ms);
            w.end_ms = mv(w.end_ms);
        }
    }

    /// Sets the span and clamps every word into it.
    pub fn set_span(&mut self, start_ms: u32, end_ms: u32) {
        self.start_ms = start_ms;
        self.end_ms = end_ms.max(start_ms);
        for w in &mut self.words {
            w.start_ms = w.start_ms.clamp(self.start_ms, self.end_ms);
            w.end_ms = w.end_ms.clamp(w.start_ms, self.end_ms);
        }
    }

    /// Cuts the segment at `keep_ms`, where the audio after it no longer
    /// exists. Words starting at or after the cut are dropped along with their
    /// text (the text is rebuilt from the kept words) and the span ends at the
    /// cut. Without word times the span alone is cut. Returns false when
    /// nothing of the segment remains (it starts at or after the cut, or no
    /// word does).
    pub fn cut_after(&mut self, keep_ms: u32) -> bool {
        if self.start_ms >= keep_ms {
            return false;
        }
        if !self.words.is_empty() {
            self.words.retain(|w| w.start_ms < keep_ms);
            if self.words.is_empty() {
                return false;
            }
            self.text = self.words.iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" ");
        }
        self.set_span(self.start_ms, self.end_ms.min(keep_ms));
        true
    }

    /// Drops the first `n` whitespace-separated tokens of the text and starts
    /// the span at the first kept token: its word's start when every word is
    /// one token, else interpolated across the span by token count (a
    /// phrase-level word cannot be cut at a token boundary, so the words are
    /// cleared). A no-op unless at least one token stays.
    pub fn drop_leading_tokens(&mut self, n: usize) {
        let tokens: Vec<&str> = self.text.split_whitespace().collect();
        if n == 0 || n >= tokens.len() {
            return;
        }
        let one_token_words = self.words.len() == tokens.len();
        let new_start = if one_token_words {
            self.words[n].start_ms
        } else {
            let span = self.end_ms.saturating_sub(self.start_ms);
            self.start_ms + (span as u64 * n as u64 / tokens.len() as u64) as u32
        };
        self.text = tokens[n..].join(" ");
        if one_token_words {
            self.words.drain(..n);
        } else {
            self.words.clear();
        }
        self.set_span(new_start, self.end_ms);
    }

    /// Replaces the text with a rewrite of itself that keeps every word run:
    /// the same number of single-space-separated tokens, each run taking the
    /// same number of tokens it had before (recasing, punctuation and spelling
    /// fixes). The runs keep their times and take the new texts. Unchanged
    /// text keeps everything; any other rewrite clears the words.
    pub fn retext(&mut self, text: String) {
        if text == self.text {
            return;
        }
        let tokens: Vec<&str> = text.split(' ').collect();
        let run_len = |w: &Word| w.text.split(' ').count();
        let fits = !self.words.is_empty()
            && tokens.len() == self.words.iter().map(run_len).sum::<usize>()
            && tokens.iter().all(|t| !t.is_empty() && !t.contains(char::is_whitespace));
        if fits {
            let mut at = 0;
            for w in &mut self.words {
                let k = run_len(w);
                w.text = tokens[at..at + k].join(" ");
                at += k;
            }
        } else {
            self.words.clear();
        }
        self.text = text;
    }

    /// Replaces the text with a rewrite that may add, drop or reorder words
    /// (an LLM polish). Tokens are aligned to the words by longest common
    /// subsequence over their normalised form (lowercase, alphanumerics
    /// only). A matched token keeps its word's times; an unmatched token
    /// becomes a zero-length word at the previous matched word's end (the next
    /// one's start when none precedes). When fewer than half the new tokens
    /// match, or a word is a multi-token run, or the new text is not
    /// single-space separated, the words are cleared.
    pub fn realign(&mut self, text: String) {
        if text == self.text {
            return;
        }
        if self.words.iter().any(|w| w.text.contains(' ')) {
            return self.retext(text);
        }
        let tokens: Vec<&str> = text.split_whitespace().collect();
        let norm = |t: &str| -> String {
            t.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
        };
        let old: Vec<String> = self.words.iter().map(|w| norm(&w.text)).collect();
        let new: Vec<String> = tokens.iter().map(|t| norm(t)).collect();
        let (n, m) = (old.len(), new.len());
        // lcs[i][j]: length for old[i..] against new[j..].
        let mut lcs = vec![vec![0u32; m + 1]; n + 1];
        for i in (0..n).rev() {
            for j in (0..m).rev() {
                lcs[i][j] = if !old[i].is_empty() && old[i] == new[j] {
                    lcs[i + 1][j + 1] + 1
                } else {
                    lcs[i + 1][j].max(lcs[i][j + 1])
                };
            }
        }
        let mut matched: Vec<Option<usize>> = vec![None; m];
        let (mut i, mut j) = (0, 0);
        while i < n && j < m {
            if !old[i].is_empty() && old[i] == new[j] {
                matched[j] = Some(i);
                i += 1;
                j += 1;
            } else if lcs[i + 1][j] >= lcs[i][j + 1] {
                i += 1;
            } else {
                j += 1;
            }
        }
        let hits = matched.iter().flatten().count();
        if self.words.is_empty() || tokens.join(" ") != text || hits * 2 < m {
            self.words.clear();
            self.text = text;
            return;
        }
        let mut out: Vec<Word> = Vec::with_capacity(m);
        for (j, tok) in tokens.iter().enumerate() {
            let (start_ms, end_ms) = match matched[j] {
                Some(i) => (self.words[i].start_ms, self.words[i].end_ms),
                None => {
                    let at = out
                        .last()
                        .map(|w| w.end_ms)
                        .or_else(|| matched[j..].iter().flatten().next().map(|&i| self.words[i].start_ms))
                        .unwrap_or(self.start_ms);
                    (at, at)
                }
            };
            out.push(Word { text: tok.to_string(), start_ms, end_ms });
        }
        self.words = out;
        self.text = text;
    }

    /// Masks profanity in the text and the words.
    pub fn mask_profanity(&mut self) {
        self.text = crate::text::mask_profanity(&self.text);
        for w in &mut self.words {
            w.text = crate::text::mask_profanity(&w.text);
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrackTranscript {
    pub track: Track,
    pub source_wav_relative: PathBuf,
    pub segments: Vec<Segment>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChunkTranscript {
    pub chunk_index: u32,
    pub tracks: Vec<TrackTranscript>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionTranscript {
    pub schema_version: u32,
    pub session_id: String,
    pub provider: String,
    pub model: String,
    pub transcribed_at_unix_seconds: i64,
    pub chunks: Vec<ChunkTranscript>,
    /// True for a transcript this desktop promoted from a phone's live
    /// captions while the phone's own transcript had not arrived. Local to
    /// this desktop; absent reads as false.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub provisional: bool,
    /// The second a person last changed the transcript's text; absent when
    /// nobody has.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edited_at_unix_seconds: Option<i64>,
}

impl SessionTranscript {
    pub const SCHEMA: u32 = 1;

    /// How recent this transcript is when two devices each hold one: the
    /// later of when it was transcribed and when it was last edited, an
    /// absent stamp reading as 0. A provisional transcript ranks by its edit
    /// alone.
    pub fn rank(&self) -> i64 {
        let edited = self.edited_at_unix_seconds.unwrap_or(0);
        match self.provisional {
            true => edited,
            false => self.transcribed_at_unix_seconds.max(edited),
        }
    }
}
