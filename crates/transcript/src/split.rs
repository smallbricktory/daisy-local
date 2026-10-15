// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Splits segments at speaker changes and joins the pieces back.

use crate::model::{Segment, SessionTranscript, Word};

/// A word whose midpoint is within this distance of a turn takes its label.
const NEAREST_TURN_MS: u32 = 300;
/// A run shorter than this is absorbed into a neighbouring run.
const MIN_RUN_MS: u32 = 400;
/// A run with fewer words than this is absorbed into a neighbouring run.
const MIN_RUN_WORDS: usize = 2;

/// One speaker turn: `(start_ms, end_ms, speaker_id)`. A slice of turns is
/// sorted by start and does not overlap.
pub type Turn = (u32, u32, u32);

struct Run {
    label: u32,
    words: Vec<Word>,
}

impl Run {
    fn start(&self) -> u32 {
        self.words.first().map_or(0, |w| w.start_ms)
    }
    fn len_ms(&self) -> u32 {
        self.words.last().map_or(0, |w| w.end_ms).saturating_sub(self.start())
    }
    fn short(&self) -> bool {
        self.len_ms() < MIN_RUN_MS || self.words.len() < MIN_RUN_WORDS
    }
}

/// Distance from `t` to the turn, 0 inside it.
fn dist_point(t: u32, (s, e, _): Turn) -> u32 {
    if t < s {
        s - t
    } else if t >= e {
        t - e + 1
    } else {
        0
    }
}

fn nearest_label(turns: &[Turn], dist: impl Fn(Turn) -> u32) -> Option<u32> {
    turns
        .iter()
        .map(|&t| (dist(t), t.2))
        .filter(|&(d, _)| d <= NEAREST_TURN_MS)
        .min_by_key(|&(d, _)| d)
        .map(|(_, id)| id)
}

/// The label for a segment kept whole: the speaker with the largest total
/// turn overlap with `[start_ms, end_ms]` (earliest wins a tie), else the
/// nearest turn within 300 ms, else `None`.
fn majority_label(start_ms: u32, end_ms: u32, turns: &[Turn]) -> Option<u32> {
    let mut best: Option<(u32, u32)> = None;
    let mut seen: Vec<u32> = Vec::new();
    for &(_, _, id) in turns {
        if seen.contains(&id) {
            continue;
        }
        seen.push(id);
        let total: u32 = turns
            .iter()
            .filter(|t| t.2 == id)
            .map(|&(s, e, _)| end_ms.min(e).saturating_sub(start_ms.max(s)))
            .sum();
        if total > 0 && best.is_none_or(|(b, _)| total > b) {
            best = Some((total, id));
        }
    }
    best.map(|(_, id)| id).or_else(|| {
        nearest_label(turns, |(s, e, _)| {
            if end_ms < s {
                s - end_ms
            } else {
                start_ms.saturating_sub(e)
            }
        })
    })
}

fn whole(seg: &Segment, turns: &[Turn]) -> Vec<Segment> {
    let mut out = seg.clone();
    if let Some(id) = majority_label(seg.start_ms, seg.end_ms, turns) {
        out.speaker_id = Some(id);
    }
    vec![out]
}

/// Per-word turn label: the turn containing the word's midpoint, else the
/// nearest turn within 300 ms, else the previous word's label (the first
/// word takes the next labelled word's). `None` when no word is labelled.
fn label_words(words: &[Word], turns: &[Turn]) -> Option<Vec<u32>> {
    let own: Vec<Option<u32>> = words
        .iter()
        .map(|w| {
            let mid = ((w.start_ms as u64 + w.end_ms as u64) / 2) as u32;
            nearest_label(turns, |t| dist_point(mid, t))
        })
        .collect();
    let mut prev = own.iter().flatten().next().copied()?;
    Some(
        own.iter()
            .map(|l| {
                prev = l.unwrap_or(prev);
                prev
            })
            .collect(),
    )
}

/// Absorbs short runs into a neighbour until none is left or one run
/// remains. The shortest run goes first; it joins the longer neighbour (the
/// previous one on a tie); runs that then touch with the same label merge.
fn absorb_short(mut runs: Vec<Run>) -> Vec<Run> {
    while runs.len() > 1 {
        let Some(i) = (0..runs.len())
            .filter(|&i| runs[i].short())
            .min_by_key(|&i| (runs[i].len_ms(), runs[i].words.len(), i))
        else {
            break;
        };
        let into_prev = match (i.checked_sub(1), runs.get(i + 1)) {
            (Some(p), Some(n)) => runs[p].len_ms() >= n.len_ms(),
            (Some(_), None) => true,
            (None, _) => false,
        };
        let run = runs.remove(i);
        let at = if into_prev { i - 1 } else { i };
        if into_prev {
            runs[at].words.extend(run.words);
        } else {
            let mut w = run.words;
            w.append(&mut runs[at].words);
            runs[at].words = w;
        }
        if at + 1 < runs.len() && runs[at].label == runs[at + 1].label {
            let next = runs.remove(at + 1);
            runs[at].words.extend(next.words);
        }
        if at > 0 && runs[at - 1].label == runs[at].label {
            let cur = runs.remove(at);
            runs[at - 1].words.extend(cur.words);
        }
    }
    runs
}

/// Splits `seg` at speaker changes given by `turns`.
///
/// A segment with consistent word times is cut into runs of consecutive
/// words with the same turn label (the turn holding the word's midpoint,
/// else the nearest within 300 ms, else the previous word's label). Runs
/// under 400 ms or under 2 words join a neighbouring run, so a "yeah" from
/// the other speaker stays inside the main speaker's piece. Each piece is
/// rebuilt with [`Segment::from_words`], keeps `confidence`, takes the run's
/// `speaker_id`, and carries `split_group = Some(group)`. The first piece
/// starts at the segment's start and the last ends at its end; inner edges
/// come from the words. When the words yield a single run, or the segment has
/// no (or inconsistent) words, the result is the segment alone with
/// `speaker_id` set to the speaker with the largest turn overlap, else the
/// nearest turn within 300 ms, else left unchanged; `split_group` stays as it
/// was.
///
/// `group` is chosen by the caller. One counter per session transcript,
/// advanced for every split, keeps ids unique across chunks and tracks.
pub fn split_by_turns(seg: &Segment, turns: &[Turn], group: u32) -> Vec<Segment> {
    if seg.words.is_empty() || !seg.words_consistent() || turns.is_empty() {
        return whole(seg, turns);
    }
    let Some(labels) = label_words(&seg.words, turns) else {
        return whole(seg, turns);
    };
    let mut runs: Vec<Run> = Vec::new();
    for (w, &label) in seg.words.iter().zip(&labels) {
        match runs.last_mut() {
            Some(r) if r.label == label => r.words.push(w.clone()),
            _ => runs.push(Run { label, words: vec![w.clone()] }),
        }
    }
    let runs = absorb_short(runs);
    if runs.len() == 1 {
        let mut out = seg.clone();
        out.speaker_id = Some(runs[0].label);
        return vec![out];
    }
    let last = runs.len() - 1;
    runs.into_iter()
        .enumerate()
        .filter_map(|(i, r)| {
            let mut piece = Segment::from_words(
                r.words.iter().map(|w| (w.text.clone(), w.start_ms as i64, w.end_ms as i64)),
            )?;
            piece.set_span(
                if i == 0 { seg.start_ms } else { piece.start_ms },
                if i == last { seg.end_ms } else { piece.end_ms },
            );
            piece.confidence = seg.confidence;
            piece.speaker_id = Some(r.label);
            piece.split_group = Some(group);
            Some(piece)
        })
        .collect()
}

/// Merges adjacent segments that share a `split_group` back into one
/// segment: words concatenated, span from the first piece's start to the last
/// piece's end, `confidence` of the first piece, `split_group` cleared, and
/// `speaker_id` of the longest piece by span (the earliest on a tie).
/// Segments without a group pass through. After [`split_by_turns`] this
/// restores the source segment.
pub fn rejoin_split_groups(segs: Vec<Segment>) -> Vec<Segment> {
    let mut out: Vec<Segment> = Vec::with_capacity(segs.len());
    let mut i = 0;
    while i < segs.len() {
        let Some(g) = segs[i].split_group else {
            out.push(segs[i].clone());
            i += 1;
            continue;
        };
        let end = i + segs[i..].iter().take_while(|s| s.split_group == Some(g)).count();
        let pieces = &segs[i..end];
        i = end;
        if pieces.iter().any(|p| p.words.is_empty() || !p.words_consistent()) {
            out.extend(pieces.iter().cloned().map(|mut p| {
                p.split_group = None;
                p
            }));
            continue;
        }
        let longest = pieces
            .iter()
            .enumerate()
            .max_by_key(|&(k, p)| (p.end_ms - p.start_ms, std::cmp::Reverse(k)))
            .map(|(_, p)| p)
            .unwrap();
        let words = pieces
            .iter()
            .flat_map(|p| &p.words)
            .map(|w| (w.text.clone(), w.start_ms as i64, w.end_ms as i64));
        let Some(mut seg) = Segment::from_words(words) else { continue };
        seg.set_span(pieces[0].start_ms, pieces[pieces.len() - 1].end_ms);
        seg.confidence = pieces[0].confidence;
        seg.speaker_id = longest.speaker_id;
        out.push(seg);
    }
    out
}

/// Makes every `split_group` in `st` one [`rejoin_split_groups`] can restore,
/// numbered densely from 0 in transcript order. A group is kept when its
/// pieces form one adjacent run on one track and every piece carries words
/// consistent with its text and span; any other group is cleared from all
/// its pieces. Returns true when a segment changed.
pub fn normalize_split_groups(st: &mut SessionTranscript) -> bool {
    use std::collections::HashMap;
    // Per group value: the runs of adjacent pieces it forms, and whether
    // every piece carries consistent words.
    let mut seen: HashMap<u32, (usize, bool)> = HashMap::new();
    for tr in st.chunks.iter().flat_map(|c| &c.tracks) {
        let mut prev = None;
        for seg in &tr.segments {
            if let Some(g) = seg.split_group {
                let e = seen.entry(g).or_insert((0, true));
                if prev != Some(g) {
                    e.0 += 1;
                }
                e.1 &= !seg.words.is_empty() && seg.words_consistent();
            }
            prev = seg.split_group;
        }
    }
    let mut renumbered: HashMap<u32, u32> = HashMap::new();
    let mut changed = false;
    for seg in st.chunks.iter_mut().flat_map(|c| &mut c.tracks).flat_map(|t| &mut t.segments) {
        let Some(g) = seg.split_group else { continue };
        let next = renumbered.len() as u32;
        let fresh = match seen[&g] {
            (1, true) => Some(*renumbered.entry(g).or_insert(next)),
            _ => None,
        };
        if fresh != seg.split_group {
            seg.split_group = fresh;
            changed = true;
        }
    }
    changed
}
