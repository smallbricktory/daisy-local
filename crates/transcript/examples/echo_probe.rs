//! Temporary probe: run the directional bleed filter against a real session
//! dir (live_transcript.jsonl + chunk WAVs) and print what changes vs legacy.
use transcript::echo_direction::WavOracle;
use transcript::model::Track;
use transcript::promote::{filter_bleed_directional, filter_mic_bleed, ChunkSpan, LiveSeg};

fn main() {
    let dir = std::path::PathBuf::from(std::env::args().nth(1).expect("usage: echo_probe <session-dir>"));
    let manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("manifest.json")).unwrap()).unwrap();
    let t0 = manifest["created_at_unix_seconds"].as_i64().unwrap();
    let spans: Vec<ChunkSpan> = manifest["chunks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            let aec = c["mic_aec_wav_relative"].as_str();
            ChunkSpan {
                index: c["index"].as_u64().unwrap() as u32,
                start_ms: ((c["started_at_unix_seconds"].as_i64().unwrap() - t0) * 1000) as u32,
                mic_track: if aec.is_some() { Track::MicAec } else { Track::Mic },
                mic_wav: dir.join(aec.or(c["mic_wav_relative"].as_str()).unwrap()),
                system_wav: dir.join(c["system_wav_relative"].as_str().unwrap()),
            }
        })
        .collect();
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

    let legacy = filter_mic_bleed(&segs);
    let oracle = WavOracle::new(&spans);
    let directional = filter_bleed_directional(&segs, &|m, s| oracle.direction(m, s));

    let key = |s: &LiveSeg| (s.is_system, s.start_ms, s.text.clone());
    let lset: std::collections::HashSet<_> = legacy.iter().map(key).collect();
    let dset: std::collections::HashSet<_> = directional.iter().map(key).collect();
    // --dump: emit raw + directional-kept as JSONL for offline analysis.
    if std::env::args().nth(2).as_deref() == Some("--dump") {
        for s in &segs {
            let kept = dset.contains(&key(s));
            println!(
                "{}",
                serde_json::json!({ "is_system": s.is_system, "start_ms": s.start_ms, "end_ms": s.end_ms, "text": s.text, "kept": kept })
            );
        }
        return;
    }
    println!(
        "segs={} legacy_kept={} directional_kept={} bleed_coverage={:.2}",
        segs.len(),
        legacy.len(),
        directional.len(),
        transcript::echo_direction::bleed_coverage(&spans, 12)
    );
    for s in &directional {
        if !lset.contains(&key(s)) {
            println!("RESCUED {} @{}s: {}", if s.is_system { "sys" } else { "mic" }, s.start_ms / 1000, &s.text[..s.text.len().min(60)]);
        }
    }
    for s in &legacy {
        if !dset.contains(&key(s)) {
            println!("NEWLY-DROPPED {} @{}s: {}", if s.is_system { "sys" } else { "mic" }, s.start_ms / 1000, &s.text[..s.text.len().min(60)]);
        }
    }
}
