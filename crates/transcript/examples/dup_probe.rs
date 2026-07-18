//! Temporary probe: quantify cross-track text duplication in a session's
//! live_transcript.jsonl vs what pooled_duplication_rate reports.
use transcript::promote::{pooled_duplication_rate, words_contained_ratio_pooled, LiveSeg};

fn main() {
    let dir = std::path::PathBuf::from(std::env::args().nth(1).expect("usage: dup_probe <session-dir>"));
    let segs: Vec<LiveSeg> = std::fs::read_to_string(dir.join("live_transcript.jsonl"))
        .unwrap()
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter(|v| v["final"].as_bool() == Some(true))
        .map(|v| LiveSeg {
            is_system: v["track"] == "system",
            start_ms: v["start_ms"].as_u64().unwrap() as u32,
            end_ms: v["end_ms"].as_u64().unwrap() as u32,
            text: v["text"].as_str().unwrap().to_string(),
        })
        .collect();
    let mic = segs.iter().filter(|s| !s.is_system).count();
    println!("finals: {} ({} mic / {} system)", segs.len(), mic, segs.len() - mic);
    println!("pooled_duplication_rate (legacy detector): {:.3}", pooled_duplication_rate(&segs));
    let rate = transcript::promote::promotion_bleed_rate(&segs);
    println!(
        "promotion_bleed_rate: {:.3} -> {}",
        rate,
        if rate >= transcript::promote::PROMOTE_BLEED_MAX_RATE { "FULL PASS (gate trips)" } else { "promote" }
    );

    // Order-free containment vs a ±20s system pool, bucketed.
    let mut buckets = [0usize; 5]; // <0.25, <0.5, <0.75, <0.9, >=0.9
    let mut eligible = 0usize;
    for s in &segs {
        if s.is_system {
            continue;
        }
        let Some(r) = words_contained_ratio_pooled(s, &segs, 20_000) else { continue };
        eligible += 1;
        let b = if r < 0.25 { 0 } else if r < 0.5 { 1 } else if r < 0.75 { 2 } else if r < 0.9 { 3 } else { 4 };
        buckets[b] += 1;
    }
    println!("mic finals vs ±20s system pool (order-free containment), n={eligible}:");
    for (label, n) in ["<0.25", "0.25-0.5", "0.5-0.75", "0.75-0.9", ">=0.9"].iter().zip(buckets) {
        println!("  {label:>8}: {n}");
    }
}
