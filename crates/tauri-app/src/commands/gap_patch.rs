//! Targeted gap re-transcription: transcribes only the uncovered spans of the
//! live transcript and splices them in.
//!
//! This module holds the pure pieces — mapping session-relative gaps onto
//! chunk-relative ranges, and merging recovered segments into the promoted
//! transcript. The WAV-slice + whisper invocation lives in `finalize.rs`,
//! which feeds its results back through [`merge_patched_segments`].

use std::path::Path;
use transcript::model::{Segment, SessionTranscript, Track};
use transcript::promote::ChunkSpan;

/// One slice of one chunk to re-transcribe, in chunk-relative milliseconds.
#[derive(Debug, Clone, PartialEq)]
pub struct PatchUnit {
    pub chunk_index: u32,
    pub rel_start_ms: u32,
    pub rel_end_ms: u32,
}

/// Recovered segments for one track of one chunk, to be merged back in.
#[derive(Debug, Clone, PartialEq)]
pub struct PatchedSegments {
    pub chunk_index: u32,
    /// True = system (far-end) track, false = mic (local) track.
    pub is_system: bool,
    pub segments: Vec<Segment>,
}

/// Map each session-relative gap `(start_ms, end_ms)` onto the chunk(s) that own
/// it, in chunk-relative time. A gap that straddles a chunk boundary splits into
/// one unit per chunk. `chunks` must be ordered by `start_ms`.
pub fn plan_gap_patch(gaps: &[(u32, u32)], chunks: &[ChunkSpan]) -> Vec<PatchUnit> {
    let mut units = Vec::new();
    for &(gs, ge) in gaps {
        if ge <= gs {
            continue;
        }
        for (i, ch) in chunks.iter().enumerate() {
            let ch_start = ch.start_ms;
            let ch_end = chunks.get(i + 1).map(|n| n.start_ms).unwrap_or(u32::MAX);
            let os = gs.max(ch_start);
            let oe = ge.min(ch_end);
            if oe > os {
                units.push(PatchUnit {
                    chunk_index: ch.index,
                    rel_start_ms: os - ch_start,
                    rel_end_ms: oe - ch_start,
                });
            }
        }
    }
    units
}

/// Splice recovered segments into the promoted transcript. For each patch,
/// finds the matching `ChunkTranscript` by index and the matching track
/// (system, or the mic track — `Mic`/`MicAec` — for local), appends the
/// segments, and re-sorts that track by `start_ms`. Patches for unknown
/// chunks/tracks are skipped.
pub fn merge_patched_segments(transcript: &mut SessionTranscript, patched: Vec<PatchedSegments>) {
    for p in patched {
        let Some(chunk) = transcript
            .chunks
            .iter_mut()
            .find(|c| c.chunk_index == p.chunk_index)
        else {
            continue;
        };
        let track = chunk.tracks.iter_mut().find(|t| {
            if p.is_system {
                t.track == Track::System
            } else {
                matches!(t.track, Track::Mic | Track::MicAec)
            }
        });
        let Some(track) = track else { continue };
        track.segments.extend(p.segments);
        track.segments.sort_by_key(|s| s.start_ms);
    }
}

/// Decide patch-in-place vs full re-pass: returns true when the gaps total
/// under half the session.
pub fn should_patch(gaps: &[(u32, u32)], total_ms: u32) -> bool {
    if total_ms == 0 {
        return false;
    }
    let gap_ms: u64 = gaps.iter().map(|(s, e)| e.saturating_sub(*s) as u64).sum();
    gap_ms * 2 < total_ms as u64
}

/// Transcribe only the gap spans from the chunk WAVs and splice the recovered
/// segments into the on-disk `transcript.json`. Both tracks (mic + system) are
/// transcribed for each gap and each is merged into its own track. Best-effort
/// per slice: a bad read is logged and skipped. Returns the number of
/// recovered segments.
pub fn patch_gaps_on_disk(
    session_root: &Path,
    model_path: &Path,
    gaps: &[(u32, u32)],
    spans: &[ChunkSpan],
) -> anyhow::Result<usize> {
    let tj_path = session_root.join("transcript.json");
    let bytes = syncsafe::read(&tj_path)?;
    let mut transcript: SessionTranscript = serde_json::from_slice(&bytes)?;

    let units = plan_gap_patch(gaps, spans);
    if units.is_empty() {
        return Ok(0);
    }
    let whisper = providers_local::WhisperLocalTranscriber::new(model_path)?;

    let mut patched: Vec<PatchedSegments> = Vec::new();
    let mut recovered = 0usize;
    for unit in &units {
        let Some(span) = spans.iter().find(|s| s.index == unit.chunk_index) else {
            continue;
        };
        for (is_system, rel_wav) in [(true, &span.system_wav), (false, &span.mic_wav)] {
            let wav = session_root.join(rel_wav);
            let samples = match read_wav_slice_f32(&wav, unit.rel_start_ms, unit.rel_end_ms) {
                Ok(s) => s,
                Err(e) => {
                    log::warn!("gap-patch read {} failed: {e}", wav.display());
                    continue;
                }
            };
            if samples.is_empty() {
                continue;
            }
            let toks = whisper.transcribe_samples(&samples)?;
            let segs: Vec<Segment> = toks
                .into_iter()
                .filter_map(|t| {
                    let text = t.text.trim().to_string();
                    if text.is_empty() {
                        return None;
                    }
                    let base = unit.rel_start_ms;
                    let start = base.saturating_add(t.start_ms.max(0) as u32);
                    let end = base.saturating_add(t.end_ms.max(0) as u32).max(start);
                    Some(Segment { start_ms: start, end_ms: end, text, confidence: None, speaker_id: None })
                })
                .collect();
            recovered += segs.len();
            if !segs.is_empty() {
                patched.push(PatchedSegments { chunk_index: unit.chunk_index, is_system, segments: segs });
            }
        }
    }

    merge_patched_segments(&mut transcript, patched);
    let out = serde_json::to_vec_pretty(&transcript)?;
    syncsafe::write(&tj_path, out)?;
    Ok(recovered)
}

/// Read `[start_ms, end_ms)` of a 16 kHz mono WAV as normalized f32 samples.
fn read_wav_slice_f32(path: &Path, start_ms: u32, end_ms: u32) -> anyhow::Result<Vec<f32>> {
    let mut reader = hound::WavReader::open(path)?;
    let sr = reader.spec().sample_rate as u64;
    let start_frame = (start_ms as u64 * sr / 1000) as u32;
    let count = (end_ms.saturating_sub(start_ms) as u64 * sr / 1000) as usize;
    reader.seek(start_frame)?;
    let out: Vec<f32> = reader
        .samples::<i16>()
        .take(count)
        .filter_map(|s| s.ok())
        .map(|s| s as f32 / 32768.0)
        .collect();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(ix: u32, start: u32) -> ChunkSpan {
        ChunkSpan {
            index: ix,
            start_ms: start,
            mic_track: Track::System, // unused by planner
            mic_wav: format!("chunks/{ix:04}/mic.wav").into(),
            system_wav: format!("chunks/{ix:04}/system.wav").into(),
        }
    }

    fn seg(start: u32, end: u32, text: &str) -> Segment {
        Segment { start_ms: start, end_ms: end, text: text.into(), confidence: None, speaker_id: None }
    }

    #[test]
    fn maps_gap_to_owning_chunk_relative_range() {
        // chunk1 [0,343000), chunk2 [343000,..). Gap 326800..342100 is in chunk1.
        let chunks = vec![span(1, 0), span(2, 343_000)];
        let plan = plan_gap_patch(&[(326_800, 342_100)], &chunks);
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0], PatchUnit { chunk_index: 1, rel_start_ms: 326_800, rel_end_ms: 342_100 });
    }

    #[test]
    fn gap_crossing_boundary_splits_per_chunk() {
        let chunks = vec![span(1, 0), span(2, 343_000)];
        let plan = plan_gap_patch(&[(340_000, 346_000)], &chunks);
        assert_eq!(plan.len(), 2);
        assert_eq!(plan[0], PatchUnit { chunk_index: 1, rel_start_ms: 340_000, rel_end_ms: 343_000 });
        assert_eq!(plan[1], PatchUnit { chunk_index: 2, rel_start_ms: 0, rel_end_ms: 3_000 });
    }

    #[test]
    fn empty_gap_is_skipped() {
        let chunks = vec![span(1, 0)];
        assert!(plan_gap_patch(&[(5_000, 5_000)], &chunks).is_empty());
    }

    #[test]
    fn should_patch_small_gaps_only() {
        assert!(should_patch(&[(326_800, 342_100)], 2_574_000)); // 15s of 43min → patch
        assert!(!should_patch(&[(0, 30_000)], 50_000)); // 30s of 50s = 60% → full pass
        assert!(!should_patch(&[], 0));
    }

    #[test]
    fn merge_inserts_into_system_track_sorted() {
        let mut t = SessionTranscript {
            schema_version: SessionTranscript::SCHEMA,
            session_id: "s".into(),
            provider: "live".into(),
            model: "live".into(),
            transcribed_at_unix_seconds: 0,
            chunks: vec![transcript::model::ChunkTranscript {
                chunk_index: 1,
                tracks: vec![
                    transcript::model::TrackTranscript {
                        track: Track::MicAec,
                        source_wav_relative: "chunks/0001/mic_aec.wav".into(),
                        segments: vec![seg(1_000, 2_000, "me")],
                    },
                    transcript::model::TrackTranscript {
                        track: Track::System,
                        source_wav_relative: "chunks/0001/system.wav".into(),
                        segments: vec![seg(1_000, 2_000, "early"), seg(350_000, 351_000, "late")],
                    },
                ],
            }],
        };
        merge_patched_segments(
            &mut t,
            vec![PatchedSegments {
                chunk_index: 1,
                is_system: true,
                segments: vec![seg(327_000, 330_000, "recovered")],
            }],
        );
        let sys = t.chunks[0].tracks.iter().find(|tr| tr.track == Track::System).unwrap();
        // Inserted and re-sorted between "early" and "late".
        assert_eq!(
            sys.segments.iter().map(|s| s.text.as_str()).collect::<Vec<_>>(),
            vec!["early", "recovered", "late"]
        );
        // Mic track untouched.
        let mic = t.chunks[0].tracks.iter().find(|tr| tr.track == Track::MicAec).unwrap();
        assert_eq!(mic.segments.len(), 1);
    }

    #[test]
    fn merge_local_patch_targets_mic_track() {
        let mut t = SessionTranscript {
            schema_version: SessionTranscript::SCHEMA,
            session_id: "s".into(),
            provider: "live".into(),
            model: "live".into(),
            transcribed_at_unix_seconds: 0,
            chunks: vec![transcript::model::ChunkTranscript {
                chunk_index: 2,
                tracks: vec![
                    transcript::model::TrackTranscript {
                        track: Track::Mic,
                        source_wav_relative: "chunks/0002/mic.wav".into(),
                        segments: vec![],
                    },
                    transcript::model::TrackTranscript {
                        track: Track::System,
                        source_wav_relative: "chunks/0002/system.wav".into(),
                        segments: vec![],
                    },
                ],
            }],
        };
        merge_patched_segments(
            &mut t,
            vec![PatchedSegments {
                chunk_index: 2,
                is_system: false,
                segments: vec![seg(0, 1_000, "local words")],
            }],
        );
        let mic = t.chunks[0].tracks.iter().find(|tr| tr.track == Track::Mic).unwrap();
        assert_eq!(mic.segments[0].text, "local words");
    }
}
