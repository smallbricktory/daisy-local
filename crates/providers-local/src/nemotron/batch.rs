// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Nemotron batch transcriber (re-transcribe, gap patch, CLI recovery): the
//! live stream run over a whole WAV as fast as the CPU allows, split into
//! utterances by the live rule.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError, TryLockError};

use providers_http::{ProviderError, Result, Transcriber};
use transcript::Segment;

use super::features::CHUNK_SAMPLES;
use super::model::NemotronModel;
use super::stream::{NemotronStream, TimedToken};
use super::words::WordAssembler;
use super::NemotronOpts;
use crate::streaming::emit::{utterance_segment, Utterances};

/// Models a batch transcriber keeps for concurrent decodes.
const MAX_SLOTS: usize = 4;
/// Most encoder threads one stream uses.
const MAX_SLOT_THREADS: usize = 4;
/// Memory one loaded model holds.
const SLOT_BYTES: u64 = 800 << 20;
/// Memory left free for the app, the OS and any live recording models.
const RESERVE_BYTES: u64 = 2 << 30;

/// `(slots, threads per slot)` for a CPU budget of `budget` threads and
/// `available_bytes` of free memory: one slot per two budget threads up to
/// `MAX_SLOTS`, limited to the models that fit above the reserve, and at
/// least one.
fn background_slots(budget: usize, available_bytes: u64) -> (usize, usize) {
    let by_cpu = (budget / 2).clamp(1, MAX_SLOTS);
    let by_memory = (available_bytes.saturating_sub(RESERVE_BYTES) / SLOT_BYTES) as usize;
    let slots = by_cpu.min(by_memory).max(1);
    (slots, (budget / slots).clamp(1, MAX_SLOT_THREADS))
}

/// Whether one more model fits above the reserve.
fn may_load_another(available_bytes: u64) -> bool {
    available_bytes >= RESERVE_BYTES + SLOT_BYTES
}

fn available_bytes() -> u64 {
    let mut sys = sysinfo::System::new();
    sys.refresh_memory();
    sys.available_memory()
}

pub struct NemotronLocalTranscriber {
    model_dir: PathBuf,
    label: String,
    opts: NemotronOpts,
    /// One per concurrent decode, each loaded on first use and kept until the
    /// transcriber drops.
    slots: Vec<Mutex<Option<NemotronModel>>>,
}

impl NemotronLocalTranscriber {
    /// Decodes inside the background finalize budget, split over several
    /// models when the budget allows.
    pub fn new(model_dir: PathBuf, label: impl Into<String>) -> Self {
        let (slots, threads) = background_slots(transcript::cpu::finalize_cpu_budget(), available_bytes());
        Self::with_slots(model_dir, label, NemotronOpts { threads, background: true }, slots)
    }

    /// One model decoding with `opts`.
    pub fn with_opts(model_dir: PathBuf, label: impl Into<String>, opts: NemotronOpts) -> Self {
        Self::with_slots(model_dir, label, opts, 1)
    }

    /// `slots` models, each decoding with `opts`.
    pub fn with_slots(model_dir: PathBuf, label: impl Into<String>, opts: NemotronOpts, slots: usize) -> Self {
        let slots = (0..slots.max(1)).map(|_| Mutex::new(None)).collect();
        Self { model_dir, label: label.into(), opts, slots }
    }

    /// The first idle slot that is loaded or may load now; waits on the first
    /// when none is.
    fn lock_slot(&self) -> MutexGuard<'_, Option<NemotronModel>> {
        for (i, slot) in self.slots.iter().enumerate() {
            match slot.try_lock() {
                Ok(g) if i == 0 || g.is_some() || may_load_another(available_bytes()) => return g,
                Ok(_) | Err(TryLockError::WouldBlock) => {}
                Err(TryLockError::Poisoned(e)) => return reset_poisoned(slot, e),
            }
        }
        self.slots[0].lock().unwrap_or_else(|e| reset_poisoned(&self.slots[0], e))
    }
}

/// The guard of a slot whose holder panicked, emptied so the next decode
/// loads a fresh model, with the poison flag cleared.
fn reset_poisoned<'a, T>(
    slot: &'a Mutex<Option<T>>,
    e: PoisonError<MutexGuard<'a, Option<T>>>,
) -> MutexGuard<'a, Option<T>> {
    let mut g = e.into_inner();
    *g = None;
    slot.clear_poison();
    g
}

/// Timed tokens to one segment per utterance.
pub fn segments(tokens: &[TimedToken]) -> Vec<Segment> {
    let mut words = WordAssembler::new();
    let mut utts = Utterances::default();
    let mut out = Vec::new();
    let mut closed_words: Vec<_> = tokens.iter().filter_map(|t| words.push(t)).collect();
    closed_words.extend(words.take());
    for w in closed_words {
        if let Some(u) = utts.push(w) {
            out.extend(utterance_segment(&u));
        }
    }
    if let Some(u) = utts.close() {
        out.extend(utterance_segment(&u));
    }
    out
}

fn decode_err(e: impl std::fmt::Display) -> ProviderError {
    ProviderError::Other(format!("nemotron decode: {e}"))
}

impl Transcriber for NemotronLocalTranscriber {
    fn name(&self) -> &'static str {
        "nemotron"
    }

    fn model(&self) -> &str {
        &self.label
    }

    /// `language_hint` is ignored: the model is English-only.
    fn transcribe(&self, wav_path: &Path, _language_hint: Option<&str>) -> Result<Vec<Segment>> {
        let samples = crate::wav::decode_wav_16k_mono_f32(wav_path).map_err(ProviderError::Other)?;
        let mut guard = self.lock_slot();
        if guard.is_none() {
            *guard = Some(
                NemotronModel::new(&self.model_dir, &self.opts)
                    .map_err(|e| ProviderError::Other(format!("nemotron load: {e}")))?,
            );
        }
        let model = guard.as_mut().expect("loaded above");
        let mut stream = NemotronStream::new();
        let mut tokens = Vec::new();
        for block in samples.chunks(CHUNK_SAMPLES) {
            tokens.extend(stream.push(model, block).map_err(decode_err)?);
        }
        tokens.extend(stream.finish(model).map_err(decode_err)?);
        Ok(segments(&tokens))
    }

    fn parallelism(&self) -> usize {
        self.slots.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tok(text: &str, ms: i64) -> TimedToken {
        TimedToken { text: text.into(), ms }
    }

    #[test]
    fn poisoned_slot_is_recovered_empty() {
        let t = std::sync::Arc::new(NemotronLocalTranscriber::with_slots(
            PathBuf::new(), "t", NemotronOpts { threads: 1, background: true }, 2,
        ));
        for i in 0..2 {
            let t2 = t.clone();
            let _ = std::thread::spawn(move || {
                let _g = t2.slots[i].lock().unwrap();
                panic!("decode panicked");
            })
            .join();
            assert!(t.slots[i].is_poisoned());
        }
        let first = t.lock_slot();
        assert!(first.is_none());
        assert!(!t.slots[0].is_poisoned());
        let second = t.lock_slot();
        assert!(second.is_none());
        assert!(!t.slots[1].is_poisoned());
    }

    #[test]
    fn tokens_become_one_segment_per_utterance() {
        let toks = [
            tok("▁Hello", 0), tok(",", 80), tok("▁there", 400),
            tok("▁Next", 5_000), tok("▁turn", 5_400), tok(".", 5_480),
        ];
        let segs = segments(&toks);
        assert_eq!(segs.len(), 2);
        assert_eq!(segs[0].text, "Hello, there");
        assert_eq!((segs[0].start_ms, segs[0].end_ms), (0, 480));
        assert_eq!(segs[1].text, "Next turn.");
        assert_eq!((segs[1].start_ms, segs[1].end_ms), (5_000, 5_560));
        let words: Vec<(&str, u32, u32)> =
            segs[0].words.iter().map(|w| (w.text.as_str(), w.start_ms, w.end_ms)).collect();
        assert_eq!(words, [("Hello,", 0, 160), ("there", 400, 480)]);
        assert!(segs.iter().all(|s| s.words_consistent()));
    }

    #[test]
    fn background_budget_splits_into_slots_of_at_most_four_threads() {
        let plenty = 64 << 30;
        assert_eq!(background_slots(1, plenty), (1, 1));
        assert_eq!(background_slots(2, plenty), (1, 2));
        assert_eq!(background_slots(4, plenty), (2, 2));
        assert_eq!(background_slots(6, plenty), (3, 2));
        assert_eq!(background_slots(8, plenty), (4, 2));
        assert_eq!(background_slots(16, plenty), (4, 4));
        assert_eq!(background_slots(64, plenty), (4, 4));
    }

    #[test]
    fn free_memory_limits_the_slots() {
        let gib = 1u64 << 30;
        // 16 GB machine, idle: all four fit above the reserve.
        assert_eq!(background_slots(8, 10 * gib), (4, 2));
        // 8 GB machine with live models loaded: about 3 GB free.
        assert_eq!(background_slots(8, 3 * gib), (1, 4));
        assert_eq!(background_slots(8, 4 * gib), (2, 4));
        // Below the reserve the transcriber still gets one model.
        assert_eq!(background_slots(8, gib), (1, 4));
        assert_eq!(background_slots(8, 0), (1, 4));
        // The memory limit never raises the CPU limit.
        assert_eq!(background_slots(2, 64 * gib), (1, 2));
    }

    #[test]
    fn an_extra_model_loads_only_with_room_above_the_reserve() {
        let gib = 1u64 << 30;
        assert!(may_load_another(4 * gib));
        assert!(!may_load_another(2 * gib));
        assert!(!may_load_another(0));
    }

    #[test]
    fn parallelism_is_the_slot_count() {
        let o = NemotronOpts { threads: 1, background: false };
        assert_eq!(NemotronLocalTranscriber::with_opts("x".into(), "x", o).parallelism(), 1);
        assert_eq!(NemotronLocalTranscriber::with_slots("x".into(), "x", o, 3).parallelism(), 3);
        assert_eq!(NemotronLocalTranscriber::with_slots("x".into(), "x", o, 0).parallelism(), 1);
    }

    #[test]
    fn no_tokens_no_segments() {
        assert!(segments(&[]).is_empty());
    }

    #[test]
    fn a_missing_model_dir_error_names_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let wav = dir.path().join("a.wav");
        let spec = hound::WavSpec { channels: 1, sample_rate: 16_000, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
        let mut w = hound::WavWriter::create(&wav, spec).unwrap();
        for _ in 0..1_600 {
            w.write_sample(0i16).unwrap();
        }
        w.finalize().unwrap();
        let t = NemotronLocalTranscriber::new(dir.path().join("absent"), "x");
        let err = t.transcribe(&wav, None).unwrap_err().to_string();
        assert!(err.contains("tokens.txt"), "{err}");
    }
}
