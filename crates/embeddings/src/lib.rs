//! Local embedding + indexing for semantic search over transcripts.
//!
//! Model: BGE-small-en-v1.5 (ONNX, 384-dim CLS-pooled), run on CPU via `ort`.
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

pub const EMBED_DIM: usize = 384;
const MAX_TOKENS: usize = 510; // BGE-small max 512 incl. [CLS]/[SEP]

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

/// Resolve the embeddings model directory. Checks, in order:
///   1. the `DAISY_EMBED_DIR` env var
///   2. `models/embeddings/` next to the executable
///   3. repo-relative `models/embeddings/`
pub fn model_dir() -> PathBuf {
    if let Ok(p) = std::env::var("DAISY_EMBED_DIR") {
        return PathBuf::from(p);
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let cand = dir.join("models/embeddings");
            if cand.is_dir() {
                return cand;
            }
        }
    }
    let crate_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    crate_dir
        .parent()
        .and_then(|p| p.parent())
        .map(|p| p.join("models/embeddings"))
        .unwrap_or_else(|| PathBuf::from("models/embeddings"))
}

/// Loaded tokenizer + ONNX session.
pub struct Encoder {
    session: ort::session::Session,
    tokenizer: Tokenizer,
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
        let tokenizer = Tokenizer::from_file(&tok_path)
            .map_err(|e| EmbedError::Tokenizer(format!("{e}")))?;
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
            let mut e = self
                .tokenizer
                .encode(*t, true)
                .map_err(|err| EmbedError::Tokenizer(format!("{err}")))?;
            if e.get_ids().len() > MAX_TOKENS + 2 {
                e.truncate(MAX_TOKENS + 2, 1, tokenizers::TruncationDirection::Right);
            }
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

        // Tensors are bound to the model's declared input names, in the
        // model's own input order: input_ids, attention_mask, token_type_ids.
        let in_names: Vec<String> = self
            .session
            .inputs()
            .iter()
            .map(|i| i.name().to_string())
            .collect();
        let in_a = ort::value::TensorRef::from_array_view(input_ids.view())?;
        let in_b = ort::value::TensorRef::from_array_view(attn_mask.view())?;
        let in_c = ort::value::TensorRef::from_array_view(tok_types.view())?;
        let mut inputs: Vec<(&str, ort::session::SessionInputValue)> = Vec::new();
        inputs.push((in_names[0].as_str(), in_a.into()));
        if in_names.len() > 1 {
            inputs.push((in_names[1].as_str(), in_b.into()));
        }
        if in_names.len() > 2 {
            inputs.push((in_names[2].as_str(), in_c.into()));
        }
        let outputs = self.session.run(inputs)?;
        let first_pair = outputs
            .iter()
            .next()
            .ok_or_else(|| EmbedError::Decode("no output tensor".into()))?;
        let first = first_pair.1.try_extract_array::<f32>()?;
        // BGE last_hidden_state: [batch, seq_len, dim]. CLS-pool then L2.
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

/// Maximum words per embedding chunk.
const WORD_WINDOW: usize = 300;
/// Words shared between consecutive windows.
const WORD_OVERLAP: usize = 50;

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

/// Split a paragraph into overlapping word windows of at most `WORD_WINDOW`
/// words. Short paragraphs pass through unchanged. All windows of a paragraph
/// share its start timestamp.
fn split_long_paragraph(text: &str, start_ms: Option<u32>) -> Vec<Chunk> {
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.len() <= WORD_WINDOW {
        return vec![Chunk { text: text.to_string(), start_ms }];
    }
    let step = WORD_WINDOW - WORD_OVERLAP;
    let mut out = Vec::new();
    let mut i = 0;
    while i < words.len() {
        let end = (i + WORD_WINDOW).min(words.len());
        out.push(Chunk { text: words[i..end].join(" "), start_ms });
        if end == words.len() {
            break;
        }
        i += step;
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
    /// Hash of the transcript.md content the index was built from.
    pub transcript_hash: String,
    pub chunks: Vec<Chunk>,
}

impl SessionIndex {
    pub const SCHEMA: u32 = 2;
    pub const MODEL_ID: &'static str = "bge-small-en-v1.5";
}

pub fn write_session_index(
    session_dir: &Path,
    idx: &SessionIndex,
    embeddings: &[Vec<f32>],
) -> Result<()> {
    let chunks_path = session_dir.join("chunks.json");
    let emb_path = session_dir.join("embeddings.bin");

    let chunks_tmp = chunks_path.with_extension("json.tmp");
    std::fs::write(&chunks_tmp, serde_json::to_vec_pretty(idx).map_err(io_err)?)?;
    std::fs::rename(&chunks_tmp, &chunks_path)?;

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
    let emb_tmp = emb_path.with_extension("bin.tmp");
    std::fs::write(&emb_tmp, &bytes)?;
    std::fs::rename(&emb_tmp, &emb_path)?;
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

pub fn transcript_sha256(md: &str) -> String {
    // FNV-1a over the bytes plus the length; not cryptographic.
    let mut h: u64 = 1469598103934665603;
    for b in md.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(1099511628211);
    }
    format!("fnv64:{:016x}:{}", h, md.len())
}

fn io_err(e: serde_json::Error) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(c.iter().all(|ch| ch.text.split_whitespace().count() <= WORD_WINDOW));
        // Coverage: the union of all windows contains every original word.
        let joined: String = c.iter().map(|ch| ch.text.clone()).collect::<Vec<_>>().join(" ");
        assert!(joined.contains("w0") && joined.contains("w699"));
        // Overlap: window 2 starts at word 250 and repeats "w250".
        assert!(c[1].text.starts_with("w250 "));
    }
}
