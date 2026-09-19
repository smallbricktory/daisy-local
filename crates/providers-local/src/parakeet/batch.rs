// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Parakeet batch transcriber (re-transcribe + single-track imports): the
//! same decode + age-margin committer as the live backend, replayed over the
//! WAV in overlapping windows — boundary de-dup is by construction, not
//! post-hoc.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use providers_http::{ProviderError, Result, Transcriber};
use transcript::Segment;

use super::committer::AgeMarginCommitter;
use super::realtime::ParakeetTune;
use super::{ParakeetASR, ParakeetOpts};
use crate::streaming::agreement::StreamToken;
use crate::streaming::emit::{join_tokens, split_at_gaps, to_segment, FINAL_GAP_SPLIT_MS};
use crate::streaming::window::SAMPLE_RATE;

/// Batch window/hop/commit-margin (ms). The margin only needs to absorb
/// boundary timestamp drift, not streaming lag.
const WINDOW_MS: i64 = 60_000;
const HOP_MS: i64 = 55_000;
const MARGIN_MS: i64 = 2_500;

pub struct ParakeetLocalTranscriber {
    model_dir: PathBuf,
    label: String,
    /// Lazily loaded on first transcribe; `Transcriber::transcribe` takes
    /// `&self`, the `Mutex` provides the `&mut` decode access.
    asr: Mutex<Option<ParakeetASR>>,
    tune: ParakeetTune,
}

impl ParakeetLocalTranscriber {
    pub fn new(model_dir: PathBuf, tune: ParakeetTune, label: impl Into<String>) -> Self {
        // Same clamp seam as the live constructor: tunables are settings-file
        // data and re-clamp on read.
        Self { model_dir, label: label.into(), asr: Mutex::new(None), tune: tune.clamped() }
    }
}

/// Consecutive decode windows covering `total_ms`: `window_ms` long, sliding
/// by `hop_ms`, the last one clipped to the end.
fn plan_windows(total_ms: i64, window_ms: i64, hop_ms: i64) -> Vec<(i64, i64)> {
    let mut out = Vec::new();
    if total_ms <= 0 {
        return out;
    }
    let mut start = 0;
    loop {
        let end = (start + window_ms).min(total_ms);
        out.push((start, end));
        if end >= total_ms {
            return out;
        }
        start += hop_ms;
    }
}

impl Transcriber for ParakeetLocalTranscriber {
    fn name(&self) -> &'static str {
        "parakeet_local"
    }

    fn model(&self) -> &str {
        &self.label
    }

    /// `language_hint` is ignored: parakeet TDT v2 is English-only.
    fn transcribe(&self, wav_path: &Path, _language_hint: Option<&str>) -> Result<Vec<Segment>> {
        let samples = crate::wav::decode_wav_16k_mono_f32(wav_path).map_err(ProviderError::Other)?;
        let total_ms = (samples.len() * 1000 / SAMPLE_RATE) as i64;

        let mut guard = self
            .asr
            .lock()
            .map_err(|_| ProviderError::Other("parakeet mutex poisoned".into()))?;
        if guard.is_none() {
            let opts: ParakeetOpts = self.tune.into();
            *guard = Some(
                ParakeetASR::new(&self.model_dir, &opts)
                    .map_err(|e| ProviderError::Other(format!("parakeet load: {e}")))?,
            );
        }
        let asr = guard.as_mut().expect("loaded above");

        let mut committer = AgeMarginCommitter::new();
        let mut committed: Vec<StreamToken> = Vec::new();
        let windows = plan_windows(total_ms, WINDOW_MS, HOP_MS);
        let n = windows.len();
        for (i, (start_ms, end_ms)) in windows.into_iter().enumerate() {
            let a = (start_ms as usize) * SAMPLE_RATE / 1000;
            let b = ((end_ms as usize) * SAMPLE_RATE / 1000).min(samples.len());
            let words = asr
                .decode(&samples[a..b])
                .map_err(|e| ProviderError::Other(format!("parakeet decode: {e}")))?;
            let words_abs: Vec<StreamToken> = words
                .into_iter()
                .map(|t| StreamToken {
                    text: t.text,
                    start_ms: start_ms + t.start_ms,
                    end_ms: start_ms + t.end_ms,
                })
                .collect();
            let horizon = if i + 1 == n { i64::MAX } else { end_ms - MARGIN_MS };
            committed.extend(committer.commit(&words_abs, horizon));
        }

        Ok(split_at_gaps(&committed, FINAL_GAP_SPLIT_MS)
            .into_iter()
            .flat_map(|run| split_long_runs(run, TARGET_SEG_MS, SPLIT_MIN_GAP_MS))
            .filter_map(|run| join_tokens(run).map(|t| to_segment(&t)))
            .collect())
    }
}

/// Diarization and turn rendering label per segment; continuous speech must
/// not collapse into one multi-minute segment. Once a run exceeds
/// `target_ms`, it is cut at the next inter-word gap ≥ `min_gap_ms`.
const TARGET_SEG_MS: i64 = 12_000;
const SPLIT_MIN_GAP_MS: i64 = 250;

fn split_long_runs(run: &[StreamToken], target_ms: i64, min_gap_ms: i64) -> Vec<&[StreamToken]> {
    let mut out = Vec::new();
    let mut start = 0;
    for i in 1..run.len() {
        let span = run[i - 1].end_ms - run[start].start_ms;
        let gap = run[i].start_ms - run[i - 1].end_ms;
        if span >= target_ms && gap >= min_gap_ms {
            out.push(&run[start..i]);
            start = i;
        }
    }
    if start < run.len() {
        out.push(&run[start..]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plans_overlapping_windows_with_clipped_tail() {
        assert_eq!(
            plan_windows(200_000, 60_000, 55_000),
            [(0, 60_000), (55_000, 115_000), (110_000, 170_000), (165_000, 200_000)]
        );
    }

    #[test]
    fn short_file_is_one_window() {
        assert_eq!(plan_windows(30_000, 60_000, 55_000), [(0, 30_000)]);
        assert!(plan_windows(0, 60_000, 55_000).is_empty());
    }

    fn tok(s: i64, e: i64) -> StreamToken {
        StreamToken { text: "w".into(), start_ms: s, end_ms: e }
    }

    #[test]
    fn long_runs_split_at_the_next_breath_gap() {
        // ~40s of continuous 400ms words with 50ms gaps; a 350ms breath gap
        // after words 30 and 60.
        let mut run = Vec::new();
        let mut t = 0i64;
        for i in 0..90 {
            run.push(tok(t, t + 400));
            t += 450;
            if i == 30 || i == 60 {
                t += 300;
            }
        }
        let parts = split_long_runs(&run, TARGET_SEG_MS, SPLIT_MIN_GAP_MS);
        assert_eq!(parts.len(), 3, "split at both breath gaps");
        assert!(parts.iter().all(|p| !p.is_empty()));
    }

    #[test]
    fn short_runs_stay_whole() {
        let run = vec![tok(0, 400), tok(500, 900), tok(1_000, 1_400)];
        assert_eq!(split_long_runs(&run, TARGET_SEG_MS, SPLIT_MIN_GAP_MS).len(), 1);
    }
}
