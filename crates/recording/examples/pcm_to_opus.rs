// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Encodes raw interleaved 16-bit PCM at 16 kHz into the Ogg Opus layout a
//! finished recording carries: left channel the local mic, right channel
//! everyone else.
//!
//!     cargo run -q -p recording --example pcm_to_opus -- in.raw out.opus

use std::path::Path;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (input, output) = match args.as_slice() {
        [_, i, o] => (i, o),
        _ => {
            eprintln!("usage: pcm_to_opus <in.raw> <out.opus>");
            std::process::exit(2);
        }
    };
    let raw = std::fs::read(input).unwrap_or_else(|e| panic!("read {input}: {e}"));
    let samples: Vec<i16> = raw
        .chunks_exact(2)
        .map(|c| i16::from_le_bytes([c[0], c[1]]))
        .collect();
    let params = recording::compress::CompressParams::default();
    let written =
        recording::compress::encode_stereo_pcm16(&samples, 16_000, &params, Path::new(output))
            .unwrap_or_else(|e| panic!("encode {output}: {e}"));
    println!("{written} bytes -> {output}");
}
