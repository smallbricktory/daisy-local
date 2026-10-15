// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Nemotron decode contract. Model-gated: skips unless
//! `DAISY_NEMOTRON_TEST_MODEL_DIR` names the staged model dir. With
//! `DAISY_NEMOTRON_WAV` (16 kHz mono speech) the speech cases run; with
//! `DAISY_NEMOTRON_PARITY_TSV` (sherpa-onnx words for that wav, one
//! `start_ms end_ms word` row per word) the parity case runs.
//!
//!   DAISY_NEMOTRON_TEST_MODEL_DIR=models/nemotron/nemotron-speech-streaming-en-0.6b-560ms-int8 \
//!   DAISY_NEMOTRON_WAV=…/track.wav DAISY_NEMOTRON_PARITY_TSV=…/track.tsv \
//!   cargo test -p providers-local --release --test nemotron_contract -- --nocapture

mod common;

use common::wer;
use providers_local::nemotron::model::NemotronModel;
use providers_local::nemotron::stream::{NemotronStream, TimedToken};
use providers_local::nemotron::words::WordAssembler;
use providers_local::nemotron::NemotronOpts;
use providers_local::streaming::agreement::StreamToken;
use std::path::PathBuf;

fn model() -> Option<NemotronModel> {
    let dir = std::env::var_os("DAISY_NEMOTRON_TEST_MODEL_DIR").map(PathBuf::from)?;
    Some(NemotronModel::new(&dir, &NemotronOpts::live()).expect("load model"))
}

fn wav() -> Option<Vec<f32>> {
    let p = std::env::var_os("DAISY_NEMOTRON_WAV").map(PathBuf::from)?;
    Some(providers_local::wav::decode_wav_16k_mono_f32(&p).expect("decode wav"))
}

fn decode(model: &mut NemotronModel, x: &[f32]) -> (Vec<TimedToken>, i64) {
    let mut s = NemotronStream::new();
    let mut toks = Vec::new();
    for block in x.chunks(8_960) {
        toks.extend(s.push(model, block).expect("push"));
    }
    toks.extend(s.finish(model).expect("finish"));
    (toks, s.decoded_ms())
}

fn words(toks: &[TimedToken]) -> Vec<StreamToken> {
    let mut w = WordAssembler::new();
    let mut out: Vec<StreamToken> = toks.iter().filter_map(|t| w.push(t)).collect();
    out.extend(w.take());
    out
}

fn norm(s: &str) -> String {
    s.chars().filter(|c| c.is_alphanumeric() || *c == '\'').collect::<String>().to_lowercase()
}

/// The fillers the word assembler drops, so the reference is scored on the
/// same terms.
fn is_filler(w: &str) -> bool {
    matches!(w, "um" | "uh" | "umm" | "uhh" | "uhm" | "erm" | "hmm" | "mm")
}

fn lcs_pairs(a: &[String], b: &[String]) -> Vec<(usize, usize)> {
    let (n, m) = (a.len(), b.len());
    let mut dp = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            dp[i][j] = if a[i] == b[j] { dp[i + 1][j + 1] + 1 } else { dp[i + 1][j].max(dp[i][j + 1]) };
        }
    }
    let (mut i, mut j, mut out) = (0, 0, Vec::new());
    while i < n && j < m {
        if a[i] == b[j] {
            out.push((i, j));
            i += 1;
            j += 1;
        } else if dp[i + 1][j] >= dp[i][j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    out
}

#[test]
fn silence_decodes_to_nothing() {
    let Some(mut m) = model() else { return eprintln!("skip: DAISY_NEMOTRON_TEST_MODEL_DIR not set") };
    let (toks, decoded) = decode(&mut m, &vec![0.0; 30 * 16_000]);
    assert!(toks.is_empty(), "silence produced {toks:?}");
    assert!(decoded >= 30_000);
}

#[test]
fn short_input_finishes_cleanly() {
    let Some(mut m) = model() else { return eprintln!("skip: DAISY_NEMOTRON_TEST_MODEL_DIR not set") };
    let tone: Vec<f32> = (0..1_600).map(|i| 0.3 * (i as f32 * 0.172).sin()).collect();
    let (toks, decoded) = decode(&mut m, &tone);
    assert_eq!(decoded, 560, "the partial chunk was flushed and the silent tail was not decoded");
    assert!(toks.iter().all(|t| t.ms < decoded), "{toks:?}");
}

#[test]
fn speech_tokens_are_timed_in_order_inside_the_decoded_audio() {
    let (Some(mut m), Some(x)) = (model(), wav()) else { return eprintln!("skip: model or wav env not set") };
    let (toks, decoded) = decode(&mut m, &x);
    assert!(!toks.is_empty(), "speech produced no tokens");
    assert!(toks.windows(2).all(|p| p[0].ms <= p[1].ms), "token times go backwards");
    assert!(toks.iter().all(|t| t.ms >= 0 && t.ms < decoded));
    assert!(decoded < x.len() as i64 * 1000 / 16_000 + 560, "decoded {decoded} ms past the input");
    let w = words(&toks);
    assert!(w.iter().all(|w| w.start_ms < w.end_ms && !w.text.is_empty()));
}

#[test]
fn words_match_the_sherpa_reference() {
    let (Some(mut m), Some(x)) = (model(), wav()) else { return eprintln!("skip: model or wav env not set") };
    let Some(tsv) = std::env::var_os("DAISY_NEMOTRON_PARITY_TSV") else { return eprintln!("skip: DAISY_NEMOTRON_PARITY_TSV not set") };
    let reference: Vec<(i64, String)> = std::fs::read_to_string(tsv)
        .unwrap()
        .lines()
        .filter_map(|l| {
            let mut p = l.split('\t');
            let start = p.next()?.parse().ok()?;
            let _end = p.next()?;
            Some((start, norm(p.next()?)))
        })
        .filter(|(_, w)| !w.is_empty() && !is_filler(w))
        .collect();
    let ours: Vec<(i64, String)> = words(&decode(&mut m, &x).0)
        .into_iter()
        .map(|w| (w.start_ms, norm(&w.text)))
        .filter(|(_, w)| !w.is_empty())
        .collect();
    let r: Vec<String> = reference.iter().map(|(_, w)| w.clone()).collect();
    let h: Vec<String> = ours.iter().map(|(_, w)| w.clone()).collect();
    let e = wer(&r, &h);
    println!("parity WER {e:.4} over {} reference words ({} ours)", r.len(), h.len());
    let mut deltas: Vec<i64> = lcs_pairs(&r, &h).into_iter().map(|(i, j)| ours[j].0 - reference[i].0).collect();
    deltas.sort_unstable();
    let mut offsets: Vec<i64> = deltas.iter().map(|d| d.abs()).collect();
    offsets.sort_unstable();
    let median = offsets[offsets.len() / 2];
    println!(
        "median |start offset| {median} ms, signed median {} ms, p90 |offset| {} ms over {} aligned words",
        deltas[deltas.len() / 2],
        offsets[offsets.len() * 9 / 10],
        offsets.len()
    );
    assert!(e <= 0.02, "word error vs sherpa {e:.4}");
    assert!(median <= 160, "median start offset {median} ms");
}

#[test]
fn batch_segments_are_ordered_and_disjoint() {
    use providers_http::Transcriber;
    let Some(dir) = std::env::var_os("DAISY_NEMOTRON_TEST_MODEL_DIR").map(PathBuf::from) else { return eprintln!("skip") };
    let Some(wav) = std::env::var_os("DAISY_NEMOTRON_WAV").map(PathBuf::from) else { return eprintln!("skip") };
    let t = providers_local::nemotron::batch::NemotronLocalTranscriber::new(dir, "batch test");
    let segs = t.transcribe(&wav, None).expect("transcribe");
    assert!(!segs.is_empty());
    assert!(segs.iter().all(|s| s.start_ms < s.end_ms && !s.text.trim().is_empty()));
    assert!(segs.windows(2).all(|p| p[1].start_ms >= p[0].end_ms), "segments overlap");
}
