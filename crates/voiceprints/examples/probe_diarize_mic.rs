// Replicates tauri-app's collect_track_embeddings gates for the Mic scope on
// a session dir, then clusters, reporting per-gate segment counts.
use serde_json::Value;
use std::path::PathBuf;

fn main() {
    let root = PathBuf::from(std::env::args().nth(1).unwrap());
    let st: Value = serde_json::from_slice(
        &std::fs::read(root.join("transcript.dedup.json")).unwrap()).unwrap();
    let mf: Value = serde_json::from_slice(
        &std::fs::read(root.join("manifest.json")).unwrap()).unwrap();
    let mut enc = voiceprints::Encoder::load().unwrap();

    let (mut total, mut too_short, mut no_wav, mut read_err, mut tiny, mut enc_err) =
        (0, 0, 0, 0, 0, 0);
    let mut embs: Vec<Vec<f32>> = Vec::new();

    for ch in st["chunks"].as_array().unwrap() {
        let idx = ch["chunk_index"].as_u64().unwrap();
        let cm = mf["chunks"].as_array().unwrap().iter()
            .find(|c| c["index"].as_u64() == Some(idx)).unwrap();
        // wav_path Mic arm, including the mic_dn preference
        let wav = ["mic_dn_wav_relative", "mic_aec_wav_relative"].iter()
            .filter_map(|k| cm[*k].as_str())
            .map(|r| root.join(r))
            .find(|p| p.is_file())
            .unwrap_or_else(|| root.join(cm["mic_wav_relative"].as_str().unwrap()));
        for tr in ch["tracks"].as_array().unwrap() {
            let t = tr["track"].as_str().unwrap();
            if t != "mic" && t != "mic_aec" { continue; }
            for seg in tr["segments"].as_array().unwrap() {
                total += 1;
                let (s, e) = (seg["start_ms"].as_u64().unwrap() as u32,
                              seg["end_ms"].as_u64().unwrap() as u32);
                let dur = e.saturating_sub(s);
                if dur < 1200 { too_short += 1; continue; }
                if !wav.is_file() { no_wav += 1; continue; }
                let cap = dur.min(8000);
                let Ok(pcm) = voiceprints::read_pcm_window(&wav, s, s + cap, cap) else {
                    read_err += 1; continue;
                };
                if pcm.len() < 8000 { tiny += 1; continue; }
                match enc.encode_pcm(&pcm) {
                    Ok(v) => embs.push(v),
                    Err(e) => { enc_err += 1; if enc_err <= 2 { eprintln!("enc err: {e}"); } }
                }
            }
        }
    }
    println!("total={total} too_short={too_short} no_wav={no_wav} read_err={read_err} tiny={tiny} enc_err={enc_err} embedded={}", embs.len());
    if !embs.is_empty() {
        let ids = voiceprints::cluster_speakers(&embs, None);
        let k = ids.iter().copied().max().map(|m| m + 1).unwrap_or(0);
        println!("clusters={k}");
    }
}
