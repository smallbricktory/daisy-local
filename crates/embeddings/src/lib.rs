// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Local embedding + indexing for semantic search over transcripts.
//!
//! Model: granite-embedding-97m-multilingual-r2 (ONNX, 384-dim CLS-pooled),
//! run on CPU via `ort`.
//!
//! Per-session index lives at:
//!   <session_dir>/chunks.json      — chunk text + timestamps
//!   <session_dir>/embeddings.bin   — [u32 count][u32 dim][count*dim f32]
//!
//! Queries run a cosine scan over all chunks.

use ndarray::{Array2, ArrayView2};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use thiserror::Error;
use tokenizers::Tokenizer;
use unicode_segmentation::UnicodeSegmentation;

pub const EMBED_DIM: usize = 384;
/// Token budget per encoded text, special tokens included. Longer inputs are
/// truncated by the tokenizer before the template adds its special tokens.
const MAX_TOKENS: usize = 2048;

#[derive(Debug, Error)]
pub enum EmbedError {
    #[error("model dir missing or incomplete: {0}")]
    ModelMissing(String),
    #[error("ort: {0}")]
    Ort(String),
    #[error("tokenizer: {0}")]
    Tokenizer(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("decode: {0}")]
    Decode(String),
}

// Converts every `ort::Error<R>` recover variant via Display.
impl<R> From<ort::Error<R>> for EmbedError {
    fn from(value: ort::Error<R>) -> Self {
        Self::Ort(format!("{value}"))
    }
}

pub type Result<T> = std::result::Result<T, EmbedError>;

/// Resolve the embeddings model directory: the `DAISY_EMBED_DIR` env var,
/// else `models/embeddings/` next to the executable.
pub fn model_dir() -> PathBuf {
    if let Ok(p) = std::env::var("DAISY_EMBED_DIR") {
        return PathBuf::from(p);
    }
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
        .unwrap_or_default()
        .join("models/embeddings")
}

static LOADS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static RESIDENT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Encoder sessions this process has built since it started.
pub fn loads() -> u64 {
    LOADS.load(std::sync::atomic::Ordering::Relaxed)
}

/// Encoder sessions alive right now. One model's weights are resident per
/// count.
pub fn resident() -> u64 {
    RESIDENT.load(std::sync::atomic::Ordering::Relaxed)
}

/// Loaded tokenizer + ONNX session.
pub struct Encoder {
    session: ort::session::Session,
    tokenizer: Tokenizer,
}

impl Drop for Encoder {
    fn drop(&mut self) {
        RESIDENT.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    }
}

impl Encoder {
    pub fn load() -> Result<Self> {
        let dir = model_dir();
        let model_path = dir.join("model.onnx");
        let tok_path = dir.join("tokenizer.json");
        if !model_path.is_file() {
            return Err(EmbedError::ModelMissing(format!(
                "{} missing (run scripts/download-embeddings.sh or rebuild the AppImage)",
                model_path.display()
            )));
        }
        if !tok_path.is_file() {
            return Err(EmbedError::ModelMissing(format!(
                "{} missing",
                tok_path.display()
            )));
        }
        let session = ort::session::Session::builder()?
            .with_optimization_level(ort::session::builder::GraphOptimizationLevel::Level3)?
            .commit_from_file(&model_path)?;
        let mut tokenizer = Tokenizer::from_file(&tok_path)
            .map_err(|e| EmbedError::Tokenizer(format!("{e}")))?;
        tokenizer
            .with_truncation(Some(tokenizers::TruncationParams {
                max_length: MAX_TOKENS,
                ..Default::default()
            }))
            .map_err(|e| EmbedError::Tokenizer(format!("{e}")))?;
        LOADS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        RESIDENT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Ok(Self { session, tokenizer })
    }

    /// Encode a single text string into a 384-dim L2-normalized vector.
    pub fn encode(&mut self, text: &str) -> Result<Vec<f32>> {
        Ok(self.encode_batch(&[text])?.into_iter().next().unwrap())
    }

    /// Encode N texts in one ONNX call. Pads to the longest in the batch.
    pub fn encode_batch(&mut self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let mut encs = Vec::with_capacity(texts.len());
        for t in texts {
            let e = self
                .tokenizer
                .encode(*t, true)
                .map_err(|err| EmbedError::Tokenizer(format!("{err}")))?;
            encs.push(e);
        }
        let max_len = encs.iter().map(|e| e.get_ids().len()).max().unwrap_or(0);
        let batch = encs.len();

        let mut input_ids = Array2::<i64>::zeros((batch, max_len));
        let mut attn_mask = Array2::<i64>::zeros((batch, max_len));
        let mut tok_types = Array2::<i64>::zeros((batch, max_len));
        for (i, e) in encs.iter().enumerate() {
            for (j, id) in e.get_ids().iter().enumerate() {
                input_ids[(i, j)] = *id as i64;
            }
            for (j, m) in e.get_attention_mask().iter().enumerate() {
                attn_mask[(i, j)] = *m as i64;
            }
            for (j, tt) in e.get_type_ids().iter().enumerate() {
                tok_types[(i, j)] = *tt as i64;
            }
        }

        // Tensors are bound by the model's declared input names; a model
        // without token_type_ids gets only the inputs it declares.
        let in_names: Vec<String> = self
            .session
            .inputs()
            .iter()
            .map(|i| i.name().to_string())
            .collect();
        let mut inputs: Vec<(&str, ort::session::SessionInputValue)> = Vec::new();
        for name in &in_names {
            let view = match name.as_str() {
                "input_ids" => input_ids.view(),
                "attention_mask" => attn_mask.view(),
                "token_type_ids" => tok_types.view(),
                other => {
                    return Err(EmbedError::Decode(format!(
                        "unexpected model input {other}"
                    )))
                }
            };
            let tensor = ort::value::TensorRef::from_array_view(view)?;
            inputs.push((name.as_str(), tensor.into()));
        }
        let outputs = self.session.run(inputs)?;
        let first_pair = outputs
            .iter()
            .next()
            .ok_or_else(|| EmbedError::Decode("no output tensor".into()))?;
        let first = first_pair.1.try_extract_array::<f32>()?;
        // last_hidden_state: [batch, seq_len, dim]. The model card's pooling
        // config is CLS, then L2-normalize.
        let view: ArrayView2<f32> = match first.view().shape().len() {
            3 => {
                // first token of each row (CLS)
                let s = first.view();
                let (_b, _l, d) = (s.shape()[0], s.shape()[1], s.shape()[2]);
                if d != EMBED_DIM {
                    return Err(EmbedError::Decode(format!(
                        "unexpected embed dim {d} (want {EMBED_DIM})"
                    )));
                }
                let owned = s
                    .slice(ndarray::s![.., 0, ..])
                    .to_owned();
                return Ok(l2_normalize_rows(owned));
            }
            2 => first
                .view()
                .into_dimensionality::<ndarray::Ix2>()
                .map_err(|e| EmbedError::Decode(format!("shape: {e}")))?,
            _ => {
                return Err(EmbedError::Decode(format!(
                    "unexpected output shape {:?}",
                    first.view().shape()
                )))
            }
        };
        Ok(l2_normalize_rows(view.to_owned()))
    }
}

fn l2_normalize_rows(mut m: Array2<f32>) -> Vec<Vec<f32>> {
    let rows = m.nrows();
    for r in 0..rows {
        let mut row = m.row_mut(r);
        let norm = row.iter().map(|v| v * v).sum::<f32>().sqrt().max(1e-12);
        for v in row.iter_mut() {
            *v /= norm;
        }
    }
    (0..rows).map(|r| m.row(r).to_vec()).collect()
}

pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() {
        return 0.0;
    }
    a.iter().zip(b.iter()).map(|(x, y)| x * y).sum()
}

// ---------------------------------------------------------------------------
// Chunking: splits a rendered transcript.md into paragraph-sized chunks;
// each chunk keeps its leading [hh:mm:ss] timestamp.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Chunk {
    pub text: String,
    /// Milliseconds into the meeting, parsed from the leading [hh:mm:ss]
    /// prefix on the first line of the chunk. None for chunks without one
    /// (e.g. headers in transcript.md).
    pub start_ms: Option<u32>,
}

/// Units per embedding chunk: a whitespace-delimited word is two, a grapheme
/// of a script written without spaces is one.
const UNIT_WINDOW: usize = 600;
/// Units shared between consecutive windows.
const UNIT_OVERLAP: usize = 100;

pub fn chunk_transcript_md(md: &str) -> Vec<Chunk> {
    let mut out = Vec::new();
    for para in md.split("\n\n") {
        let trimmed = para.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.starts_with('#') {
            // Skip transcript headers / chunk markers.
            continue;
        }
        // Skip very short lines.
        if trimmed.chars().filter(|c| c.is_alphanumeric()).count() < 12 {
            continue;
        }
        let start_ms = parse_leading_timestamp(trimmed);
        out.extend(split_long_paragraph(trimmed, start_ms));
    }
    out
}

/// Scripts written without spaces between words; text in these is windowed
/// by grapheme.
fn is_unsegmented(c: char) -> bool {
    matches!(
        c as u32,
        0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF | 0x20000..=0x2A6DF
        | 0x3000..=0x303F | 0x3040..=0x309F | 0x30A0..=0x30FF | 0x31F0..=0x31FF | 0xFF00..=0xFFEF
        | 0x0E00..=0x0E7F | 0x0E80..=0x0EFF | 0x1000..=0x109F | 0x1780..=0x17FF
    )
}

/// One piece of a paragraph: a word where the script writes them, a grapheme
/// where it does not, and whether whitespace came before it in the source.
struct Unit<'a> {
    text: &'a str,
    cost: usize,
    spaced: bool,
}

fn units(text: &str) -> Vec<Unit<'_>> {
    let mut out = Vec::new();
    for word in text.split_whitespace() {
        let mut spaced = !out.is_empty();
        let mut run: Option<usize> = None;
        for (i, g) in word.grapheme_indices(true) {
            let dense = g.chars().next().is_some_and(is_unsegmented);
            if !dense {
                run.get_or_insert(i);
                continue;
            }
            if let Some(s) = run.take() {
                out.push(Unit { text: &word[s..i], cost: 2, spaced });
                spaced = false;
            }
            out.push(Unit { text: g, cost: 1, spaced });
            spaced = false;
        }
        if let Some(s) = run {
            out.push(Unit { text: &word[s..], cost: 2, spaced });
        }
    }
    out
}

fn render(units: &[Unit]) -> String {
    let mut s = String::new();
    for (i, u) in units.iter().enumerate() {
        if i > 0 && u.spaced {
            s.push(' ');
        }
        s.push_str(u.text);
    }
    s
}

/// Split a paragraph into overlapping windows of at most `UNIT_WINDOW`
/// units. Short paragraphs pass through unchanged. All windows of a
/// paragraph share its start timestamp.
fn split_long_paragraph(text: &str, start_ms: Option<u32>) -> Vec<Chunk> {
    let units = units(text);
    if units.iter().map(|u| u.cost).sum::<usize>() <= UNIT_WINDOW {
        return vec![Chunk { text: text.to_string(), start_ms }];
    }
    let mut out = Vec::new();
    let mut start = 0;
    loop {
        let mut end = start;
        let mut cost = 0;
        while end < units.len() && cost + units[end].cost <= UNIT_WINDOW {
            cost += units[end].cost;
            end += 1;
        }
        out.push(Chunk { text: render(&units[start..end]), start_ms });
        if end == units.len() {
            break;
        }
        let mut next = end;
        let mut back = 0;
        while next > start + 1 && back + units[next - 1].cost <= UNIT_OVERLAP {
            back += units[next - 1].cost;
            next -= 1;
        }
        start = next;
    }
    out
}

fn parse_leading_timestamp(s: &str) -> Option<u32> {
    let s = s.strip_prefix('[')?;
    let end = s.find(']')?;
    let ts = &s[..end];
    let parts: Vec<&str> = ts.split(':').collect();
    if parts.len() != 3 {
        return None;
    }
    let h: u32 = parts[0].parse().ok()?;
    let m: u32 = parts[1].parse().ok()?;
    let sec: u32 = parts[2].parse().ok()?;
    Some(((h * 3600) + (m * 60) + sec) * 1000)
}

// ---------------------------------------------------------------------------
// Per-session index: chunks.json side-car + embeddings.bin packed f32.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionIndex {
    pub schema_version: u32,
    pub session_id: String,
    pub model_id: String,
    pub generated_at_unix_seconds: i64,
    /// Hash of the transcript.md the index was built from, followed by the
    /// session's notes.json when there is one; a rewrite of either is a
    /// rebuild. The key keeps its first name on disk.
    pub transcript_hash: String,
    pub chunks: Vec<Chunk>,
}

impl SessionIndex {
    pub const SCHEMA: u32 = 2;
    pub const MODEL_ID: &'static str = "granite-embedding-97m-multilingual-r2";
}

pub fn write_session_index(
    session_dir: &Path,
    idx: &SessionIndex,
    embeddings: &[Vec<f32>],
) -> Result<()> {
    let chunks_path = session_dir.join("chunks.json");
    let emb_path = session_dir.join("embeddings.bin");

    syncsafe::write_atomic(&chunks_path, serde_json::to_vec_pretty(idx).map_err(io_err)?)?;

    let mut bytes = Vec::with_capacity(8 + embeddings.len() * EMBED_DIM * 4);
    let count = embeddings.len() as u32;
    let dim = EMBED_DIM as u32;
    bytes.extend_from_slice(&count.to_le_bytes());
    bytes.extend_from_slice(&dim.to_le_bytes());
    for v in embeddings {
        for f in v {
            bytes.extend_from_slice(&f.to_le_bytes());
        }
    }
    syncsafe::write_atomic(&emb_path, &bytes)?;
    Ok(())
}

pub fn read_session_index(session_dir: &Path) -> Result<Option<(SessionIndex, Vec<Vec<f32>>)>> {
    let chunks_path = session_dir.join("chunks.json");
    let emb_path = session_dir.join("embeddings.bin");
    if !chunks_path.is_file() || !emb_path.is_file() {
        return Ok(None);
    }
    let idx: SessionIndex = serde_json::from_slice(&std::fs::read(&chunks_path)?)
        .map_err(|e| EmbedError::Decode(format!("chunks.json: {e}")))?;
    let bytes = std::fs::read(&emb_path)?;
    if bytes.len() < 8 {
        return Err(EmbedError::Decode("embeddings.bin too short".into()));
    }
    let count = u32::from_le_bytes(bytes[..4].try_into().unwrap()) as usize;
    let dim = u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize;
    if dim != EMBED_DIM {
        return Err(EmbedError::Decode(format!(
            "embeddings.bin dim {dim} != {EMBED_DIM}"
        )));
    }
    let expected = 8 + count * dim * 4;
    if bytes.len() != expected {
        return Err(EmbedError::Decode(format!(
            "embeddings.bin len {} != expected {}",
            bytes.len(),
            expected
        )));
    }
    let mut vecs = Vec::with_capacity(count);
    let mut cur = 8usize;
    for _ in 0..count {
        let mut v = Vec::with_capacity(dim);
        for _ in 0..dim {
            let f = f32::from_le_bytes(bytes[cur..cur + 4].try_into().unwrap());
            v.push(f);
            cur += 4;
        }
        vecs.push(v);
    }
    Ok(Some((idx, vecs)))
}

/// The stamp an index carries for what it was built from: the transcript
/// followed by the notes, so a session without notes keeps the stamp it
/// had and a note edit moves it.
pub fn source_hash(transcript_md: &str, notes: Option<&[u8]>) -> String {
    // FNV-1a over the bytes plus the length; not cryptographic.
    let mut h: u64 = 1469598103934665603;
    let notes = notes.unwrap_or_default();
    for b in transcript_md.as_bytes().iter().chain(notes) {
        h ^= *b as u64;
        h = h.wrapping_mul(1099511628211);
    }
    format!("fnv64:{:016x}:{}", h, transcript_md.len() + notes.len())
}

fn io_err(e: serde_json::Error) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_source_hash_moves_with_the_notes_and_only_then() {
        let plain = source_hash("[00:00:01] hello", None);
        assert_eq!(plain, source_hash("[00:00:01] hello", Some(b"")), "no notes and an empty file stamp alike");
        assert_ne!(plain, source_hash("[00:00:01] hello", Some(br#"{"runs":[]}"#)));
        assert_ne!(source_hash("[00:00:01] hello", Some(b"a")), source_hash("[00:00:01] hello", Some(b"b")));
    }

    /// Runs only where the model directory is staged (`download-embeddings.sh`).
    #[test]
    fn tokenizer_truncates_to_budget_and_keeps_special_tokens() {
        let tok_path = model_dir().join("tokenizer.json");
        if !tok_path.is_file() {
            return;
        }
        let mut tokenizer = Tokenizer::from_file(&tok_path).unwrap();
        tokenizer
            .with_truncation(Some(tokenizers::TruncationParams {
                max_length: MAX_TOKENS,
                ..Default::default()
            }))
            .unwrap();
        let short = tokenizer.encode("a short line", true).unwrap();
        let long_text = "회의에서 논의된 안건은 다음과 같습니다 ".repeat(600);
        let long = tokenizer.encode(long_text.as_str(), true).unwrap();
        assert_eq!(long.get_ids().len(), MAX_TOKENS);
        assert_eq!(long.get_ids().first(), short.get_ids().first());
        assert_eq!(long.get_ids().last(), short.get_ids().last());
    }

    #[test]
    fn parse_ts_basic() {
        assert_eq!(parse_leading_timestamp("[00:01:23] hello"), Some(83000));
        assert_eq!(parse_leading_timestamp("[01:00:00] hi"), Some(3_600_000));
        assert_eq!(parse_leading_timestamp("no ts"), None);
    }

    #[test]
    fn chunker_skips_headers() {
        let md = "# Transcript\n\n[00:00:01] **Me**: A real line of speech with enough words.\n\n## Chunk 1\n\n[00:00:02] **Them**: Another real line that has substance.";
        let chunks = chunk_transcript_md(md);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].start_ms, Some(1000));
        assert_eq!(chunks[1].start_ms, Some(2000));
    }

    #[test]
    fn short_paragraph_not_split() {
        let c = split_long_paragraph("just a few words here", Some(5));
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].text, "just a few words here");
    }

    #[test]
    fn long_paragraph_splits_with_overlap_and_full_coverage() {
        // 700 words → windows of 300 with 50 overlap (step 250): [0..300],
        // [250..550], [500..700] = 3 windows.
        let words: Vec<String> = (0..700).map(|i| format!("w{i}")).collect();
        let para = words.join(" ");
        let c = split_long_paragraph(&para, Some(1234));
        assert_eq!(c.len(), 3);
        assert!(c.iter().all(|ch| ch.start_ms == Some(1234)));
        assert!(c.iter().all(|ch| ch.text.split_whitespace().count() <= 300));
        // Coverage: the union of all windows contains every original word.
        let joined: String = c.iter().map(|ch| ch.text.clone()).collect::<Vec<_>>().join(" ");
        assert!(joined.contains("w0") && joined.contains("w699"));
        // Overlap: window 2 starts at word 250 and repeats "w250".
        assert!(c[1].text.starts_with("w250 "));
    }

    #[test]
    fn unsegmented_paragraph_splits_by_grapheme_with_overlap() {
        // 60 × 26 characters = 1560, no spaces: windows of 600 with 100
        // overlap (step 500) = [0..600], [500..1100], [1000..1560].
        let sentence = "今日の会議では新しい設計の予算と日程を確認しました。";
        let para = sentence.repeat(60);
        let chars: Vec<char> = para.chars().collect();
        let c = split_long_paragraph(&para, Some(7));
        assert_eq!(c.len(), 3);
        assert!(c.iter().all(|ch| ch.start_ms == Some(7)));
        assert!(c.iter().all(|ch| ch.text.chars().count() <= 600));
        assert!(c.iter().all(|ch| para.contains(&ch.text)));
        assert_eq!(c[0].text, chars[..600].iter().collect::<String>());
        assert_eq!(c[1].text, chars[500..1100].iter().collect::<String>());
        assert_eq!(c[2].text, chars[1000..].iter().collect::<String>());
    }

    #[test]
    fn mixed_script_windows_keep_the_source_spacing() {
        // A spaced word costs two units, a character of an unsegmented
        // script one; either way every window is a verbatim slice.
        let para = "PAD の見積もりを確認する ".repeat(120).trim_end().to_string();
        let c = split_long_paragraph(&para, None);
        assert!(c.len() > 1);
        assert!(c.iter().all(|ch| para.contains(&ch.text)));
        assert!(c.iter().all(|ch| !ch.text.contains("  ")));
        assert!(c.iter().all(|ch| !ch.text.starts_with(' ') && !ch.text.ends_with(' ')));
    }

    #[test]
    fn thai_windows_never_open_on_a_combining_mark() {
        let para = "วันนี้เราคุยเรื่องงบประมาณและกำหนดการของโครงการใหม่".repeat(40);
        let c = split_long_paragraph(&para, None);
        assert!(c.len() > 1);
        let combining = |ch: char| matches!(ch as u32, 0x0E31 | 0x0E34..=0x0E3A | 0x0E47..=0x0E4E);
        assert!(c.iter().all(|ch| !combining(ch.text.chars().next().unwrap())));
        assert!(c.iter().all(|ch| para.contains(&ch.text)));
    }
}
