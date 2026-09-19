// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Daisy's on-device LLM worker. Spawned by the app as a sidecar; one model,
//! JSON lines on stdin/stdout, timings on stderr.
//!
//!   daisy-llm --probe                 → one Probe JSON line
//!   daisy-llm --model <gguf> --ctx N  → serve requests until stdin closes

mod engine;
mod probe;

use std::io::Write;

fn main() {
    if let Err(e) = run() {
        eprintln!("daisy-llm: {e:#}");
        std::process::exit(2);
    }
}

fn run() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("--probe") => {
            let p = probe::run()?;
            let mut out = std::io::stdout().lock();
            serde_json::to_writer(&mut out, &p)?;
            out.write_all(b"\n")?;
            Ok(())
        }
        Some("--model") => {
            let path = args.get(1).ok_or_else(|| anyhow::anyhow!("--model needs a path"))?;
            let ctx = match (args.get(2).map(String::as_str), args.get(3)) {
                (Some("--ctx"), Some(n)) => n.parse::<u32>()?,
                _ => anyhow::bail!("usage: daisy-llm --model <gguf> --ctx <n>"),
            };
            engine::serve(std::path::Path::new(path), ctx)
        }
        _ => anyhow::bail!("usage: daisy-llm --probe | --model <gguf> --ctx <n>"),
    }
}
