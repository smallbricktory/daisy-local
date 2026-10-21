// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Targeted gap re-transcription: transcribes only the uncovered spans of the
//! live transcript and splices them in.
//!
//! This module holds the pure pieces — mapping session-relative gaps onto
//! chunk-relative ranges, and merging recovered segments into the promoted
//! transcript — plus [`patch_gaps_on_disk`], which slices the chunk WAVs and
//! decodes each slice with the engine `finalize.rs` hands it (the same batch
//! dispatch re-transcribe uses).

use providers_http::Transcriber;
use std::path::Path;
use transcript::model::{Segment, SessionTranscript, Track};
use transcript::promote::ChunkSpan;

/// Audio decoded on each side of a stretch with energy.
const ACTIVE_PAD_MS: u32 = 2_000;

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
                t.track.is_mic()
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
/// per slice: a bad read is logged and skipped. Slices decode on as many
/// threads as the provider's `parallelism` allows. Returns the number of
/// recovered segments.
pub fn patch_gaps_on_disk(
    session_root: &Path,
    provider: &dyn Transcriber,
    gaps: &[(u32, u32)],
    spans: &[ChunkSpan],
) -> anyhow::Result<usize> {
    let tj_path = session_root.join("transcript.json");
    let bytes = syncsafe::read(&tj_path)?;
    let mut transcript: SessionTranscript = serde_json::from_slice(&bytes)?;

    sweep_slice_files(session_root);
    let units = plan_gap_patch(gaps, spans);
    if units.is_empty() {
        return Ok(0);
    }

    // One job per stretch of audio with energy in each unit and track, system
    // first, run side by side.
    // Per wav, the 1 s windows that hold audio; `None` when the wav cannot be
    // read, which leaves its units whole.
    let mut activity: std::collections::HashMap<std::path::PathBuf, Option<Vec<bool>>> = Default::default();
    let mut jobs: Vec<Job> = Vec::new();
    // Per track (mic, system): gap ms, ms with energy, ms decoded.
    let mut stats = [(0u64, 0u64, 0u64); 2];
    for unit in &units {
        let Some(span) = spans.iter().find(|s| s.index == unit.chunk_index) else {
            continue;
        };
        for (is_system, rel_wav) in [(true, &span.system_wav), (false, &span.mic_wav)] {
            let wav = syncsafe::join_rel(session_root, rel_wav);
            let flags = activity.entry(wav.clone()).or_insert_with(|| {
                transcript::rms::WavSamples::load(&wav)
                    .ok()
                    .map(|w| transcript::energy_gate::activity_windows(&w, is_system, transcript::energy_gate::MIC_IDLE_DBFS))
            });
            let range = (unit.rel_start_ms, unit.rel_end_ms);
            let slices = match flags {
                Some(flags) => {
                    let regions = transcript::energy_gate::active_regions(flags, range.0, range.1);
                    stats[is_system as usize].1 += regions.iter().map(|r| (r.1 - r.0) as u64).sum::<u64>();
                    transcript::gap::pad_and_merge(&regions, ACTIVE_PAD_MS, range)
                }
                None => vec![range],
            };
            stats[is_system as usize].0 += (range.1 - range.0) as u64;
            for (start, end) in slices {
                stats[is_system as usize].2 += (end - start) as u64;
                jobs.push(Job {
                    unit: PatchUnit { chunk_index: unit.chunk_index, rel_start_ms: start, rel_end_ms: end },
                    is_system,
                    wav: wav.clone(),
                });
            }
        }
    }
    let secs = |ms: u64| ms as f64 / 1000.0;
    log::info!(
        "gap-patch: gap audio mic {:.0} s / system {:.0} s; with energy {:.0} / {:.0} s; decoding {:.0} / {:.0} s in {} slice(s)",
        secs(stats[0].0), secs(stats[1].0), secs(stats[0].1), secs(stats[1].1), secs(stats[0].2), secs(stats[1].2), jobs.len()
    );

    let workers = provider.parallelism().clamp(1, jobs.len().max(1));
    let slice_wavs = SliceFiles(
        (0..workers).map(|w| session_root.join(format!("{SLICE_PREFIX}{w}.tmp.wav"))).collect(),
    );
    let results = run_jobs(provider, &jobs, &slice_wavs.0);
    drop(slice_wavs);

    let mut patched: Vec<PatchedSegments> = Vec::new();
    let mut recovered = 0usize;
    let total = results.len();
    let mut failures: Vec<anyhow::Error> = Vec::new();
    for r in results {
        match r {
            Ok(Some(p)) => {
                recovered += p.segments.len();
                patched.push(p);
            }
            Ok(None) => {}
            Err(e) => {
                log::warn!("gap-patch: slice skipped: {e}");
                failures.push(e);
            }
        }
    }
    if total > 0 && failures.len() == total {
        return Err(failures.swap_remove(0));
    }

    merge_patched_segments(&mut transcript, patched);
    let out = serde_json::to_vec_pretty(&transcript)?;
    syncsafe::write(&tj_path, out)?;
    Ok(recovered)
}

/// Name prefix of the temporary slice WAVs in the session directory.
const SLICE_PREFIX: &str = ".gap-patch.";

/// Removes the temporary slice WAVs when dropped, on every exit path
/// including a panic in a worker.
struct SliceFiles(Vec<std::path::PathBuf>);

impl Drop for SliceFiles {
    fn drop(&mut self) {
        for p in &self.0 {
            let _ = std::fs::remove_file(p);
        }
    }
}

/// Removes slice WAVs a killed run left behind.
fn sweep_slice_files(session_root: &Path) {
    let Ok(rd) = std::fs::read_dir(session_root) else { return };
    for e in rd.flatten() {
        if e.file_name().to_string_lossy().starts_with(SLICE_PREFIX) {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// One slice of one track to decode.
struct Job {
    unit: PatchUnit,
    is_system: bool,
    wav: std::path::PathBuf,
}

/// Decodes `jobs` on `slice_wavs.len()` threads, each using its own slice
/// file; one thread runs inline. Jobs start longest first; the results come
/// back in job order, a failed job as its own `Err`.
fn run_jobs(
    provider: &dyn Transcriber,
    jobs: &[Job],
    slice_wavs: &[std::path::PathBuf],
) -> Vec<anyhow::Result<Option<PatchedSegments>>> {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    let mut order: Vec<usize> = (0..jobs.len()).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(jobs[i].unit.rel_end_ms - jobs[i].unit.rel_start_ms));
    let next = AtomicUsize::new(0);
    let slots: Vec<Mutex<Option<anyhow::Result<Option<PatchedSegments>>>>> =
        jobs.iter().map(|_| Mutex::new(None)).collect();
    let work = |slice_wav: &std::path::Path| loop {
        let n = next.fetch_add(1, Ordering::Relaxed);
        if n >= order.len() {
            break;
        }
        let i = order[n];
        let r = decode_job(provider, &jobs[i], slice_wav);
        *slots[i].lock().unwrap_or_else(|e| e.into_inner()) = Some(r);
    };
    if let [only] = slice_wavs {
        work(only);
    } else {
        std::thread::scope(|s| {
            for slice_wav in slice_wavs {
                let work = &work;
                s.spawn(move || {
                    transcript::cpu::lower_to_background_priority();
                    work(slice_wav);
                });
            }
        });
    }
    slots
        .into_iter()
        .map(|m| m.into_inner().unwrap_or_else(|e| e.into_inner()).unwrap_or(Ok(None)))
        .collect()
}

/// The recovered segments of one job, shifted to chunk-relative time; `None`
/// when the slice is empty, unreadable, or decodes to no text. A decode
/// failure is an error.
fn decode_job(
    provider: &dyn Transcriber,
    job: &Job,
    slice_wav: &Path,
) -> anyhow::Result<Option<PatchedSegments>> {
    let unit = &job.unit;
    match write_wav_slice(&job.wav, unit.rel_start_ms, unit.rel_end_ms, slice_wav) {
        Ok(true) => {}
        Ok(false) => return Ok(None),
        Err(e) => {
            log::warn!("gap-patch read {} failed: {e}", job.wav.display());
            return Ok(None);
        }
    }
    let base = unit.rel_start_ms;
    let segments: Vec<Segment> = provider
        .transcribe(slice_wav, None)
        .map_err(|e| anyhow::anyhow!("gap-patch decode: {e}"))?
        .into_iter()
        .filter(|s| !s.text.trim().is_empty())
        .map(|mut s| {
            s.shift(base as i64);
            s
        })
        .collect();
    Ok((!segments.is_empty()).then_some(PatchedSegments {
        chunk_index: unit.chunk_index,
        is_system: job.is_system,
        segments,
    }))
}

/// Copy `[start_ms, end_ms)` of a 16 kHz mono WAV into `dest` as a standalone
/// WAV. Returns false when the slice holds no samples.
fn write_wav_slice(path: &Path, start_ms: u32, end_ms: u32, dest: &Path) -> anyhow::Result<bool> {
    let mut reader = hound::WavReader::open(path)?;
    let spec = reader.spec();
    let sr = spec.sample_rate as u64;
    let start_frame = (start_ms as u64 * sr / 1000) as u32;
    let count = (end_ms.saturating_sub(start_ms) as u64 * sr / 1000) as usize;
    reader.seek(start_frame)?;
    let samples: Vec<i16> = reader
        .samples::<i16>()
        .take(count)
        .filter_map(|s| s.ok())
        .collect();
    if samples.is_empty() {
        return Ok(false);
    }
    let mut writer = hound::WavWriter::create(dest, spec)?;
    for s in &samples {
        writer.write_sample(*s)?;
    }
    writer.finalize()?;
    Ok(true)
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
            mic_host_ns: None,
            system_host_ns: None,
            mic_samples: None,
            system_samples: None,
        }
    }

    fn seg(start: u32, end: u32, text: &str) -> Segment {
        Segment { start_ms: start, end_ms: end, text: text.into(), confidence: None, speaker_id: None, words: Vec::new(), split_group: None }
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
            provisional: false,
            edited_at_unix_seconds: None,
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
            provisional: false,
            edited_at_unix_seconds: None,
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

    mod on_disk {
        use super::*;
        use std::sync::atomic::{AtomicUsize, Ordering};

        /// Names the audio of each slice by its first sample and reports how
        /// many decodes ran at once.
        struct Marker {
            width: usize,
            running: AtomicUsize,
            peak: AtomicUsize,
            fail_on: Option<i16>,
            fail_all: bool,
        }
        impl Marker {
            fn new(width: usize) -> Self {
                Self { width, running: AtomicUsize::new(0), peak: AtomicUsize::new(0), fail_on: None, fail_all: false }
            }
        }
        impl Transcriber for Marker {
            fn name(&self) -> &'static str {
                "marker"
            }
            fn model(&self) -> &str {
                "marker"
            }
            fn parallelism(&self) -> usize {
                self.width
            }
            fn transcribe(&self, wav: &Path, _: Option<&str>) -> providers_http::Result<Vec<Segment>> {
                let now = self.running.fetch_add(1, Ordering::SeqCst) + 1;
                self.peak.fetch_max(now, Ordering::SeqCst);
                std::thread::sleep(std::time::Duration::from_millis(30));
                self.running.fetch_sub(1, Ordering::SeqCst);
                let first = hound::WavReader::open(wav).unwrap().samples::<i16>().next().unwrap().unwrap();
                if self.fail_all || self.fail_on == Some(first) {
                    return Err(providers_http::ProviderError::Other("boom".into()));
                }
                Ok(vec![seg(0, 500, &format!("sample {first}"))])
            }
        }

        /// Chunk `i` holds 10 s per track; every sample is `base + second`,
        /// with base 20000 + 1000 * i on the mic and 20000 + 1000 * i + 500 on the
        /// system, loud enough to count as audio on both tracks.
        fn session(root: &Path) -> Vec<ChunkSpan> {
            let mut spans = Vec::new();
            for i in 1..=3u32 {
                let dir = root.join(format!("chunks/{i:04}"));
                std::fs::create_dir_all(&dir).unwrap();
                for (name, base) in [("mic.wav", 20_000 + 1000 * i as i16), ("system.wav", 20_000 + 1000 * i as i16 + 500)] {
                    let pcm: Vec<i16> =
                        (0..10).flat_map(|s| std::iter::repeat_n(base + s, 16_000)).collect();
                    recording::write_wav_mono_i16(&dir.join(name), &pcm, 16_000).unwrap();
                }
                let mut s = span(i, (i - 1) * 10_000);
                s.mic_track = Track::Mic;
                spans.push(s);
            }
            let tracks = |i: u32| {
                [
                    (Track::Mic, format!("chunks/{i:04}/mic.wav")),
                    (Track::System, format!("chunks/{i:04}/system.wav")),
                ]
                .into_iter()
                .map(|(track, rel)| transcript::model::TrackTranscript {
                    track,
                    source_wav_relative: rel.into(),
                    segments: vec![],
                })
                .collect()
            };
            let t = SessionTranscript {
                schema_version: SessionTranscript::SCHEMA,
                session_id: "s".into(),
                provider: "live".into(),
                model: "live".into(),
                transcribed_at_unix_seconds: 0,
                provisional: false,
                edited_at_unix_seconds: None,
                chunks: (1..=3)
                    .map(|i| transcript::model::ChunkTranscript { chunk_index: i, tracks: tracks(i) })
                    .collect(),
            };
            std::fs::write(root.join("transcript.json"), serde_json::to_vec(&t).unwrap()).unwrap();
            spans
        }

        fn patched(provider: &Marker, root: &Path, spans: &[ChunkSpan]) -> (usize, Vec<u8>) {
            // Gaps inside chunk 1, across the chunk 1/2 boundary, and in chunk 3.
            let gaps = [(2_000, 4_000), (9_000, 12_000), (22_000, 25_000)];
            let n = patch_gaps_on_disk(root, provider, &gaps, spans).unwrap();
            (n, std::fs::read(root.join("transcript.json")).unwrap())
        }

        #[test]
        fn parallel_patch_writes_the_same_transcript_as_serial() {
            let dir = tempfile::tempdir().unwrap();
            let spans = session(dir.path());
            let before = std::fs::read(dir.path().join("transcript.json")).unwrap();

            let serial = Marker::new(1);
            let (n_serial, out_serial) = patched(&serial, dir.path(), &spans);
            assert_eq!(serial.peak.load(Ordering::SeqCst), 1);
            assert_eq!(n_serial, 8, "4 units, 2 tracks each");

            std::fs::write(dir.path().join("transcript.json"), &before).unwrap();
            let wide = Marker::new(3);
            let (n_wide, out_wide) = patched(&wide, dir.path(), &spans);
            assert!(wide.peak.load(Ordering::SeqCst) > 1, "jobs ran concurrently");
            assert_eq!(n_wide, n_serial);
            assert_eq!(out_wide, out_serial);

            let t: SessionTranscript = serde_json::from_slice(&out_wide).unwrap();
            let sys = t.chunks[0].tracks.iter().find(|x| x.track == Track::System).unwrap();
            assert_eq!(sys.segments[0].text, "sample 21502", "system slice starts at 2 s of chunk 1");
            let mic = t.chunks[2].tracks.iter().find(|x| x.track == Track::Mic).unwrap();
            assert_eq!(mic.segments[0].text, "sample 23002", "mic slice starts at 2 s of chunk 3");
        }

        /// "Hears" a slice when any sample is audible; records slice lengths.
        struct Hears(std::sync::Mutex<Vec<usize>>);
        impl Transcriber for Hears {
            fn name(&self) -> &'static str {
                "hears"
            }
            fn model(&self) -> &str {
                "hears"
            }
            fn transcribe(&self, wav: &Path, _: Option<&str>) -> providers_http::Result<Vec<Segment>> {
                let s: Vec<i16> = hound::WavReader::open(wav).unwrap().samples::<i16>().map(|x| x.unwrap()).collect();
                self.0.lock().unwrap().push(s.len() / 16);
                let at = s.iter().position(|x| x.abs() > 20).map(|i| (i / 16) as u32);
                Ok(at.map(|ms| vec![seg(ms, ms + 500, "heard")]).unwrap_or_default())
            }
        }

        #[test]
        fn a_quiet_utterance_in_a_long_silent_gap_is_recovered_and_the_silence_is_skipped() {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path();
            let cdir = root.join("chunks/0001");
            std::fs::create_dir_all(&cdir).unwrap();
            // 120 s chunk. Mic: room noise at -70 dBFS with a 2 s utterance at
            // -48 dBFS from 60 s. System: digital zero except 3 s at -40 dBFS
            // from 100 s.
            let tone = |amp: f32, i: usize| if i % 2 == 0 { (amp * 32767.0) as i16 } else { -(amp * 32767.0) as i16 };
            let mic: Vec<i16> = (0..120 * 16_000usize)
                .map(|i| match i / 16_000 {
                    60..=61 => tone(0.004, i),
                    _ => tone(0.0003, i),
                })
                .collect();
            let sys: Vec<i16> = (0..120 * 16_000usize)
                .map(|i| if (100..103).contains(&(i / 16_000)) { tone(0.01, i) } else { 0 })
                .collect();
            recording::write_wav_mono_i16(&cdir.join("mic.wav"), &mic, 16_000).unwrap();
            recording::write_wav_mono_i16(&cdir.join("system.wav"), &sys, 16_000).unwrap();
            let mut s = span(1, 0);
            s.mic_track = Track::Mic;
            let spans = vec![s];
            let t = SessionTranscript {
                schema_version: SessionTranscript::SCHEMA,
                session_id: "s".into(),
                provider: "live".into(),
                model: "live".into(),
                transcribed_at_unix_seconds: 0,
                provisional: false,
                edited_at_unix_seconds: None,
                chunks: vec![transcript::model::ChunkTranscript {
                    chunk_index: 1,
                    tracks: vec![
                        transcript::model::TrackTranscript { track: Track::Mic, source_wav_relative: "chunks/0001/mic.wav".into(), segments: vec![] },
                        transcript::model::TrackTranscript { track: Track::System, source_wav_relative: "chunks/0001/system.wav".into(), segments: vec![] },
                    ],
                }],
            };
            std::fs::write(root.join("transcript.json"), serde_json::to_vec(&t).unwrap()).unwrap();

            let p = Hears(Default::default());
            let n = patch_gaps_on_disk(root, &p, &[(5_000, 115_000)], &spans).unwrap();
            assert_eq!(n, 2, "one word on each track");
            // Two slices of about 6 s and 7 s, not two of 110 s.
            let lens = p.0.lock().unwrap().clone();
            assert_eq!(lens.len(), 2, "{lens:?}");
            assert!(lens.iter().all(|&ms| ms <= 8_000), "{lens:?}");
            let out: SessionTranscript = serde_json::from_slice(&std::fs::read(root.join("transcript.json")).unwrap()).unwrap();
            let mic = out.chunks[0].tracks.iter().find(|x| x.track == Track::Mic).unwrap();
            assert!((59_000..=62_000).contains(&mic.segments[0].start_ms), "{}", mic.segments[0].start_ms);
            let sys = out.chunks[0].tracks.iter().find(|x| x.track == Track::System).unwrap();
            assert!((99_000..=102_000).contains(&sys.segments[0].start_ms), "{}", sys.segments[0].start_ms);
        }

        struct Panics;
        impl Transcriber for Panics {
            fn name(&self) -> &'static str {
                "panics"
            }
            fn model(&self) -> &str {
                "panics"
            }
            fn parallelism(&self) -> usize {
                2
            }
            fn transcribe(&self, _: &Path, _: Option<&str>) -> providers_http::Result<Vec<Segment>> {
                panic!("decoder crashed");
            }
        }

        fn slice_files(root: &Path) -> Vec<String> {
            std::fs::read_dir(root)
                .unwrap()
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n.starts_with(".gap-patch"))
                .collect()
        }

        #[test]
        fn a_worker_panic_leaves_no_slice_files_and_a_stale_one_is_swept() {
            let dir = tempfile::tempdir().unwrap();
            let spans = session(dir.path());
            std::fs::write(dir.path().join(".gap-patch.7.tmp.wav"), b"left by a killed run").unwrap();
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                patch_gaps_on_disk(dir.path(), &Panics, &[(2_000, 4_000), (22_000, 25_000)], &spans)
            }));
            assert!(r.is_err(), "the panic propagates");
            assert!(slice_files(dir.path()).is_empty(), "{:?}", slice_files(dir.path()));
        }

        #[test]
        fn a_quiet_utterance_is_recovered_when_the_session_derives_a_gate_threshold() {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path();
            let cdir = root.join("chunks/0001");
            std::fs::create_dir_all(&cdir).unwrap();
            let tone = |amp: f32, i: usize| if i % 2 == 0 { (amp * 32767.0) as i16 } else { -(amp * 32767.0) as i16 };
            // 0-19 s: far end plays (-20 dBFS) with echo on the mic (-30).
            // 20-39 s: the local user speaks at -10 dBFS. So the gate
            // derives a threshold near -24 dBFS.
            // 40-109 s: the gap, with room noise and a 1 s utterance at
            // -54 dBFS from 70 s, under that threshold.
            let mic: Vec<i16> = (0..110 * 16_000usize)
                .map(|i| match i / 16_000 {
                    0..=19 => tone(0.03, i),
                    20..=39 => tone(0.3, i),
                    70 => tone(0.002, i),
                    _ => tone(0.0003, i),
                })
                .collect();
            let sys: Vec<i16> =
                (0..110 * 16_000usize).map(|i| if i / 16_000 < 20 { tone(0.1, i) } else { 0 }).collect();
            recording::write_wav_mono_i16(&cdir.join("mic.wav"), &mic, 16_000).unwrap();
            recording::write_wav_mono_i16(&cdir.join("system.wav"), &sys, 16_000).unwrap();
            let anchors = transcript::energy_gate::anchors_from_windows(
                &(20..40).map(|_| -10.5f32).collect::<Vec<_>>(),
                &(0..20).map(|_| -30.5f32).collect::<Vec<_>>(),
            );
            assert!(anchors.threshold_dbfs.is_some_and(|t| t > -30.0), "the session derives a gate threshold");
            let t = SessionTranscript {
                schema_version: SessionTranscript::SCHEMA,
                session_id: "s".into(),
                provider: "live".into(),
                model: "live".into(),
                transcribed_at_unix_seconds: 0,
                provisional: false,
                edited_at_unix_seconds: None,
                chunks: vec![transcript::model::ChunkTranscript {
                    chunk_index: 1,
                    tracks: vec![transcript::model::TrackTranscript {
                        track: Track::Mic,
                        source_wav_relative: "chunks/0001/mic.wav".into(),
                        segments: vec![],
                    }],
                }],
            };
            std::fs::write(root.join("transcript.json"), serde_json::to_vec(&t).unwrap()).unwrap();
            let mut s = span(1, 0);
            s.mic_track = Track::Mic;
            let p = Hears(Default::default());
            let n = patch_gaps_on_disk(root, &p, &[(40_000, 109_000)], &[s]).unwrap();
            assert_eq!(n, 1, "the utterance below the gate threshold is decoded");
        }

        #[test]
        fn far_end_speech_running_up_to_stop_is_recovered_from_the_tail_gap() {
            // Two chunks: 60 s, then 40 s. The live finals end at 84 s; the
            // far end then talks from 86 s until the last sample at 100 s,
            // and that utterance never reached the live transcript as Final.
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path();
            let tone = |amp: f32, i: usize| if i % 2 == 0 { (amp * 32767.0) as i16 } else { -(amp * 32767.0) as i16 };
            let mut spans = Vec::new();
            for (ix, start_ms, secs) in [(1u32, 0u32, 60usize), (2, 60_000, 40)] {
                let cdir = root.join(format!("chunks/{ix:04}"));
                std::fs::create_dir_all(&cdir).unwrap();
                let mic: Vec<i16> = (0..secs * 16_000).map(|i| tone(0.0003, i)).collect();
                let sys: Vec<i16> = (0..secs * 16_000)
                    .map(|i| if ix == 2 && i / 16_000 >= 26 { tone(0.03, i) } else { 0 })
                    .collect();
                recording::write_wav_mono_i16(&cdir.join("mic.wav"), &mic, 16_000).unwrap();
                recording::write_wav_mono_i16(&cdir.join("system.wav"), &sys, 16_000).unwrap();
                let mut s = span(ix, start_ms);
                s.mic_track = Track::Mic;
                spans.push(s);
            }
            let tracks = |ix: u32| {
                [Track::Mic, Track::System]
                    .into_iter()
                    .map(|track| transcript::model::TrackTranscript {
                        source_wav_relative: format!("chunks/{ix:04}/{}.wav", if track == Track::Mic { "mic" } else { "system" }).into(),
                        track,
                        segments: vec![],
                    })
                    .collect()
            };
            let t = SessionTranscript {
                schema_version: SessionTranscript::SCHEMA,
                session_id: "s".into(),
                provider: "live".into(),
                model: "live".into(),
                transcribed_at_unix_seconds: 0,
                provisional: false,
                edited_at_unix_seconds: None,
                chunks: (1..=2).map(|ix| transcript::model::ChunkTranscript { chunk_index: ix, tracks: tracks(ix) }).collect(),
            };
            std::fs::write(root.join("transcript.json"), serde_json::to_vec(&t).unwrap()).unwrap();

            let finals = [(1_000u32, 30_000u32), (31_000, 84_000)];
            let gaps = transcript::gap::uncovered_spans(&finals, 100_000, transcript::promote::GAP_TOLERANCE_MS);
            assert_eq!(gaps, vec![(84_000, 100_000)]);
            assert!(should_patch(&gaps, 100_000));
            let p = Hears(Default::default());
            let n = patch_gaps_on_disk(root, &p, &gaps, &spans).unwrap();
            assert_eq!(n, 1, "the far-end utterance at the tail");
            let out: SessionTranscript = serde_json::from_slice(&std::fs::read(root.join("transcript.json")).unwrap()).unwrap();
            let sys = out.chunks[1].tracks.iter().find(|x| x.track == Track::System).unwrap();
            assert!((25_000..=27_000).contains(&sys.segments[0].start_ms), "{}", sys.segments[0].start_ms);
        }

        #[test]
        fn a_gap_with_no_energy_on_either_track_decodes_nothing() {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path();
            let cdir = root.join("chunks/0001");
            std::fs::create_dir_all(&cdir).unwrap();
            let zeros = vec![0i16; 60 * 16_000];
            recording::write_wav_mono_i16(&cdir.join("mic.wav"), &zeros, 16_000).unwrap();
            recording::write_wav_mono_i16(&cdir.join("system.wav"), &zeros, 16_000).unwrap();
            let t = SessionTranscript {
                schema_version: SessionTranscript::SCHEMA,
                session_id: "s".into(),
                provider: "live".into(),
                model: "live".into(),
                transcribed_at_unix_seconds: 0,
                provisional: false,
                edited_at_unix_seconds: None,
                chunks: vec![],
            };
            std::fs::write(root.join("transcript.json"), serde_json::to_vec(&t).unwrap()).unwrap();
            let p = Hears(Default::default());
            let n = patch_gaps_on_disk(root, &p, &[(5_000, 55_000)], &[span(1, 0)]).unwrap();
            assert_eq!(n, 0);
            assert!(p.0.lock().unwrap().is_empty());
        }

        fn leftovers(root: &Path) -> usize {
            std::fs::read_dir(root)
                .unwrap()
                .flatten()
                .filter(|e| e.file_name().to_string_lossy().starts_with(".gap-patch"))
                .count()
        }

        #[test]
        fn one_failed_slice_is_skipped_and_the_others_are_kept() {
            let dir = tempfile::tempdir().unwrap();
            let spans = session(dir.path());
            let mut p = Marker::new(3);
            p.fail_on = Some(22509); // chunk 2 system, 9 s in
            let gaps = [(2_000, 4_000), (19_000, 20_000)];
            let n = patch_gaps_on_disk(dir.path(), &p, &gaps, &spans).unwrap();
            assert_eq!(n, 3, "4 slices, one failed");
            let out: SessionTranscript =
                serde_json::from_slice(&std::fs::read(dir.path().join("transcript.json")).unwrap()).unwrap();
            let texts: Vec<&str> = out
                .chunks
                .iter()
                .flat_map(|c| c.tracks.iter())
                .flat_map(|t| t.segments.iter())
                .map(|s| s.text.as_str())
                .collect();
            assert_eq!(texts.len(), 3, "{texts:?}");
            assert!(!texts.contains(&"sample 22509"), "{texts:?}");
            assert_eq!(leftovers(dir.path()), 0);
        }

        #[test]
        fn when_every_slice_fails_the_error_is_reported_and_the_transcript_is_kept() {
            let dir = tempfile::tempdir().unwrap();
            let spans = session(dir.path());
            let before = std::fs::read(dir.path().join("transcript.json")).unwrap();
            let mut p = Marker::new(3);
            p.fail_all = true;
            let err = patch_gaps_on_disk(dir.path(), &p, &[(19_000, 20_000)], &spans).unwrap_err();
            assert!(err.to_string().contains("gap-patch decode"), "{err}");
            assert_eq!(std::fs::read(dir.path().join("transcript.json")).unwrap(), before);
            assert_eq!(leftovers(dir.path()), 0);
        }
    }
}
