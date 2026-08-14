//! Field harness: report what finalize dedup does to a real session — spans
//! clipped to audible extent, boundary repeats trimmed, bleed drops.
//!
//!   DAISY_SESSION_DIR=…/sessions/daisy-XXXX \
//!   cargo test -p transcript --test span_clip_field -- --ignored --nocapture

use std::collections::HashMap;
use std::path::PathBuf;
use transcript::dedup::{dedup_session, DedupParams};
use transcript::SessionTranscript;

#[test]
#[ignore]
fn span_clip_field_report() {
    let dir = PathBuf::from(std::env::var("DAISY_SESSION_DIR").expect("DAISY_SESSION_DIR"));
    let bytes = std::fs::read(dir.join("transcript.json")).expect("transcript.json");
    let mut st: SessionTranscript = serde_json::from_slice(&bytes).expect("parse");
    transcript::hallucination::filter_session(&mut st);

    let result = dedup_session(&st, &dir, &DedupParams::default()).expect("dedup");
    println!(
        "dedup: dropped={} kept={} clipped_spans={}",
        result.report.dropped, result.report.kept_count, result.report.clipped_spans
    );

    // Input spans by text, to show how surviving segments moved.
    for (chunk_in, chunk_out) in st.chunks.iter().zip(&result.deduped.chunks) {
        for (tr_in, tr_out) in chunk_in.tracks.iter().zip(&chunk_out.tracks) {
            let spans_in: HashMap<&str, (u32, u32)> = tr_in
                .segments
                .iter()
                .map(|s| (s.text.as_str(), (s.start_ms, s.end_ms)))
                .collect();
            for seg in &tr_out.segments {
                let Some(&(s0, e0)) = spans_in.get(seg.text.as_str()) else {
                    println!(
                        "  {:?} boundary-trimmed: {:>7.1}-{:<7.1} {}",
                        tr_out.track,
                        seg.start_ms as f64 / 1000.0,
                        seg.end_ms as f64 / 1000.0,
                        seg.text.chars().take(60).collect::<String>()
                    );
                    continue;
                };
                if (s0, e0) != (seg.start_ms, seg.end_ms) {
                    println!(
                        "  {:?} {:>7.1}-{:<7.1} -> {:>7.1}-{:<7.1}  {}",
                        tr_out.track,
                        s0 as f64 / 1000.0,
                        e0 as f64 / 1000.0,
                        seg.start_ms as f64 / 1000.0,
                        seg.end_ms as f64 / 1000.0,
                        seg.text.chars().take(50).collect::<String>()
                    );
                }
            }
        }
    }
}
