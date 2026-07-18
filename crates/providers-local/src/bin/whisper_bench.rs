//! Whisper performance harness.
//!
//!   whisper_bench <model.bin> <wav-16k-mono>
//!
//! Env:
//!   BENCH_THREADS  comma list of thread counts to sweep (default "1,2,4,8")
//!   BENCH_LANG     language hint (default "en"; empty = auto-detect)
//!
//! Measures:
//!   1. FULL-FILE xRT per thread count — the batch path.
//!   2. WINDOW-LENGTH SWEEP           — decode 1s/5s/10s/15s/30s slices.
//!   3. CONCURRENT 2-TRACK            — two decodes at once (mic+system).
//!   Derived numbers (5s-window xRT, streaming latency/keep-up) are printed
//!   from D = single 30s-window wall.

use std::path::Path;
use std::time::Instant;

use providers_local::wav::decode_wav_16k_mono_f32;

const SR: usize = 16_000;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: whisper_bench <model.bin> <wav-16k-mono>");
        std::process::exit(2);
    }
    let model_path = &args[1];
    let wav_path = &args[2];

    let threads: Vec<i32> = std::env::var("BENCH_THREADS")
        .unwrap_or_else(|_| "1,2,4,8".into())
        .split(',')
        .filter_map(|s| s.trim().parse().ok())
        .collect();
    let lang = std::env::var("BENCH_LANG").unwrap_or_else(|_| "en".into());
    let lang_hint: Option<&str> = if lang.is_empty() { None } else { Some(&lang) };

    let nproc = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(0);
    let backend = providers_local::bench::backend_label();

    println!("================ whisper_bench ================");
    println!("host threads (nproc): {nproc}");
    println!("backend:              {backend}");
    println!("model:                {model_path}");
    println!("wav:                  {wav_path}");
    println!("lang hint:            {:?}", lang_hint);

    // Decode WAV once.
    let samples = match decode_wav_16k_mono_f32(Path::new(wav_path)) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("wav decode failed: {e}");
            std::process::exit(1);
        }
    };
    let audio_secs = samples.len() as f64 / SR as f64;
    println!("audio length:         {:.1}s ({} samples)", audio_secs, samples.len());

    // Load model once; time it separately (not part of decode timing).
    let t = Instant::now();
    let ctx = match whisper_rs::WhisperContext::new_with_params(
        model_path,
        whisper_rs::WhisperContextParameters::default(),
    ) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("model load failed: {e}");
            std::process::exit(1);
        }
    };
    println!("model load:           {:.2}s", t.elapsed().as_secs_f64());
    println!();

    // ── 1. FULL-FILE xRT per thread count ───────────────────────────────
    println!("--- [1] FULL-FILE decode (batch path ≈ finalize) ---");
    println!("{:>8} {:>10} {:>8}", "threads", "wall_s", "xRT");
    let mut full_wall_at = std::collections::BTreeMap::new();
    let mut best_n = threads.first().copied().unwrap_or(4);
    let mut best_xrt = 0.0f64;
    for &n in &threads {
        let (wall, _segs) = decode(&ctx, &samples, n, lang_hint);
        let xrt = audio_secs / wall;
        full_wall_at.insert(n, wall);
        if xrt > best_xrt {
            best_xrt = xrt;
            best_n = n;
        }
        println!("{n:>8} {wall:>10.2} {xrt:>7.2}x");
    }
    println!("  → best: {best_n} threads @ {best_xrt:.1}x (more threads can COLLAPSE via E-core/HT thrash)");
    println!();

    // ── 2. WINDOW-LENGTH SWEEP ───────────────────────────────────────────
    // Uses the optimal thread count from [1], not the max.
    let bench_threads = best_n;
    println!("--- [2] WINDOW-LENGTH SWEEP @ {bench_threads} threads ---");
    println!("(flat wall across lengths ⇒ fixed 30s encoder cost ⇒ short windows waste compute)");
    println!("{:>10} {:>10} {:>8} {:>14}", "window_s", "wall_s", "xRT", "wall/30s_call");
    let reps = 3usize;
    let mid = samples.len() / 4; // start a quarter in
    let mut wall_30s = None;
    let mut wall_5s = None;
    for win_s in [1usize, 5, 10, 15, 30] {
        let want = win_s * SR;
        let mut walls = Vec::new();
        for r in 0..reps {
            let start = (mid + r * SR).min(samples.len().saturating_sub(want.min(samples.len())));
            let end = (start + want).min(samples.len());
            if end <= start {
                continue;
            }
            let slice = &samples[start..end];
            let (wall, _) = decode(&ctx, slice, bench_threads, lang_hint);
            walls.push(wall);
        }
        if walls.is_empty() {
            continue;
        }
        let avg = walls.iter().sum::<f64>() / walls.len() as f64;
        let xrt = win_s as f64 / avg;
        if win_s == 30 {
            wall_30s = Some(avg);
        }
        if win_s == 5 {
            wall_5s = Some(avg);
        }
        println!("{win_s:>10} {avg:>10.2} {xrt:>7.2}x {:>13.0}%", 100.0 * (win_s as f64 / 30.0));
    }
    println!();

    // ── 3. CONCURRENT 2-TRACK (mic + system at once) ────────────────────
    // The optimal thread budget is split across the two tracks (best_n/2
    // each); total threads = best_n.
    let per_track = (best_n / 2).max(1);
    println!("--- [3] CONCURRENT 2-TRACK @ {per_track} threads each (= {best_n} total) ---");
    let bench_threads = per_track;
    let single = full_wall_at.get(&bench_threads).copied().unwrap_or_else(|| {
        let (w, _) = decode(&ctx, &samples, bench_threads, lang_hint);
        w
    });
    // Two full-file decodes in parallel, each thread with its own context.
    let t = Instant::now();
    std::thread::scope(|s| {
        let smp = &samples;
        let mp = model_path.clone();
        let lh = lang_hint.map(|x| x.to_string());
        s.spawn(|| {
            let _ = decode(&ctx, smp, bench_threads, lang_hint);
        });
        s.spawn(move || {
            if let Ok(ctx2) = whisper_rs::WhisperContext::new_with_params(
                &mp,
                whisper_rs::WhisperContextParameters::default(),
            ) {
                let lh2 = lh.as_deref();
                let _ = decode(&ctx2, smp, bench_threads, lh2);
            }
        });
    });
    let concurrent_wall = t.elapsed().as_secs_f64();
    let slowdown = concurrent_wall / single;
    println!("single-track wall:    {single:.2}s");
    println!("2-track parallel wall: {concurrent_wall:.2}s  ({slowdown:.2}x the single-track wall)");
    println!("(1.0x = perfect scaling/no contention; 2.0x = fully serialized)");
    println!();

    // ── Derived numbers ──────────────────────────────────────────────────
    if let (Some(d30), Some(d5)) = (wall_30s, wall_5s) {
        println!("--- DERIVED @ {best_n} threads (measured: 5s-window={d5:.2}s, 30s-window={d30:.2}s) ---");
        // One 5s-window decode per 5s of audio (single active track).
        let broken_xrt = 5.0 / d5;
        println!(
            "current 5s-window design, 1 active track:  {broken_xrt:.2}x realtime  ({})",
            if broken_xrt >= 1.0 { "KEEPS UP on this CPU" } else { "DROPS audio on this CPU" }
        );
        // With both tracks talking: apply the measured 2-track contention factor.
        let two_track_xrt = broken_xrt / (concurrent_wall / single).max(1.0);
        println!(
            "current 5s-window design, 2 active tracks: {two_track_xrt:.2}x realtime  ({})",
            if two_track_xrt >= 1.0 { "KEEPS UP" } else { "DROPS audio (cross-talk)" }
        );
        println!("streaming-whisper latency floor:           ~{d30:.1}s behind live (= one 30s decode)");
        println!("per-second efficiency, 5s vs 30s window:   {:.0}% (lower = more wasted compute on short windows)",
            100.0 * ((5.0 / d5) / (30.0 / d30)));
    }
    println!("================ done ================");
}

/// Decode `samples` with `n_threads`; return (wall_seconds, segment_count).
fn decode(
    ctx: &whisper_rs::WhisperContext,
    samples: &[f32],
    n_threads: i32,
    lang_hint: Option<&str>,
) -> (f64, i32) {
    let mut state = ctx.create_state().expect("create_state");
    let mut params =
        whisper_rs::FullParams::new(whisper_rs::SamplingStrategy::Greedy { best_of: 1 });
    params.set_n_threads(n_threads);
    params.set_translate(false);
    params.set_print_special(false);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);
    params.set_language(lang_hint);
    let t = Instant::now();
    state.full(params, samples).expect("whisper full");
    let wall = t.elapsed().as_secs_f64();
    (wall, state.full_n_segments())
}
