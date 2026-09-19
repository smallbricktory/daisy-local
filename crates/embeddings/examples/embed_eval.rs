// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! embed_eval: dump chunk embeddings for every session under a profile, or
//! embed a query list, using whatever model `DAISY_EMBED_DIR` points at.
//! Pairs with scripts/embeddings-export/drift.py to verify a re-exported
//! model against the fp32 reference.
//!
//!   cargo run -p embeddings --release --example embed_eval -- index   <sessions_root> <out.json> [batch]
//!   cargo run -p embeddings --release --example embed_eval -- queries <queries.json>  <out.json>
//!   cargo run -p embeddings --release --example embed_eval -- inputs  <model.onnx>
use embeddings::{chunk_transcript_md, Encoder};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::Instant;

#[derive(Serialize)]
struct SessionOut {
    session_id: String,
    chunks: Vec<String>,
    start_ms: Vec<Option<u32>>,
    vecs: Vec<Vec<f32>>,
    embed_ms: f64,
}

#[derive(Serialize)]
struct IndexOut {
    model_dir: String,
    batch: usize,
    sessions: Vec<SessionOut>,
    total_chunks: usize,
    total_embed_ms: f64,
    total_words: usize,
}

#[derive(Deserialize)]
struct QueryIn {
    id: String,
    q: String,
    #[serde(default)]
    session: Option<String>,
    #[serde(default)]
    needle: Option<serde_json::Value>,
}

#[derive(Serialize)]
struct QueryOut {
    id: String,
    q: String,
    session: Option<String>,
    needle: Option<serde_json::Value>,
    vec: Vec<f32>,
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: embed-eval index <sessions_root> <out.json> [batch] | queries <queries.json> <out.json>");
        std::process::exit(2);
    }
    if args[1] == "inputs" {
        let sess = ort::session::Session::builder().unwrap().commit_from_file(&args[2]).unwrap();
        for i in sess.inputs() { println!("input  {} {:?}", i.name(), i.dtype()); }
        for o in sess.outputs() { println!("output {} {:?}", o.name(), o.dtype()); }
        return;
    }
    let t0 = Instant::now();
    let mut enc = Encoder::load().expect("load encoder");
    eprintln!("model {} loaded in {:.0} ms", embeddings::model_dir().display(), t0.elapsed().as_secs_f64() * 1e3);
    // warm-up
    let _ = enc.encode("warm up the session once").expect("warmup");

    match args[1].as_str() {
        "index" => index(&mut enc, Path::new(&args[2]), &args[3], args.get(4).and_then(|b| b.parse().ok()).unwrap_or(16)),
        "queries" => queries(&mut enc, &args[2], &args[3]),
        other => {
            eprintln!("unknown mode {other}");
            std::process::exit(2);
        }
    }
}

fn index(enc: &mut Encoder, root: &Path, out: &str, batch: usize) {
    let mut sessions = Vec::new();
    let mut total_chunks = 0;
    let mut total_embed_ms = 0.0;
    let mut total_words = 0;
    let mut dirs: Vec<_> = std::fs::read_dir(root).expect("read root").flatten().map(|e| e.path()).collect();
    dirs.sort();
    for d in dirs {
        let t = d.join("transcript.md");
        if !t.is_file() {
            continue;
        }
        let md = std::fs::read_to_string(&t).expect("read transcript");
        total_words += md.split_whitespace().count();
        let chunks = chunk_transcript_md(&md);
        if chunks.is_empty() {
            continue;
        }
        let texts: Vec<&str> = chunks.iter().map(|c| c.text.as_str()).collect();
        let t1 = Instant::now();
        let mut vecs = Vec::with_capacity(texts.len());
        for b in texts.chunks(batch) {
            vecs.extend(enc.encode_batch(b).expect("encode batch"));
        }
        let ms = t1.elapsed().as_secs_f64() * 1e3;
        let sid = d.file_name().unwrap().to_string_lossy().to_string();
        eprintln!("{sid}: {} chunks in {:.0} ms ({:.1} ms/chunk)", chunks.len(), ms, ms / chunks.len() as f64);
        total_chunks += chunks.len();
        total_embed_ms += ms;
        sessions.push(SessionOut {
            session_id: sid,
            chunks: chunks.iter().map(|c| c.text.clone()).collect(),
            start_ms: chunks.iter().map(|c| c.start_ms).collect(),
            vecs,
            embed_ms: ms,
        });
    }
    eprintln!("TOTAL {total_chunks} chunks, {total_words} words, {:.0} ms ({:.1} ms/chunk)", total_embed_ms, total_embed_ms / total_chunks.max(1) as f64);
    let o = IndexOut {
        model_dir: embeddings::model_dir().display().to_string(),
        batch,
        sessions,
        total_chunks,
        total_embed_ms,
        total_words,
    };
    std::fs::write(out, serde_json::to_vec(&o).unwrap()).expect("write out");
}

fn queries(enc: &mut Encoder, qfile: &str, out: &str) {
    let qs: Vec<QueryIn> = serde_json::from_slice(&std::fs::read(qfile).expect("read queries")).expect("parse queries");
    let mut res = Vec::with_capacity(qs.len());
    let t1 = Instant::now();
    for q in qs {
        let vec = enc.encode(&q.q).expect("encode query");
        res.push(QueryOut { id: q.id, q: q.q, session: q.session, needle: q.needle, vec });
    }
    eprintln!("{} queries in {:.0} ms", res.len(), t1.elapsed().as_secs_f64() * 1e3);
    std::fs::write(out, serde_json::to_vec(&res).unwrap()).expect("write out");
}
