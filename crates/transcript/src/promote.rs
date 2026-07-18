//! Promote a complete live transcript into the canonical `SessionTranscript`,
//! replacing the finalize whisper full-pass when live coverage spans the
//! whole recording. Pure functions: the caller adapts the recording /
//! live-transcript types into these primitive inputs.

use crate::echo_direction::EchoDirection;
use crate::model::{ChunkTranscript, Segment, SessionTranscript, Track, TrackTranscript};
use crate::text::normalize;
use std::collections::HashSet;
use std::path::PathBuf;

/// Speaker-bleed filter params. Similarity is bigram containment of the
/// echo-candidate side in the other (|∩| / candidate's bigrams) within a
/// wide time window. The no-audio path
/// (`filter_mic_bleed`) is asymmetric — the system/remote track is never
/// dropped; the finalize path (`filter_bleed_directional`) may drop the
/// system copy instead on acoustic proof.
///
/// MUST stay in sync with the live-display twin in
/// `apps/frontend/src/liveTranscript.ts` (`isMicBleedOfSystem`) — same
/// overlap/window/min-words. A threshold change here requires the same
/// change there. The twin mirrors the no-audio path only: the browser has no
/// track audio, so direction arbitration is finalize-only.
const BLEED_OVERLAP: f32 = 0.6;
const BLEED_WINDOW_MS: u32 = 8_000;
const BLEED_MIN_WORDS: usize = 3;

type Bigrams = HashSet<(String, String)>;

fn bigrams(text: &str) -> Bigrams {
    let norm = normalize(text);
    let words: Vec<&str> = norm.split_whitespace().collect();
    words
        .windows(2)
        .map(|w| (w[0].to_string(), w[1].to_string()))
        .collect()
}

/// Fraction of `frag`'s bigrams present in `hay` — "frag's content is
/// contained in hay". Directional on purpose: dividing by the smaller set
/// (overlap coefficient) let a 3-word stub on one track delete a full
/// sentence on the other because the stub's couple of bigrams matched.
fn bigram_containment(frag: &Bigrams, hay: &Bigrams) -> f32 {
    if frag.is_empty() || hay.is_empty() {
        return 0.0;
    }
    frag.intersection(hay).count() as f32 / frag.len() as f32
}

/// Short fragments (below the bigram floor) still count as bleed when their
/// words appear, in order, inside a nearby system segment: "Share with"
/// against "all that we can share with suppliers". Echo ASR mangles the odd
/// word ("supplier"/"suppliers"), so words compare by 4-char prefix and
/// fragments of 5+ words may miss one. 1-2 word fragments must match
/// contiguously and exactly — a free-standing "Yes." only dies when the
/// remote side actually said it in the window.
const BLEED_SUBSTRING_MAX_WORDS: usize = 6;
/// Echo trails its source: a mic segment may start slightly before its
/// system twin (decode jitter) but not by much. The trailing side keeps
/// BLEED_WINDOW_MS.
const BLEED_LEAD_MS: u32 = 2_000;

fn in_echo_window(mic_start: u32, sys_start: u32) -> bool {
    if mic_start >= sys_start {
        mic_start - sys_start <= BLEED_WINDOW_MS
    } else {
        sys_start - mic_start <= BLEED_LEAD_MS
    }
}

/// Prefix-equality (first 4 chars) — tolerates plural/tense mangling.
/// normalize() output is ascii, so byte slicing is safe.
fn word_eq(a: &str, b: &str) -> bool {
    let pa = &a[..a.len().min(4)];
    let pb = &b[..b.len().min(4)];
    !pa.is_empty() && pa == pb
}

/// Ordered, gap-tolerant containment of `frag` in `hay`; up to `misses`
/// fragment words may be absent.
fn subseq_contained(frag: &[&str], hay: &[&str], misses: usize) -> bool {
    let mut hi = 0;
    let mut missed = 0;
    for w in frag {
        let mut found = false;
        while hi < hay.len() {
            let ok = word_eq(w, hay[hi]);
            hi += 1;
            if ok {
                found = true;
                break;
            }
        }
        if !found {
            missed += 1;
            if missed > misses {
                return false;
            }
        }
    }
    true
}

/// Order-free containment for longer mic turns: ASR rewords echoes ("That.
/// That is the time…" vs "That, and that will be the time…"), defeating both
/// ordered subsequence and bigram overlap. If ≥75% of a 5+-word mic turn's
/// words (prefix-matched, multiset) appear in one nearby system turn, it is
/// echo regardless of order.
const BLEED_CONTAIN_MIN_WORDS: usize = 5;
const BLEED_CONTAIN_RATIO: f32 = 0.75;

fn contained_echo_match(mic: &LiveSeg, system: &[&LiveSeg]) -> Option<usize> {
    let m = normalize(&mic.text);
    let frag: Vec<&str> = m.split_whitespace().collect();
    if frag.len() < BLEED_CONTAIN_MIN_WORDS {
        return None;
    }
    system.iter().position(|s| {
        if !in_echo_window(mic.start_ms, s.start_ms) {
            return false;
        }
        let h = normalize(&s.text);
        let mut hay: Vec<&str> = h.split_whitespace().collect();
        let mut matched = 0usize;
        for w in &frag {
            if let Some(i) = hay.iter().position(|x| word_eq(w, x)) {
                hay.swap_remove(i);
                matched += 1;
            }
        }
        matched as f32 / frag.len() as f32 >= BLEED_CONTAIN_RATIO
    })
}

fn substring_echo_match(mic: &LiveSeg, system: &[&LiveSeg]) -> Option<usize> {
    let m = normalize(&mic.text);
    let frag: Vec<&str> = m.split_whitespace().collect();
    if frag.is_empty() || frag.len() > BLEED_SUBSTRING_MAX_WORDS {
        return None;
    }
    system.iter().position(|s| {
        if !in_echo_window(mic.start_ms, s.start_ms) {
            return false;
        }
        let h = normalize(&s.text);
        let hay: Vec<&str> = h.split_whitespace().collect();
        if frag.len() <= 2 {
            hay.windows(frag.len()).any(|w| w.iter().zip(&frag).all(|(a, b)| a == b))
        } else {
            let misses = usize::from(frag.len() >= 5);
            subseq_contained(&frag, &hay, misses)
        }
    })
}

/// The one forward matcher: the system segment `mic` is an echo of, if any.
/// Four clauses, each validated against a distinct measured failure mode —
/// they do NOT collapse into one similarity score (tried; every merge either
/// leaked verified echo or deleted verified real speech):
/// - verbatim word-substring of one segment (short fragments),
/// - order-free word containment in one segment (reworded echo),
/// - bigram containment in one segment (scattered partial echo),
/// - ordered subsequence across the pooled window (decoder fragmentation).
fn echo_match(mic: &LiveSeg, system: &[&LiveSeg]) -> Option<usize> {
    if let Some(i) = substring_echo_match(mic, system).or_else(|| contained_echo_match(mic, system))
    {
        return Some(i);
    }
    if normalize(&mic.text).split_whitespace().count() >= BLEED_MIN_WORDS {
        let mb = bigrams(&mic.text);
        if !mb.is_empty() {
            if let Some(i) = system.iter().position(|s| {
                in_echo_window(mic.start_ms, s.start_ms)
                    && bigram_containment(&mb, &bigrams(&s.text)) >= BLEED_OVERLAP
            }) {
                return Some(i);
            }
        }
    }
    pooled_echo_match(mic, system)
}

/// Drop the echo copy of each cross-track near-duplicate pair. `direction`
/// arbitrates acoustically (see `echo_direction`).
///
/// Two passes with different burdens of proof:
/// - Legacy window (mic trails, or leads ≤ `BLEED_LEAD_MS`): the mic copy is
///   dropped unless `direction` positively says `SystemIsEcho`.
/// - Reverse window (mic leads by more — a round-trip echo of the local
///   voice): the pair was never eligible before, so nothing is dropped
///   unless `direction` positively says `SystemIsEcho`; then the system
///   copy dies and the mic original stays.
pub fn filter_bleed_directional(
    segs: &[LiveSeg],
    direction: &dyn Fn(&LiveSeg, &LiveSeg) -> EchoDirection,
) -> Vec<LiveSeg> {
    // Original indices of system entries; `echo_match` positions index into
    // `system`.
    let sys_idx: Vec<usize> = (0..segs.len()).filter(|&i| segs[i].is_system).collect();
    let system: Vec<&LiveSeg> = sys_idx.iter().map(|&i| &segs[i]).collect();
    let mut dropped: Vec<bool> = vec![false; segs.len()];
    for (i, seg) in segs.iter().enumerate() {
        if seg.is_system {
            continue;
        }
        if let Some(si) = echo_match(seg, &system) {
            // Flipping the drop onto the system copy requires BOTH acoustic
            // proof and the reverse-echo content signature: the system copy
            // adds (almost) no words over the kept mic original. Without
            // that, a cross-correlation misled by double-talk could delete a
            // system segment carrying the far end's own speech.
            if direction(seg, system[si]) == EchoDirection::SystemIsEcho
                && words_contained_ratio(&system[si].text, &seg.text) >= BLEED_CONTAIN_RATIO
            {
                dropped[sys_idx[si]] = true;
            } else {
                dropped[i] = true;
            }
        } else if let Some(si) = reverse_echo_match(seg, &system) {
            if direction(seg, system[si]) == EchoDirection::SystemIsEcho {
                dropped[sys_idx[si]] = true;
            }
        }
    }
    segs.iter()
        .enumerate()
        .filter(|(i, _)| !dropped[*i])
        .map(|(_, s)| s.clone())
        .collect()
}

/// Minimum words for the pooled rule — short fragments against a large pool
/// would false-positive on function words alone.
const BLEED_POOL_MIN_WORDS: usize = 4;

/// Mic fragment appears, in order, inside the concatenated (time-ordered)
/// text of the system segments in its echo window. Catches echo that the
/// per-segment rules miss because the two decoders fragment the same speech
/// at different boundaries. Returns the pool member nearest in time (for the
/// direction oracle's span).
fn pooled_echo_match(mic: &LiveSeg, system: &[&LiveSeg]) -> Option<usize> {
    let m = normalize(&mic.text);
    let frag: Vec<&str> = m.split_whitespace().collect();
    if frag.len() < BLEED_POOL_MIN_WORDS {
        return None;
    }
    let mut members: Vec<usize> = (0..system.len())
        .filter(|&i| in_echo_window(mic.start_ms, system[i].start_ms))
        .collect();
    if members.len() < 2 {
        return None; // a single member is the per-segment rules' job
    }
    members.sort_by_key(|&i| system[i].start_ms);
    let pooled: Vec<String> = members
        .iter()
        .map(|&i| normalize(&system[i].text))
        .collect();
    let pool: Vec<&str> = pooled.iter().flat_map(|t| t.split_whitespace()).collect();
    let misses = frag.len() / 6;
    if !subseq_contained(&frag, &pool, misses) {
        return None;
    }
    members
        .into_iter()
        .min_by_key(|&i| (system[i].start_ms as i64 - mic.start_ms as i64).abs())
}

/// Promotion gate inputs: a mic final counts as bled when ≥75% of its words
/// appear (order-free) in the ±20 s pooled system text.
pub const PROMOTE_BLEED_WINDOW_MS: u32 = 20_000;
pub const PROMOTE_BLEED_CONTAIN: f32 = 0.75;
/// At or above this bled-fraction, finalize skips promotion and runs the
/// whisper full pass on the AEC track.
pub const PROMOTE_BLEED_MAX_RATE: f32 = 0.15;

/// Fraction of eligible mic finals that are order-free contained
/// (≥ [`PROMOTE_BLEED_CONTAIN`]) in their ±[`PROMOTE_BLEED_WINDOW_MS`]
/// pooled system text.
pub fn promotion_bleed_rate(segs: &[LiveSeg]) -> f32 {
    let mut eligible = 0usize;
    let mut bled = 0usize;
    for s in segs {
        if let Some(r) = words_contained_ratio_pooled(s, segs, PROMOTE_BLEED_WINDOW_MS) {
            eligible += 1;
            if r >= PROMOTE_BLEED_CONTAIN {
                bled += 1;
            }
        }
    }
    if eligible == 0 {
        0.0
    } else {
        bled as f32 / eligible as f32
    }
}

/// Order-free containment of a mic final's words in the pooled system text
/// within ±`window_ms` of its start. `None` when the segment is a system
/// final or too short to judge (< `BLEED_POOL_MIN_WORDS` words).
pub fn words_contained_ratio_pooled(
    mic: &LiveSeg,
    segs: &[LiveSeg],
    window_ms: u32,
) -> Option<f32> {
    if mic.is_system {
        return None;
    }
    if normalize(&mic.text).split_whitespace().count() < BLEED_POOL_MIN_WORDS {
        return None;
    }
    let lo = mic.start_ms.saturating_sub(window_ms);
    let hi = mic.start_ms.saturating_add(window_ms);
    let pooled = segs
        .iter()
        .filter(|s| s.is_system && s.start_ms >= lo && s.start_ms <= hi)
        .map(|s| s.text.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    if pooled.is_empty() {
        return Some(0.0);
    }
    Some(words_contained_ratio(&mic.text, &pooled))
}

/// Fraction of `needle`'s words present in `hay` (order-free multiset,
/// prefix-matched). 0.0 when `needle` has no words.
fn words_contained_ratio(needle: &str, hay: &str) -> f32 {
    let n = normalize(needle);
    let frag: Vec<&str> = n.split_whitespace().collect();
    if frag.is_empty() {
        return 0.0;
    }
    let h = normalize(hay);
    let mut pool: Vec<&str> = h.split_whitespace().collect();
    let mut matched = 0usize;
    for w in &frag {
        if let Some(i) = pool.iter().position(|x| word_eq(w, x)) {
            pool.swap_remove(i);
            matched += 1;
        }
    }
    matched as f32 / frag.len() as f32
}

/// A system segment duplicating `mic` from far behind it — the round-trip
/// echo region the legacy window excludes: system trails the mic original by
/// (`BLEED_LEAD_MS`, `BLEED_WINDOW_MS`]. Text matching mirrors `bleed_match`
/// with the roles of the time bounds swapped.
fn reverse_echo_match(mic: &LiveSeg, system: &[&LiveSeg]) -> Option<usize> {
    let m = normalize(&mic.text);
    let words: Vec<&str> = m.split_whitespace().collect();
    if words.len() < BLEED_MIN_WORDS {
        return None;
    }
    let mb = bigrams(&mic.text);
    system.iter().position(|s| {
        let trails = s.start_ms > mic.start_ms
            && s.start_ms - mic.start_ms > BLEED_LEAD_MS
            && s.start_ms - mic.start_ms <= BLEED_WINDOW_MS;
        if !trails {
            return false;
        }
        if !mb.is_empty() && bigram_containment(&bigrams(&s.text), &mb) >= BLEED_OVERLAP {
            return true;
        }
        // Reworded echo: most of the system copy's words appear in the mic
        // original (order-free), mirroring `contained_echo_match`.
        let h = normalize(&s.text);
        if h.split_whitespace().count() < BLEED_CONTAIN_MIN_WORDS {
            return false;
        }
        words_contained_ratio(&s.text, &mic.text) >= BLEED_CONTAIN_RATIO
    })
}

/// Drop mic live segments that are speaker-bleed of a nearby system segment.
/// System segments are always kept. Pure; exported for tests. This is the
/// no-audio path — the live-display TS twin mirrors exactly this behavior.
pub fn filter_mic_bleed(segs: &[LiveSeg]) -> Vec<LiveSeg> {
    filter_bleed_directional(segs, &|_, _| EchoDirection::Unknown)
}

/// Fraction of pool-eligible mic finals (≥ `BLEED_POOL_MIN_WORDS` words)
/// that are pooled echoes of nearby system text. Near-zero on a healthy
/// session; wholesale speaker bleed (mic hearing the speakers all meeting)
/// pushes it toward 0.5. Finalize gates live-promotion on it: past the
/// threshold, the whisper full pass on the AEC track beats promoting a
/// zippered live transcript.
pub fn pooled_duplication_rate(segs: &[LiveSeg]) -> f32 {
    let system: Vec<&LiveSeg> = segs.iter().filter(|s| s.is_system).collect();
    let mut eligible = 0usize;
    let mut dupes = 0usize;
    for seg in segs {
        if seg.is_system {
            continue;
        }
        if normalize(&seg.text).split_whitespace().count() < BLEED_POOL_MIN_WORDS {
            continue;
        }
        eligible += 1;
        if pooled_echo_match(seg, &system).is_some() {
            dupes += 1;
        }
    }
    if eligible == 0 {
        return 0.0;
    }
    dupes as f32 / eligible as f32
}

/// Default max gap (ms) between consecutive covered spans before the live
/// transcript is treated as incomplete.
pub const GAP_TOLERANCE_MS: u32 = 10_000;

/// One final live segment with session-relative timestamps, track-tagged.
#[derive(Debug, Clone)]
pub struct LiveSeg {
    /// True = system (loopback / far end), false = mic (local).
    pub is_system: bool,
    pub start_ms: u32,
    pub end_ms: u32,
    pub text: String,
}

/// A chunk's session-relative time span + its track wav paths, from the
/// manifest. `start_ms` is the chunk's offset from session start; segments are
/// rebased onto it (each chunk WAV starts at 0).
#[derive(Debug, Clone)]
pub struct ChunkSpan {
    pub index: u32,
    pub start_ms: u32,
    /// Mic track kind: `MicAec` when the chunk has an AEC wav, else `Mic`.
    pub mic_track: Track,
    pub mic_wav: PathBuf,
    pub system_wav: PathBuf,
}

/// True when speech coverage (the union of both tracks) spans `total_ms` with
/// no gap larger than `gap_tolerance_ms`, and the tail reaches within
/// tolerance of the end.
pub fn coverage_is_lossless(spans: &[(u32, u32)], total_ms: u32, gap_tolerance_ms: u32) -> bool {
    if total_ms == 0 || spans.is_empty() {
        return false;
    }
    let mut spans: Vec<(u32, u32)> = spans.to_vec();
    spans.sort_by_key(|s| s.0);
    // Leading gap, allowed up to tolerance.
    if spans[0].0 > gap_tolerance_ms {
        return false;
    }
    let mut cursor = spans[0].1;
    for &(s, e) in &spans[1..] {
        if s > cursor.saturating_add(gap_tolerance_ms) {
            return false;
        }
        cursor = cursor.max(e);
    }
    // Tail gap, allowed up to tolerance.
    total_ms.saturating_sub(cursor) <= gap_tolerance_ms
}

/// Build a `SessionTranscript` from final live segments + chunk spans. Each
/// segment is assigned to the last chunk whose `start_ms <= seg.start_ms`
/// (trailing segments land in the final chunk) and rebased to chunk-relative
/// time. Every chunk gets a mic + system `TrackTranscript`, matching the
/// shape of an orchestrator-produced transcript. `speaker_id` is left
/// `None`; diarization runs locally at finalize.
pub fn promote_live_to_transcript(
    segs: &[LiveSeg],
    chunks: &[ChunkSpan],
    session_id: &str,
    backend: &str,
    transcribed_at_unix_seconds: i64,
    echo_direction: &dyn Fn(&LiveSeg, &LiveSeg) -> EchoDirection,
) -> SessionTranscript {
    // Drop the echo copy of cross-track duplicates before splitting into
    // chunks; `echo_direction` (WAV-backed at finalize) picks the side.
    let segs = filter_bleed_directional(segs, echo_direction);

    // Per-chunk accumulators, parallel to `chunks`.
    let mut mic: Vec<Vec<Segment>> = vec![Vec::new(); chunks.len()];
    let mut sys: Vec<Vec<Segment>> = vec![Vec::new(); chunks.len()];

    for s in &segs {
        // Last chunk that starts at or before this segment.
        let Some(ci) = chunks.iter().rposition(|ch| ch.start_ms <= s.start_ms) else {
            continue; // before the first chunk: dropped.
        };
        let base = chunks[ci].start_ms;
        let rel_start = s.start_ms - base;
        let rel_end = s.end_ms.saturating_sub(base).max(rel_start);
        let seg = Segment {
            start_ms: rel_start,
            end_ms: rel_end,
            text: s.text.clone(),
            confidence: None,
            speaker_id: None,
        };
        if s.is_system {
            sys[ci].push(seg);
        } else {
            mic[ci].push(seg);
        }
    }

    let chunk_transcripts = chunks
        .iter()
        .enumerate()
        .map(|(i, ch)| ChunkTranscript {
            chunk_index: ch.index,
            tracks: vec![
                TrackTranscript {
                    track: ch.mic_track,
                    source_wav_relative: ch.mic_wav.clone(),
                    segments: std::mem::take(&mut mic[i]),
                },
                TrackTranscript {
                    track: Track::System,
                    source_wav_relative: ch.system_wav.clone(),
                    segments: std::mem::take(&mut sys[i]),
                },
            ],
        })
        .collect();

    SessionTranscript {
        schema_version: SessionTranscript::SCHEMA,
        session_id: session_id.to_string(),
        provider: backend.to_string(),
        model: backend.to_string(),
        transcribed_at_unix_seconds,
        chunks: chunk_transcripts,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spans(v: &[(u32, u32)]) -> Vec<(u32, u32)> {
        v.to_vec()
    }

    fn seg(is_system: bool, start_ms: u32, text: &str) -> LiveSeg {
        LiveSeg { is_system, start_ms, end_ms: start_ms + 3000, text: text.into() }
    }

    #[test]
    fn promotion_bleed_rate_flags_shuffled_garbled_duplicates() {
        // Mic finals re-decode the system speech out of order with mangled
        // words.
        let segs = vec![
            seg(true, 10_000, "the quarterly northwind forecast needs another full revision pass"),
            seg(true, 16_000, "borealis freight signs the renewal after the security review closes"),
            seg(false, 13_000, "needs quarterly the forecastt northwind revision another pass full"),
            seg(false, 19_500, "signs borealis the renewal freight after security the review closes"),
        ];
        let rate = promotion_bleed_rate(&segs);
        assert!(rate >= PROMOTE_BLEED_MAX_RATE, "rate={rate}");
    }

    #[test]
    fn promotion_bleed_rate_near_zero_on_distinct_speech() {
        let segs = vec![
            seg(true, 10_000, "the quarterly northwind forecast needs another full revision pass"),
            seg(true, 16_000, "borealis freight signs the renewal after the security review closes"),
            seg(false, 13_000, "let me pull up my calendar and check thursday afternoon instead"),
            seg(false, 19_500, "our side still owes the updated onboarding checklist this week"),
        ];
        assert!(promotion_bleed_rate(&segs) < PROMOTE_BLEED_MAX_RATE);
    }

    #[test]
    fn promotion_bleed_rate_ignores_short_and_out_of_window_mic_finals() {
        let segs = vec![
            seg(true, 10_000, "the quarterly northwind forecast needs another full revision pass"),
            // Too short to judge.
            seg(false, 11_000, "yes exactly right"),
            // Same words but a full minute away — outside the pool window.
            seg(false, 80_000, "the quarterly northwind forecast needs another full revision pass"),
        ];
        assert_eq!(promotion_bleed_rate(&segs), 0.0);
    }

    #[test]
    fn coverage_full_is_lossless() {
        // Contiguous coverage 0..60000, total 60000.
        let s = spans(&[(0, 20000), (20000, 40000), (40000, 60000)]);
        assert!(coverage_is_lossless(&s, 60_000, GAP_TOLERANCE_MS));
    }

    #[test]
    fn coverage_small_gaps_ok() {
        // 5s gaps (< 10s tolerance) pass.
        let s = spans(&[(0, 10000), (15000, 25000), (30000, 40000)]);
        assert!(coverage_is_lossless(&s, 42_000, GAP_TOLERANCE_MS));
    }

    #[test]
    fn coverage_big_gap_not_lossless() {
        // 30s hole in the middle -> incomplete.
        let s = spans(&[(0, 10000), (40000, 50000)]);
        assert!(!coverage_is_lossless(&s, 50_000, GAP_TOLERANCE_MS));
    }

    #[test]
    fn coverage_tail_gap_not_lossless() {
        // Coverage stops at 20s but recording is 60s -> missing the tail.
        let s = spans(&[(0, 10000), (15000, 20000)]);
        assert!(!coverage_is_lossless(&s, 60_000, GAP_TOLERANCE_MS));
    }

    #[test]
    fn coverage_empty_or_zero_not_lossless() {
        assert!(!coverage_is_lossless(&[], 60_000, GAP_TOLERANCE_MS));
        assert!(!coverage_is_lossless(&[(0, 60000)], 0, GAP_TOLERANCE_MS));
    }

    #[test]
    fn coverage_union_of_tracks_covers_turn_taking() {
        // Caller merges both tracks: mic during your turn, system during theirs.
        let s = spans(&[(0, 10000), (10000, 20000), (20000, 30000)]);
        assert!(coverage_is_lossless(&s, 30_000, GAP_TOLERANCE_MS));
    }

    #[test]
    fn promote_splits_into_chunk_relative() {
        let chunks = vec![
            ChunkSpan {
                index: 1,
                start_ms: 0,
                mic_track: Track::MicAec,
                mic_wav: "chunks/0001/mic_aec.wav".into(),
                system_wav: "chunks/0001/system.wav".into(),
            },
            ChunkSpan {
                index: 2,
                start_ms: 300_000,
                mic_track: Track::Mic,
                mic_wav: "chunks/0002/mic.wav".into(),
                system_wav: "chunks/0002/system.wav".into(),
            },
        ];
        let segs = vec![
            LiveSeg { is_system: true, start_ms: 1000, end_ms: 4000, text: "hi".into() },
            LiveSeg { is_system: false, start_ms: 5000, end_ms: 7000, text: "yo".into() },
            // Falls into chunk 2; rebased to 5000..8000.
            LiveSeg { is_system: true, start_ms: 305_000, end_ms: 308_000, text: "later".into() },
        ];
        let t = promote_live_to_transcript(&segs, &chunks, "sess", "whisper", 123, &|_, _| EchoDirection::Unknown);

        assert_eq!(t.chunks.len(), 2);
        assert_eq!(t.provider, "whisper");
        // chunk 1: system has "hi" @1000..4000, mic has "yo" @5000..7000.
        let c1 = &t.chunks[0];
        let c1_mic = c1.tracks.iter().find(|tr| tr.track == Track::MicAec).unwrap();
        let c1_sys = c1.tracks.iter().find(|tr| tr.track == Track::System).unwrap();
        assert_eq!(c1_mic.segments.len(), 1);
        assert_eq!(c1_mic.segments[0].text, "yo");
        assert_eq!(c1_sys.segments[0].start_ms, 1000);
        assert_eq!(c1_sys.segments[0].end_ms, 4000);
        // chunk 2: "later" rebased to 5000..8000.
        let c2_sys = t.chunks[1].tracks.iter().find(|tr| tr.track == Track::System).unwrap();
        assert_eq!(c2_sys.segments[0].start_ms, 5000);
        assert_eq!(c2_sys.segments[0].end_ms, 8000);
        // mic track present even when empty in chunk 2.
        let c2_mic = t.chunks[1].tracks.iter().find(|tr| tr.track == Track::Mic).unwrap();
        assert!(c2_mic.segments.is_empty());
    }

    #[test]
    fn bleed_filter_drops_mic_echo_keeps_real_speech() {
        let segs = vec![
            // Remote turn.
            LiveSeg { is_system: true, start_ms: 1000, end_ms: 5000, text: "the project deadline is next friday".into() },
            // Mic echo of it (built-in mic picked up the speakers) — overlaps heavily.
            LiveSeg { is_system: false, start_ms: 1200, end_ms: 5200, text: "the project deadline is next friday".into() },
            // Genuine mic speech — no system twin → must survive.
            LiveSeg { is_system: false, start_ms: 6000, end_ms: 8000, text: "sounds good i will start monday".into() },
        ];
        let kept = filter_mic_bleed(&segs);
        let mic: Vec<&LiveSeg> = kept.iter().filter(|s| !s.is_system).collect();
        assert_eq!(mic.len(), 1, "echo dropped, real speech kept");
        assert_eq!(mic[0].text, "sounds good i will start monday");
        // System turn always kept.
        assert_eq!(kept.iter().filter(|s| s.is_system).count(), 1);
    }

    #[test]
    fn promote_applies_bleed_filter() {
        let chunks = vec![ChunkSpan {
            index: 1,
            start_ms: 0,
            mic_track: Track::MicAec,
            mic_wav: "chunks/0001/mic_aec.wav".into(),
            system_wav: "chunks/0001/system.wav".into(),
        }];
        let segs = vec![
            LiveSeg { is_system: true, start_ms: 1000, end_ms: 5000, text: "can you send the report by end of day".into() },
            // Mic echo of the remote turn → must be filtered out of the promoted transcript.
            LiveSeg { is_system: false, start_ms: 1300, end_ms: 5300, text: "can you send the report by end of day".into() },
            // Real mic turn → kept.
            LiveSeg { is_system: false, start_ms: 6000, end_ms: 8000, text: "yes i will get that over shortly".into() },
        ];
        let t = promote_live_to_transcript(&segs, &chunks, "s", "whisper", 0, &|_, _| EchoDirection::Unknown);
        let mic = t.chunks[0].tracks.iter().find(|tr| tr.track == Track::MicAec).unwrap();
        assert_eq!(mic.segments.len(), 1, "mic echo filtered, real turn kept");
        assert_eq!(mic.segments[0].text, "yes i will get that over shortly");
        // Remote turn untouched.
        let sys = t.chunks[0].tracks.iter().find(|tr| tr.track == Track::System).unwrap();
        assert_eq!(sys.segments.len(), 1);
    }

    #[test]
    fn pooled_echo_catches_interleaved_fragments() {
        // The decoders split the same remote speech at different points: the
        // mic fragment straddles two system segments, so no single segment
        // contains it — the pooled window does.
        let segs = vec![
            LiveSeg { is_system: true, start_ms: 1000, end_ms: 2400, text: "i know marco has".into() },
            LiveSeg { is_system: true, start_ms: 2500, end_ms: 4400, text: "met noor but noor have you met priya".into() },
            // Mic echo spanning both system segments.
            LiveSeg { is_system: false, start_ms: 1700, end_ms: 3400, text: "marco has met noor but noor have you".into() },
            // Real mic turn — words NOT in the pool → kept.
            LiveSeg { is_system: false, start_ms: 5000, end_ms: 7000, text: "she is our point of contact for the rollout".into() },
        ];
        let kept = filter_mic_bleed(&segs);
        assert_eq!(kept.len(), 3);
        assert!(kept.iter().filter(|s| !s.is_system).all(|s| s.text.starts_with("she is")));
        // Rate reflects one of two eligible mic finals being pooled echo.
        assert!((pooled_duplication_rate(&segs) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn reverse_echo_keeps_mic_drops_system_copy() {
        let segs = vec![
            // Local speech that came back through the call 2.8s later.
            LiveSeg { is_system: false, start_ms: 1000, end_ms: 3000, text: "no no no that is not what we agreed".into() },
            LiveSeg { is_system: true, start_ms: 3800, end_ms: 5800, text: "no no no that is not what we agreed".into() },
            // Unrelated remote turn → kept.
            LiveSeg { is_system: true, start_ms: 9000, end_ms: 11_000, text: "let us move to the next item".into() },
        ];
        let kept = filter_bleed_directional(&segs, &|_, _| EchoDirection::SystemIsEcho);
        assert_eq!(kept.len(), 2);
        assert!(!kept[0].is_system, "the local original survives");
        assert_eq!(kept[1].text, "let us move to the next item");
        // Without audio arbitration the pair sits outside the legacy window
        // (mic leads by 2.8s) — nothing is dropped. The reverse region only
        // ever acts on positive acoustic proof.
        let legacy = filter_mic_bleed(&segs);
        assert_eq!(legacy.len(), 3);
    }

    #[test]
    fn substring_echo_drops_fragment_keeps_free_standing_short() {
        let segs = vec![
            LiveSeg { is_system: true, start_ms: 99_000, end_ms: 103_000,
                      text: "All that we can share with suppliers about".into() },
            // Verbatim fragment of the remote turn → bleed, dropped.
            LiveSeg { is_system: false, start_ms: 99_500, end_ms: 100_000, text: "Share with".into() },
            // Short but NOT contained in any nearby remote text → kept.
            LiveSeg { is_system: false, start_ms: 100_200, end_ms: 100_500, text: "Agreed.".into() },
            // Contained words but 20s away → outside the window → kept.
            LiveSeg { is_system: false, start_ms: 120_000, end_ms: 120_400, text: "share with".into() },
        ];
        let kept = filter_mic_bleed(&segs);
        let mic: Vec<&str> = kept.iter().filter(|s| !s.is_system).map(|s| s.text.as_str()).collect();
        assert_eq!(mic, vec!["Agreed.", "share with"]);
    }

    #[test]
    fn subsequence_echo_tolerates_mangled_words_and_window_is_asymmetric() {
        let segs = vec![
            LiveSeg { is_system: true, start_ms: 50_000, end_ms: 54_000,
                      text: "we should share it with the suppliers next week".into() },
            // Echo with a mangled plural + a dropped word → still bleed.
            LiveSeg { is_system: false, start_ms: 51_000, end_ms: 52_000,
                      text: "share with the supplier next".into() },
            // Same words but the MIC spoke 5s BEFORE the remote turn — the
            // user said it first and the remote repeated it → kept.
            LiveSeg { is_system: false, start_ms: 44_000, end_ms: 45_000,
                      text: "share it with the suppliers".into() },
            // Slight lead (decode jitter, 1.5s) → still counted as echo.
            LiveSeg { is_system: false, start_ms: 48_500, end_ms: 49_000,
                      text: "share it with the suppliers".into() },
        ];
        let kept = filter_mic_bleed(&segs);
        let mic: Vec<u32> = kept.iter().filter(|s| !s.is_system).map(|s| s.start_ms).collect();
        assert_eq!(mic, vec![44_000]);
    }

    #[test]
    fn contained_echo_catches_reworded_long_fragments() {
        let segs = vec![
            LiveSeg { is_system: true, start_ms: 10_000, end_ms: 14_000,
                      text: "That, and that will be the time that I will be talking to you about money".into() },
            // Reworded echo: order shifted, punctuation split — ≥75% word
            // containment → dropped.
            LiveSeg { is_system: false, start_ms: 11_000, end_ms: 13_000,
                      text: "That. That is the time that I will be talking to you about".into() },
            // Long turn with mostly novel words → kept.
            LiveSeg { is_system: false, start_ms: 12_000, end_ms: 13_500,
                      text: "we should circle back on the budget spreadsheet tomorrow".into() },
        ];
        let kept = filter_mic_bleed(&segs);
        let mic: Vec<&str> = kept.iter().filter(|s| !s.is_system).map(|s| s.text.as_str()).collect();
        assert_eq!(mic.len(), 1);
        assert!(mic[0].starts_with("we should circle"));
    }

    #[test]
    fn bleed_filter_keeps_short_and_out_of_window_mic() {
        let segs = vec![
            LiveSeg { is_system: true, start_ms: 1000, end_ms: 3000, text: "yes absolutely agreed".into() },
            // Under min-words → kept (too short to confidently call bleed).
            LiveSeg { is_system: false, start_ms: 1100, end_ms: 1400, text: "yeah".into() },
            // Same words but 20s away → outside window → kept.
            LiveSeg { is_system: false, start_ms: 23000, end_ms: 25000, text: "yes absolutely agreed".into() },
        ];
        let kept = filter_mic_bleed(&segs);
        assert_eq!(kept.iter().filter(|s| !s.is_system).count(), 2);
    }

    #[test]
    fn promote_trailing_segment_lands_in_last_chunk() {
        let chunks = vec![ChunkSpan {
            index: 1,
            start_ms: 0,
            mic_track: Track::Mic,
            mic_wav: "m.wav".into(),
            system_wav: "s.wav".into(),
        }];
        // start beyond the (only) chunk's nominal end still lands in it.
        let segs = vec![LiveSeg { is_system: true, start_ms: 999_999, end_ms: 1_000_000, text: "tail".into() }];
        let t = promote_live_to_transcript(&segs, &chunks, "s", "whisper", 0, &|_, _| EchoDirection::Unknown);
        let sys = t.chunks[0].tracks.iter().find(|tr| tr.track == Track::System).unwrap();
        assert_eq!(sys.segments[0].text, "tail");
    }
}
