//! Render a SessionTranscript as a unified Me/Them markdown timeline.

use crate::model::{SessionTranscript, Track};
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
            if matches!(tr.track, Track::Mic | Track::MicAec) {
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
            if matches!(tr.track, Track::Mic | Track::MicAec) && !mic_multi {
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

    for chunk in &session.chunks {
        let off = chunk_offset_ms.get(&chunk.chunk_index).copied().unwrap_or(0);
        let _ = writeln!(out, "## Chunk {}", chunk.chunk_index);
        let _ = writeln!(out);

        // Rows carry an owned label string; each "Them" resolves to its own
        // per-speaker name.
        let mut rows: Vec<(u32, String, String)> = Vec::new();
        for tr in &chunk.tracks {
            let track_kind = tr.track;
            for seg in &tr.segments {
                let cleaned = crate::backchannel::strip_leading_filler(&seg.text);
                if cleaned.is_empty() {
                    continue;
                }
                let cleaned = crate::backchannel::strip_mid_filler(&cleaned);
                if cleaned.is_empty() {
                    continue;
                }
                let label = match track_kind {
                    Track::MicAec | Track::Mic => label_for_me(seg.speaker_id),
                    Track::System => label_for_them(seg.speaker_id),
                };
                rows.push((seg.start_ms.saturating_add(off), label, cleaned));
            }
        }
        rows.sort_by_key(|(start, _, _)| *start);

        // Every paragraph gets its own `[ts] **Speaker**:` header. A paragraph
        // breaks at the speaker change, at MAX_PARA_CHARS, or after a
        // same-speaker pause of PARA_BREAK_MS. Adjacent duplicate texts within
        // a paragraph collapse.
        const MAX_PARA_CHARS: usize = 600;
        const PARA_BREAK_MS: u32 = 20_000;
        let mut i = 0;
        while i < rows.len() {
            let label = rows[i].1.clone();
            let mut paragraphs: Vec<(u32, String)> = Vec::new();
            let mut para_start = rows[i].0;
            let mut para = String::new();
            let mut last_text: Option<String> = None;
            let mut last_seg_start = rows[i].0;
            while i < rows.len() && rows[i].1 == label {
                let seg_start = rows[i].0;
                let exceeds_chars = para.chars().count() >= MAX_PARA_CHARS;
                let gap_too_big = seg_start.saturating_sub(last_seg_start) >= PARA_BREAK_MS;
                if !para.is_empty() && (exceeds_chars || gap_too_big) {
                    paragraphs.push((para_start, std::mem::take(&mut para)));
                    para_start = seg_start;
                }
                let text = rows[i].2.clone();
                if last_text.as_deref() != Some(text.as_str()) {
                    if !para.is_empty() {
                        para.push(' ');
                    }
                    para.push_str(&text);
                    last_text = Some(text);
                }
                last_seg_start = seg_start;
                i += 1;
            }
            if !para.is_empty() {
                paragraphs.push((para_start, para));
            }
            for (idx, (start, text)) in paragraphs.iter().enumerate() {
                if idx > 0 {
                    let _ = writeln!(out);
                }
                let _ = writeln!(out, "[{}] **{}**: {}", format_hms(*start), label, text);
            }
        }
        let _ = writeln!(out);
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
