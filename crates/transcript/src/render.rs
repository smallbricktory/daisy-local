// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Render a SessionTranscript as a unified Me/Them markdown timeline.

use crate::model::{SessionTranscript, Track};
use crate::reading;
use std::collections::HashMap;
use std::fmt::Write;

/// Render with all timestamps chunk-relative (each chunk restarts at 00:00:00).
pub fn render_markdown(session: &SessionTranscript) -> String {
    render_markdown_with_offsets(session, &HashMap::new())
}

/// Render the transcript as markdown. `chunk_offset_ms` maps a chunk_index to
/// milliseconds added to every timestamp in that chunk (each chunk's start
/// offset relative to the meeting start).
pub fn render_markdown_with_offsets(
    session: &SessionTranscript,
    chunk_offset_ms: &HashMap<u32, u32>,
) -> String {
    render_markdown_with_speakers(session, chunk_offset_ms, &HashMap::new())
}

/// Same as `render_markdown_with_offsets` but also accepts a `speaker_map`
/// that turns a diarized cluster id into a display name. When a segment has a
/// `speaker_id` but no entry in the map, it falls back to "Person A",
/// "Person B", etc., assigned by order of first appearance within the
/// session.
pub fn render_markdown_with_speakers(
    session: &SessionTranscript,
    chunk_offset_ms: &HashMap<u32, u32>,
    speaker_map: &HashMap<u32, String>,
) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "# Transcript: {}", session.session_id);
    let _ = writeln!(out, "_Provider: {} ({})_", session.provider, session.model);
    let _ = writeln!(out);

    // Distinct diarized clusters on the mic (room) track. A single unnamed
    // room cluster keeps the plain "Me" label; two or more get Person letters
    // so in-person speakers stay distinguishable.
    let mut mic_sids: std::collections::HashSet<u32> = std::collections::HashSet::new();
    for chunk in &session.chunks {
        for tr in &chunk.tracks {
            if tr.track.is_mic() {
                mic_sids.extend(tr.segments.iter().filter_map(|s| s.speaker_id));
            }
        }
    }
    let mic_multi = mic_sids.len() >= 2;

    // Build the cluster-id → display name lookup once per render. Clusters
    // without a user-supplied label get "Person A/B/C…" in order-of-first-
    // appearance across the whole session (not per chunk).
    let mut auto_labels: HashMap<u32, String> = HashMap::new();
    let mut next_letter: u8 = b'A';
    for chunk in &session.chunks {
        for tr in &chunk.tracks {
            if tr.track.is_mic() && !mic_multi {
                continue;
            }
            for seg in &tr.segments {
                if let Some(sid) = seg.speaker_id {
                    if speaker_map.contains_key(&sid) {
                        continue;
                    }
                    auto_labels.entry(sid).or_insert_with(|| {
                        let l = format!("Person {}", next_letter as char);
                        if next_letter < b'Z' {
                            next_letter += 1;
                        }
                        l
                    });
                }
            }
        }
    }
    let label_for_them = |sid: Option<u32>| -> String {
        match sid {
            Some(s) => speaker_map
                .get(&s)
                .cloned()
                .or_else(|| auto_labels.get(&s).cloned())
                .unwrap_or_else(|| "Them".to_string()),
            None => "Them".to_string(),
        }
    };
    // Mic segments: a named or lettered cluster shows that label; otherwise
    // the plain "Me".
    let label_for_me = |sid: Option<u32>| -> String {
        sid.and_then(|s| speaker_map.get(&s).cloned().or_else(|| auto_labels.get(&s).cloned()))
            .unwrap_or_else(|| "Me".to_string())
    };

    // Every segment on the session timeline, fillers stripped. Segments merge
    // into turns within their own track by `reading::joins`, in time order,
    // across chunk boundaries.
    struct Row {
        start: u32,
        end: u32,
        chunk: usize,
        track: Track,
        speaker: Option<u32>,
        text: String,
    }
    let mut rows: Vec<Row> = Vec::new();
    for (c, chunk) in session.chunks.iter().enumerate() {
        let off = chunk_offset_ms.get(&chunk.chunk_index).copied().unwrap_or(0);
        for tr in &chunk.tracks {
            for seg in &tr.segments {
                let cleaned = crate::backchannel::strip_leading_filler(&seg.text);
                if cleaned.is_empty() {
                    continue;
                }
                let cleaned = crate::backchannel::strip_mid_filler(&cleaned);
                if cleaned.is_empty() {
                    continue;
                }
                rows.push(Row {
                    start: seg.start_ms.saturating_add(off),
                    end: seg.end_ms.saturating_add(off),
                    chunk: c,
                    track: tr.track,
                    speaker: seg.speaker_id,
                    text: cleaned,
                });
            }
        }
    }
    // With every chunk placed on the session timeline, turns run across
    // chunk boundaries. A chunk without an offset restarts at 00:00:00, so
    // then each chunk groups on its own. Stable: equal keys keep chunk,
    // track and segment order.
    let timeline = session.chunks.iter().all(|c| chunk_offset_ms.contains_key(&c.chunk_index));
    if timeline {
        rows.sort_by_key(|r| r.start);
    } else {
        rows.sort_by_key(|r| (r.chunk, r.start));
    }

    // A turn: its first row's start and chunk, the latest end, the first
    // speaker any of its rows holds, and its last row's text, which
    // repeats collapse against.
    struct Turn {
        start: u32,
        end: u32,
        chunk: usize,
        track: Track,
        speaker: Option<u32>,
        text: String,
        last: String,
    }
    let mut turns: Vec<Turn> = Vec::new();
    let mut open: Vec<(Track, usize)> = Vec::new();
    for row in rows {
        if !timeline && turns.last().is_some_and(|t| t.chunk != row.chunk) {
            open.clear();
        }
        let at = open.iter().find(|(t, _)| *t == row.track).map(|&(_, i)| i);
        let reads_on = at.is_some_and(|i| {
            let t = &turns[i];
            reading::reads_on(row.start, row.speaker, &t.text, t.end, t.speaker)
        });
        match at {
            Some(i) if reads_on && !reading::full(&turns[i].text) => {
                let t = &mut turns[i];
                t.end = t.end.max(row.end);
                t.speaker = t.speaker.or(row.speaker);
                if t.last != row.text {
                    t.text = reading::join(&t.text, &row.text);
                    t.last = row.text;
                }
            }
            _ => {
                // A full turn that would have read on hands its label to the next.
                let speaker = match at {
                    Some(i) if reads_on => row.speaker.or(turns[i].speaker),
                    _ => row.speaker,
                };
                open.retain(|(t, _)| *t != row.track);
                open.push((row.track, turns.len()));
                turns.push(Turn {
                    start: row.start,
                    end: row.end,
                    chunk: row.chunk,
                    track: row.track,
                    speaker,
                    last: row.text.clone(),
                    text: row.text,
                });
            }
        }
    }
    // Opened in row order, so already in time order.

    // Every turn gets its own `[ts] **Speaker**:` line under the header of
    // the chunk it starts in. A blank line separates consecutive turns of
    // one label.
    let mut next_header = 0;
    let mut last_label: Option<String> = None;
    let header = |out: &mut String, c: usize| {
        let _ = writeln!(out, "## Chunk {}", session.chunks[c].chunk_index);
        let _ = writeln!(out);
    };
    for t in &turns {
        if t.chunk >= next_header {
            if next_header > 0 {
                let _ = writeln!(out);
            }
            while next_header <= t.chunk {
                if next_header < t.chunk {
                    header(&mut out, next_header);
                    let _ = writeln!(out);
                } else {
                    header(&mut out, next_header);
                }
                next_header += 1;
            }
            last_label = None;
        }
        let label = if t.track.is_mic() { label_for_me(t.speaker) } else { label_for_them(t.speaker) };
        if last_label.as_deref() == Some(label.as_str()) {
            let _ = writeln!(out);
        }
        let _ = writeln!(out, "[{}] **{}**: {}", format_hms(t.start), label, t.text);
        last_label = Some(label);
    }
    if next_header > 0 {
        let _ = writeln!(out);
    }
    while next_header < session.chunks.len() {
        header(&mut out, next_header);
        let _ = writeln!(out);
        next_header += 1;
    }

    out
}

fn format_hms(ms: u32) -> String {
    let total_secs = ms / 1000;
    let h = total_secs / 3600;
    let m = (total_secs % 3600) / 60;
    let s = total_secs % 60;
    format!("{:02}:{:02}:{:02}", h, m, s)
}
