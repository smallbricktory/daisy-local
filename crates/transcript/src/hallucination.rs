// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Text-only hallucination filters, applied to every track.
//!
//! Removes two whole-segment shapes:
//!
//! * video outros ("Thanks for watching!", "Don't forget to subscribe!");
//! * repetition loops, where one line repeats across consecutive segments.
//!
//! Both are whole-segment judgements. A phrase inside a longer sentence is
//! left alone. Nothing here reads audio.

use crate::model::SessionTranscript;

/// Lowercased, punctuation- and marker-stripped form used for comparison.
use crate::text::normalize_line as normalize;

/// Normalized segment texts treated as video outros. Matched whole-segment.
const OUTROS: &[&str] = &[
    "thanks for watching",
    "thank you for watching",
    "thanks for watching this video",
    "thanks for watching and see you next time",
    "see you next time",
    "see you in the next video",
    "subscribe",
    "please subscribe",
    "don't forget to subscribe",
    "like and subscribe",
    "please subscribe to my channel",
    "if you liked this video, please subscribe to my channel",
    "please subscribe and share this video with your friends",
    "share this video with your friends",
    "thanks for watching, and don't forget to subscribe",
    "subtitles by the amara.org community",
    "transcription by castingwords",
];

/// True when the whole segment is a video outro rather than meeting speech.
pub fn is_outro(text: &str) -> bool {
    let n = normalize(text);
    OUTROS.iter().any(|o| n == *o)
}

/// Minimum run of identical consecutive segments that collapses to its first
/// occurrence. Shorter runs are kept.
const LOOP_RUN: usize = 4;

/// Drop outros, and collapse each run of identical consecutive segments to
/// its first occurrence. Returns the number of segments removed.
pub fn filter_session(st: &mut SessionTranscript) -> usize {
    let mut dropped = 0usize;
    for chunk in st.chunks.iter_mut() {
        for track in chunk.tracks.iter_mut() {
            let before = track.segments.len();
            track.segments.retain(|s| !is_outro(&s.text));

            // Mark every member of a run of LOOP_RUN or more beyond the first.
            let mut remove = vec![false; track.segments.len()];
            let mut i = 0;
            while i < track.segments.len() {
                let key = normalize(&track.segments[i].text);
                let mut j = i + 1;
                while j < track.segments.len() && normalize(&track.segments[j].text) == key {
                    j += 1;
                }
                if !key.is_empty() && j - i >= LOOP_RUN {
                    for slot in remove.iter_mut().take(j).skip(i + 1) {
                        *slot = true;
                    }
                }
                i = j;
            }
            let keep: Vec<bool> = remove.iter().map(|r| !r).collect();
            crate::retain_by_mask(&mut track.segments, &keep);
            dropped += before - track.segments.len();
        }
    }
    dropped
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ChunkTranscript, Segment, SessionTranscript, Track, TrackTranscript};

    fn seg(start_ms: u32, text: &str) -> Segment {
        Segment {
            start_ms,
            end_ms: start_ms + 1000,
            text: text.into(),
            confidence: None,
            speaker_id: None,
        }
    }

    fn session(texts: &[&str]) -> SessionTranscript {
        SessionTranscript {
            schema_version: 1,
            session_id: "s".into(),
            provider: "t".into(),
            model: "t".into(),
            transcribed_at_unix_seconds: 0,
            chunks: vec![ChunkTranscript {
                chunk_index: 1,
                tracks: vec![TrackTranscript {
                    track: Track::System,
                    source_wav_relative: "chunks/0001/system.wav".into(),
                    segments: texts
                        .iter()
                        .enumerate()
                        .map(|(i, t)| seg(i as u32 * 30_000, t))
                        .collect(),
                }],
            }],
        }
    }

    fn texts(st: &SessionTranscript) -> Vec<String> {
        st.chunks[0].tracks[0].segments.iter().map(|s| s.text.clone()).collect()
    }

    #[test]
    fn outros_go_only_as_whole_segments() {
        assert!(is_outro("Thanks for watching!"));
        assert!(is_outro(">> Thanks for watching."));
        assert!(is_outro("Don't forget to subscribe!"));
        // The same words inside real speech stay.
        assert!(!is_outro("Thanks for watching the demo earlier, it helped."));
        assert!(!is_outro("we should subscribe to their API"));
    }

    #[test]
    fn a_decode_loop_collapses_to_one_line() {
        // The decode-loop shape: one line per 30 s window for minutes.
        let mut st = session(&[
            "Whereas this event was a big deal.",
            "The event was a big deal.",
            "The event was a big deal.",
            "The event was a big deal.",
            "The event was a big deal.",
            "The event was a big deal.",
            "Right, moving on.",
        ]);
        let dropped = filter_session(&mut st);
        assert_eq!(dropped, 4);
        assert_eq!(
            texts(&st),
            vec![
                "Whereas this event was a big deal.",
                "The event was a big deal.",
                "Right, moving on.",
            ]
        );
    }

    #[test]
    fn short_natural_repeats_are_kept() {
        let mut st = session(&["Yeah.", "Yeah.", "Yeah.", "Okay."]);
        assert_eq!(filter_session(&mut st), 0, "a run of three is speech");
        assert_eq!(texts(&st).len(), 4);
    }

    #[test]
    fn outro_and_loop_together() {
        let mut st = session(&["Thanks for watching!", "Hi.", "Hi.", "Hi.", "Hi."]);
        assert_eq!(filter_session(&mut st), 4);
        assert_eq!(texts(&st), vec!["Hi."]);
    }
}
