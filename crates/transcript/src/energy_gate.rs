//! Per-session speech/residue energy anchors and the finalize-time energy
//! gate. Anchors come from 1-second windows over each chunk's mic + system
//! WAVs: windows where the system track is silent teach the local speech
//! level; windows where it is active teach the echo-residue floor. The gate
//! drops mic transcript segments whose audio peak sits below the derived
//! threshold. Everything degrades to "drop nothing" when the evidence is
//! thin.

use crate::model::{SessionTranscript, Track};
use crate::rms::{peak_dbfs_window, rms_dbfs_window, WavSamples};
use std::path::Path;

const WINDOW_MS: u32 = 1_000;
/// System window with RMS above this counts as far-end audio playing.
const SYSTEM_ACTIVE_DBFS: f32 = -50.0;
/// Mic window below this is idle-room silence, not a speech candidate.
const MIC_IDLE_DBFS: f32 = -60.0;
/// Anchors need at least this many windows each to be trusted.
const MIN_WINDOWS: usize = 10;
const THRESHOLD_MIN_DBFS: f32 = -60.0;
const THRESHOLD_MAX_DBFS: f32 = -20.0;
/// The threshold sits this far above the residue ceiling.
const RESIDUE_MARGIN_DB: f32 = 6.0;
/// The threshold stays at least this far under the speech anchor, keeping a
/// second local speaker sitting farther from the mic (well below the main
/// speaker's level) out of the gate's reach.
const SPEECH_HEADROOM_DB: f32 = 10.0;
/// Percentiles. Speech = central mass of speaking windows. The residue
/// *floor* (p20 of system-active windows) is the stored telemetry anchor;
/// the residue *ceiling* (p90 of the same windows minus double-talk) is
/// what the threshold clears.
const SPEECH_PCT: f32 = 0.75;
const RESIDUE_FLOOR_PCT: f32 = 0.20;
const RESIDUE_CEIL_PCT: f32 = 0.90;
/// System-active windows with mic peaks within this of the speech anchor
/// are double-talk (the user speaking over the far end), not residue.
const DOUBLE_TALK_GAP_DB: f32 = 10.0;

#[derive(Debug, Clone, Default)]
pub struct SessionAnchors {
    pub speech_dbfs: Option<f32>,
    pub residue_dbfs: Option<f32>,
    pub threshold_dbfs: Option<f32>,
    pub speech_windows: usize,
    pub residue_windows: usize,
}

/// Split a chunk into 1-second windows and return the mic peak (dBFS) of
/// each window, bucketed by whether the system track was active in it:
/// `(speech_candidates, residue_candidates)`.
pub fn scan_windows(mic: &WavSamples, system: &WavSamples) -> (Vec<f32>, Vec<f32>) {
    // Only the span both tracks cover is classifiable: past a truncated
    // system WAV there is no activity evidence, and bleed there would be
    // mistaken for speech.
    let ms = |w: &WavSamples| (w.len() as u64 * 1000 / w.sample_rate().max(1) as u64) as u32;
    let dur_ms = ms(mic).min(ms(system));
    let mut speech = Vec::new();
    let mut residue = Vec::new();
    let mut t = 0u32;
    while t + WINDOW_MS <= dur_ms {
        let mic_peak = peak_dbfs_window(mic, t, t + WINDOW_MS);
        let sys_rms = rms_dbfs_window(system, t, t + WINDOW_MS);
        if sys_rms > SYSTEM_ACTIVE_DBFS {
            residue.push(mic_peak);
        } else if mic_peak > MIC_IDLE_DBFS {
            speech.push(mic_peak);
        }
        t += WINDOW_MS;
    }
    (speech, residue)
}

/// Load a chunk's mic + system WAV pair and scan it. None when either file
/// is missing/unreadable.
pub fn scan_chunk_files(mic: &Path, system: &Path) -> Option<(Vec<f32>, Vec<f32>)> {
    let (Ok(m), Ok(s)) = (WavSamples::load(mic), WavSamples::load(system)) else {
        return None;
    };
    Some(scan_windows(&m, &s))
}

fn percentile(sorted: &[f32], pct: f32) -> f32 {
    let idx = ((sorted.len() - 1) as f32 * pct).round() as usize;
    sorted[idx]
}

/// Derive anchors + threshold from accumulated window peaks. Threshold is
/// `None` unless both anchors have enough windows AND are separated enough;
/// callers treat `None` as gate-off.
pub fn anchors_from_windows(speech: &[f32], residue: &[f32]) -> SessionAnchors {
    let mut a = SessionAnchors {
        speech_windows: speech.len(),
        residue_windows: residue.len(),
        ..Default::default()
    };
    let mut sp: Vec<f32> = speech.to_vec();
    let mut rs: Vec<f32> = residue.to_vec();
    sp.sort_by(|x, y| x.total_cmp(y));
    rs.sort_by(|x, y| x.total_cmp(y));
    if sp.len() >= MIN_WINDOWS {
        a.speech_dbfs = Some(percentile(&sp, SPEECH_PCT));
    }
    if rs.len() >= MIN_WINDOWS {
        a.residue_dbfs = Some(percentile(&rs, RESIDUE_FLOOR_PCT));
    }
    if let Some(s) = a.speech_dbfs {
        // Residue ceiling: the loudest genuine bleed, with the user's own
        // double-talk windows excluded.
        let quiet: Vec<f32> = rs
            .iter()
            .copied()
            .filter(|&p| p < s - DOUBLE_TALK_GAP_DB)
            .collect();
        if quiet.len() >= MIN_WINDOWS {
            let ceiling = percentile(&quiet, RESIDUE_CEIL_PCT);
            let t = ceiling + RESIDUE_MARGIN_DB;
            if t <= s - SPEECH_HEADROOM_DB {
                a.threshold_dbfs = Some(t.clamp(THRESHOLD_MIN_DBFS, THRESHOLD_MAX_DBFS));
            }
        }
    }
    a
}

/// One mic segment's measured level and the gate's verdict, in
/// chunk-relative time. `peak_dbfs` None = unmeasurable (always kept).
#[derive(Debug, Clone)]
pub struct SegmentPeak {
    pub chunk_index: u32,
    pub start_ms: u32,
    pub end_ms: u32,
    pub peak_dbfs: Option<f32>,
    pub kept: bool,
}

#[derive(Debug, Clone, Default)]
pub struct GateOutcome {
    pub anchors: SessionAnchors,
    pub dropped: usize,
    /// Every mic segment's measurement + verdict, for the session's
    /// flight-recorder sidecar.
    pub segment_peaks: Vec<SegmentPeak>,
}

/// Compute session anchors and gate the mic tracks in one pass: each chunk's
/// WAV pair is loaded once, feeding both the anchor windows and the
/// per-segment peaks the gate filters on. System tracks are never touched;
/// segments with missing/unreadable audio, or lying wholly past a truncated
/// WAV, are unmeasurable and always kept.
///
/// `apply` false computes anchors and peaks but drops nothing — callers pass
/// false for multi-local-speaker sessions, where a second speaker far from
/// the mic is indistinguishable from residue by level.
pub fn gate_session(st: &mut SessionTranscript, session_dir: &Path, apply: bool) -> GateOutcome {
    let mut speech = Vec::new();
    let mut residue = Vec::new();
    // Per-segment mic peaks (None = unmeasurable, always kept), parallel to
    // each chunk's mic track segments.
    let mut mic_peaks: Vec<Option<Vec<Option<f32>>>> = Vec::with_capacity(st.chunks.len());

    for chunk in &st.chunks {
        let mic_track = chunk
            .tracks
            .iter()
            .find(|t| matches!(t.track, Track::Mic | Track::MicAec));
        let sys_rel = chunk
            .tracks
            .iter()
            .find(|t| t.track == Track::System)
            .map(|t| &t.source_wav_relative);
        let (Some(mic_t), Some(sys_rel)) = (mic_track, sys_rel) else {
            mic_peaks.push(None);
            continue;
        };
        let Ok(mic) = WavSamples::load(&session_dir.join(&mic_t.source_wav_relative)) else {
            mic_peaks.push(None);
            continue;
        };
        if let Ok(sys) = WavSamples::load(&session_dir.join(sys_rel)) {
            let (sp, rs) = scan_windows(&mic, &sys);
            speech.extend(sp);
            residue.extend(rs);
        }
        let mic_dur_ms = (mic.len() as u64 * 1000 / mic.sample_rate().max(1) as u64) as u32;
        mic_peaks.push(Some(
            mic_t
                .segments
                .iter()
                .map(|s| {
                    // A segment wholly past the recorded audio (truncated
                    // WAV) cannot be measured; it must not read as silence.
                    if s.start_ms >= mic_dur_ms {
                        None
                    } else {
                        Some(peak_dbfs_window(&mic, s.start_ms, s.end_ms))
                    }
                })
                .collect(),
        ));
    }

    let anchors = anchors_from_windows(&speech, &residue);
    let threshold = anchors.threshold_dbfs.filter(|_| apply);

    let mut dropped = 0usize;
    let mut segment_peaks = Vec::new();
    for (chunk, peaks) in st.chunks.iter_mut().zip(mic_peaks) {
        let Some(peaks) = peaks else { continue };
        let Some(track) = chunk
            .tracks
            .iter_mut()
            .find(|t| matches!(t.track, Track::Mic | Track::MicAec))
        else {
            continue;
        };
        for (seg, peak) in track.segments.iter().zip(&peaks) {
            let kept = match (threshold, peak) {
                (Some(t), Some(p)) => *p >= t,
                _ => true,
            };
            segment_peaks.push(SegmentPeak {
                chunk_index: chunk.chunk_index,
                start_ms: seg.start_ms,
                end_ms: seg.end_ms,
                peak_dbfs: *peak,
                kept,
            });
        }
        if let Some(threshold) = threshold {
            let before = track.segments.len();
            let mut i = 0;
            track.segments.retain(|_| {
                let keep = peaks[i].is_none_or(|p| p >= threshold);
                i += 1;
                keep
            });
            dropped += before - track.segments.len();
        }
    }
    GateOutcome { anchors, dropped, segment_peaks }
}

/// Calibration window length. Shorter than the anchor scan's 1 s so a brief
/// read-aloud yields enough voiced windows for a stable percentile.
const CALIBRATION_WINDOW_MS: u32 = 250;
/// Calibration windows quieter than this are pauses, not speech.
const CALIBRATION_VOICED_DBFS: f32 = -50.0;
/// Minimum voiced windows (2 s of speech) for a trustworthy calibration.
const CALIBRATION_MIN_VOICED: usize = 8;
const CALIBRATION_PCT: f32 = 0.90;

/// Speech level (dBFS) from a calibration clip: p90 of voiced 250 ms window
/// peaks. None when the clip holds under 2 s of voiced audio.
pub fn calibration_speech_dbfs(mic: &WavSamples) -> Option<f32> {
    let dur_ms = (mic.len() as u64 * 1000 / mic.sample_rate().max(1) as u64) as u32;
    let mut voiced: Vec<f32> = Vec::new();
    let mut t = 0u32;
    while t + CALIBRATION_WINDOW_MS <= dur_ms {
        let p = peak_dbfs_window(mic, t, t + CALIBRATION_WINDOW_MS);
        if p > CALIBRATION_VOICED_DBFS {
            voiced.push(p);
        }
        t += CALIBRATION_WINDOW_MS;
    }
    if voiced.len() < CALIBRATION_MIN_VOICED {
        return None;
    }
    voiced.sort_by(|a, b| a.total_cmp(b));
    Some(percentile(&voiced, CALIBRATION_PCT))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rms::WavSamples;

    fn silence(secs: usize) -> Vec<i16> {
        vec![0; 16_000 * secs]
    }
    fn tone(secs: usize, amp: f32) -> Vec<i16> {
        let v = (amp * i16::MAX as f32) as i16;
        (0..16_000 * secs).map(|i| if i % 2 == 0 { v } else { -v }).collect()
    }

    #[test]
    fn scan_separates_speech_and_residue_windows() {
        let mut mic = tone(10, 0.25);
        mic.extend(tone(10, 0.01));
        let mut sys = silence(10);
        sys.extend(tone(10, 0.3));
        let mic = WavSamples::from_raw(mic, 16_000);
        let sys = WavSamples::from_raw(sys, 16_000);
        let (speech, residue) = scan_windows(&mic, &sys);
        assert!(speech.len() >= 9 && speech.len() <= 10, "speech windows: {}", speech.len());
        assert!(residue.len() >= 9 && residue.len() <= 10, "residue windows: {}", residue.len());
        assert!(speech.iter().all(|&p| p > -14.0 && p < -10.0));
        assert!(residue.iter().all(|&p| p > -42.0 && p < -38.0));
    }

    #[test]
    fn quiet_idle_windows_are_not_speech_candidates() {
        let mic = WavSamples::from_raw(silence(10), 16_000);
        let sys = WavSamples::from_raw(silence(10), 16_000);
        let (speech, residue) = scan_windows(&mic, &sys);
        assert!(speech.is_empty());
        assert!(residue.is_empty());
    }

    #[test]
    fn anchors_require_min_windows_and_separation() {
        let a = anchors_from_windows(&[-12.0; 5], &[-40.0; 5]);
        assert!(a.threshold_dbfs.is_none());

        // Residue ceiling -40 → threshold -34, well under speech-10.
        let a = anchors_from_windows(&[-12.0; 20], &[-40.0; 20]);
        let t = a.threshold_dbfs.unwrap();
        assert!((t - (-34.0)).abs() < 0.5, "got {t}");

        // Too close: threshold would violate the speech headroom.
        let a = anchors_from_windows(&[-30.0; 20], &[-36.0; 20]);
        assert!(a.threshold_dbfs.is_none());
    }

    #[test]
    fn threshold_sits_on_residue_side_sparing_quiet_second_speaker() {
        // Main speaker -14, far-from-mic second speaker -30, residue -40:
        // the threshold must clear the residue ceiling but stay below the
        // quiet speaker's level.
        let mut speech = vec![-14.0f32; 30];
        speech.extend(vec![-30.0f32; 6]);
        let a = anchors_from_windows(&speech, &[-40.0; 30]);
        let t = a.threshold_dbfs.unwrap();
        assert!(t < -30.0, "quiet speaker at -30 must survive, got {t}");
        assert!(t > -40.0, "residue at -40 must be gated, got {t}");
    }

    #[test]
    fn double_talk_windows_do_not_raise_threshold() {
        // System-active windows where the user talks over the far end peak
        // near the speech level; they must not push the residue ceiling up.
        let mut residue = vec![-40.0f32; 30];
        residue.extend(vec![-13.0f32; 10]); // double-talk
        let a = anchors_from_windows(&[-12.0; 30], &residue);
        let t = a.threshold_dbfs.unwrap();
        assert!((t - (-34.0)).abs() < 0.5, "got {t}");
    }

    #[test]
    fn threshold_clamps_to_sane_range() {
        let a = anchors_from_windows(&[-2.0; 20], &[-80.0; 20]);
        let t = a.threshold_dbfs.unwrap();
        assert!((-60.0..=-20.0).contains(&t), "got {t}");
    }

    #[test]
    fn calibration_needs_voiced_windows_and_takes_p90() {
        // 1.5 s of speech in 8 s of near-silence: under the 2 s voiced floor.
        let mut short = silence(3);
        short.extend(tone(1, 0.1));
        short.extend(tone(1, 0.1)[..8_000].to_vec());
        short.extend(silence(3));
        let w = WavSamples::from_raw(short, 16_000);
        assert!(calibration_speech_dbfs(&w).is_none());

        // 4 s of speech at -20 dB among pauses: calibrates near -20.
        let mut clip = silence(2);
        clip.extend(tone(4, 0.1));
        clip.extend(silence(2));
        let w = WavSamples::from_raw(clip, 16_000);
        let db = calibration_speech_dbfs(&w).unwrap();
        assert!((db - (-20.0)).abs() < 1.0, "got {db}");
    }

    #[test]
    fn gate_session_drops_only_quiet_mic_segments_single_pass() {
        use crate::model::{ChunkTranscript, Segment, SessionTranscript, Track, TrackTranscript};
        let dir = std::env::temp_dir().join(format!("daisy-gate-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("chunks/0001")).unwrap();

        // 30 s: 15 s user speaking (-12 dB) with system silent, then 15 s of
        // system audio (-20 dB) with mic residue (-40 dB) — enough windows
        // for both anchors.
        let mut mic = tone(15, 0.25);
        mic.extend(tone(15, 0.01));
        let mut sys = silence(15);
        sys.extend(tone(15, 0.1));
        write_wav(&dir.join("chunks/0001/mic_aec.wav"), &mic);
        write_wav(&dir.join("chunks/0001/system.wav"), &sys);

        let seg = |s: u32, e: u32, t: &str| Segment {
            start_ms: s,
            end_ms: e,
            text: t.into(),
            confidence: None,
            speaker_id: None,
        };
        let mut st = SessionTranscript {
            schema_version: SessionTranscript::SCHEMA,
            session_id: "s".into(),
            provider: "t".into(),
            model: "t".into(),
            transcribed_at_unix_seconds: 0,
            chunks: vec![ChunkTranscript {
                chunk_index: 1,
                tracks: vec![
                    TrackTranscript {
                        track: Track::MicAec,
                        source_wav_relative: "chunks/0001/mic_aec.wav".into(),
                        segments: vec![seg(1_000, 5_000, "real"), seg(20_000, 24_000, "ghost")],
                    },
                    TrackTranscript {
                        track: Track::System,
                        source_wav_relative: "chunks/0001/system.wav".into(),
                        segments: vec![seg(16_000, 29_000, "remote words")],
                    },
                ],
            }],
        };
        let out = gate_session(&mut st, &dir, true);
        assert!(out.anchors.threshold_dbfs.is_some());
        assert_eq!(out.dropped, 1);
        assert_eq!(out.segment_peaks.len(), 2);
        assert!(out.segment_peaks.iter().any(|p| !p.kept));
        let mic_t = st.chunks[0].tracks.iter().find(|t| t.track == Track::MicAec).unwrap();
        assert_eq!(mic_t.segments.len(), 1);
        assert_eq!(mic_t.segments[0].text, "real");
        let sys_t = st.chunks[0].tracks.iter().find(|t| t.track == Track::System).unwrap();
        assert_eq!(sys_t.segments.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn gate_session_missing_audio_drops_nothing() {
        use crate::model::{ChunkTranscript, Segment, SessionTranscript, Track, TrackTranscript};
        let dir = std::env::temp_dir().join(format!("daisy-gate-miss-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut st = SessionTranscript {
            schema_version: SessionTranscript::SCHEMA,
            session_id: "s".into(),
            provider: "t".into(),
            model: "t".into(),
            transcribed_at_unix_seconds: 0,
            chunks: vec![ChunkTranscript {
                chunk_index: 1,
                tracks: vec![TrackTranscript {
                    track: Track::Mic,
                    source_wav_relative: "chunks/0001/mic.wav".into(),
                    segments: vec![Segment {
                        start_ms: 0,
                        end_ms: 1_000,
                        text: "kept".into(),
                        confidence: None,
                        speaker_id: None,
                    }],
                }],
            }],
        };
        let out = gate_session(&mut st, &dir, true);
        assert_eq!(out.dropped, 0);
        assert_eq!(st.chunks[0].tracks[0].segments.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn gate_session_keeps_segments_past_truncated_wav_and_honors_apply_false() {
        use crate::model::{ChunkTranscript, Segment, SessionTranscript, Track, TrackTranscript};
        let dir = std::env::temp_dir().join(format!("daisy-gate-trunc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("chunks/0001")).unwrap();

        // Valid anchors from the first 30 s; the mic WAV ends at 30 s but a
        // trailing segment claims 31-33 s (crash-truncated recording).
        let mut mic = tone(15, 0.25);
        mic.extend(tone(15, 0.01));
        let mut sys = silence(15);
        sys.extend(tone(15, 0.1));
        write_wav(&dir.join("chunks/0001/mic_aec.wav"), &mic);
        write_wav(&dir.join("chunks/0001/system.wav"), &sys);

        let seg = |s: u32, e: u32, t: &str| Segment {
            start_ms: s,
            end_ms: e,
            text: t.into(),
            confidence: None,
            speaker_id: None,
        };
        let mk = || SessionTranscript {
            schema_version: SessionTranscript::SCHEMA,
            session_id: "s".into(),
            provider: "t".into(),
            model: "t".into(),
            transcribed_at_unix_seconds: 0,
            chunks: vec![ChunkTranscript {
                chunk_index: 1,
                tracks: vec![
                    TrackTranscript {
                        track: Track::MicAec,
                        source_wav_relative: "chunks/0001/mic_aec.wav".into(),
                        segments: vec![
                            seg(20_000, 24_000, "residue"),
                            seg(31_000, 33_000, "trailing words"),
                        ],
                    },
                    TrackTranscript {
                        track: Track::System,
                        source_wav_relative: "chunks/0001/system.wav".into(),
                        segments: vec![seg(16_000, 29_000, "remote")],
                    },
                ],
            }],
        };

        let mut st = mk();
        let out = gate_session(&mut st, &dir, true);
        assert_eq!(out.dropped, 1, "only the measurable residue segment drops");
        let mic_t = &st.chunks[0].tracks[0];
        assert_eq!(mic_t.segments.len(), 1);
        assert_eq!(mic_t.segments[0].text, "trailing words");

        // apply=false: anchors computed, nothing dropped.
        let mut st = mk();
        let out = gate_session(&mut st, &dir, false);
        assert!(out.anchors.threshold_dbfs.is_some());
        assert_eq!(out.dropped, 0);
        assert_eq!(st.chunks[0].tracks[0].segments.len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn write_wav(path: &std::path::Path, samples: &[i16]) {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 16_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut w = hound::WavWriter::create(path, spec).unwrap();
        for &s in samples {
            w.write_sample(s).unwrap();
        }
        w.finalize().unwrap();
    }
}
