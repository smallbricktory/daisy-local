// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! The room case's CPU cost: two Nemotron ASR live tracks at real-time pace, then the same with
//! one live-names diarizer (the system track), then with two (system + mic),
//! each `--seconds` long, in one process.
//!
//!   cargo run --release -p providers-local --example live_names_room_bench -- \
//!       --diar-dir DIR --asr-dir DIR --mic MIC.wav --system SYSTEM.wav \
//!       [--seconds S] --out FILE.json
//!
//! Each diarizer is the app's `LiveDiarizer` with the app's thread count, on
//! its own thread at background priority, fed a quarter second every 250 ms
//! as the live-names worker reads the chunk files. The system instance reads
//! `--system`, the mic instance `--mic`; both ASR tracks read both files.
//! Per phase: process CPU (percent of one core), peak RSS, ASR decode
//! p50/p90 and final lag p90, and per instance the real-time factor (step
//! compute time over audio time) and the largest backlog.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use providers_local::nemotron3_diar::LiveDiarizer;

const SR: usize = 16_000;
/// The app's live-names diarizer thread count (`live_names::worker::DIAR_THREADS`).
const DIAR_THREADS: usize = 2;
const FEED_MS: u64 = 250;

#[derive(Default, Clone)]
struct AsrStats {
    decode_ms: Vec<u64>,
    lags: Vec<i64>,
    finals: u64,
    behind: u64,
}

/// Process CPU seconds (user + system).
#[cfg(not(unix))]
fn cpu_seconds() -> f64 {
    0.0
}

/// `getrusage(RUSAGE_SELF)` as 18 machine words: user time (sec, usec),
/// system time (sec, usec), then the counters starting with peak RSS.
#[cfg(unix)]
fn rusage_words() -> Option<[i64; 18]> {
    extern "C" {
        fn getrusage(who: i32, usage: *mut [i64; 18]) -> i32;
    }
    let mut r = [0i64; 18];
    (unsafe { getrusage(0, &mut r) } == 0).then_some(r)
}

/// Process CPU seconds (user + system).
#[cfg(unix)]
fn cpu_seconds() -> f64 {
    let Some(r) = rusage_words() else { return 0.0 };
    let tv = |sec: i64, usec: i64| sec as f64 + (usec & 0xffff_ffff) as f64 / 1e6;
    tv(r[0], r[1]) + tv(r[2], r[3])
}

/// Peak resident set, MB.
fn peak_rss_mb() -> f64 {
    #[cfg(target_os = "linux")]
    {
        let s = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
        s.lines()
            .find(|l| l.starts_with("VmHWM:"))
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|v| v.parse::<f64>().ok())
            .map_or(0.0, |kb| kb / 1024.0)
    }
    #[cfg(target_os = "macos")]
    {
        // ru_maxrss is in bytes on macOS.
        rusage_words().map_or(0.0, |r| r[4] as f64 / 1_048_576.0)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        0.0
    }
}

fn wav(path: &Path) -> Result<Vec<f32>> {
    let v = providers_local::wav::decode_wav_16k_mono_f32(path).map_err(|e| anyhow!("{}: {e}", path.display()))?;
    if v.is_empty() {
        return Err(anyhow!("{} is empty", path.display()));
    }
    Ok(v)
}

fn looped(clip: &[f32], seconds: f64) -> Vec<f32> {
    clip.iter().copied().cycle().take((seconds * SR as f64) as usize).collect()
}

fn pct<T: Copy>(sorted: &[T], p: f64, empty: T) -> T {
    if sorted.is_empty() {
        empty
    } else {
        sorted[((sorted.len() - 1) as f64 * p).round() as usize]
    }
}

/// One live-names instance: a diarizer on a background thread, fed by the
/// phase clock.
struct Instance {
    name: &'static str,
    d: Arc<Mutex<LiveDiarizer>>,
    clip: Vec<f32>,
    /// Audio offset into `clip` where the next phase continues.
    at: usize,
}

struct InstanceStats {
    compute_s: f64,
    audio_s: f64,
    backlog_ms_max: u64,
}

/// Runs `instances` at real-time pace for `seconds` from `start`; returns
/// their stats once each has drained what it was fed.
fn run_instances(instances: &mut [Instance], seconds: f64, start: Instant) -> Vec<std::thread::JoinHandle<InstanceStats>> {
    let mut joins = Vec::new();
    for inst in instances.iter_mut() {
        let per = SR * FEED_MS as usize / 1000;
        let n = (seconds * SR as f64) as usize / per;
        let pieces: Vec<Vec<f32>> = (0..n)
            .map(|i| (0..per).map(|k| inst.clip[(inst.at + i * per + k) % inst.clip.len()]).collect())
            .collect();
        inst.at += n * per;
        let d = Arc::clone(&inst.d);
        let name = inst.name;
        joins.push(std::thread::spawn(move || {
            let (tx, rx) = mpsc::channel::<Vec<f32>>();
            let processed_ms = Arc::new(AtomicU64::new(0));
            let backlog = Arc::new(AtomicU64::new(0));
            let feeder = {
                let processed_ms = Arc::clone(&processed_ms);
                let backlog = Arc::clone(&backlog);
                std::thread::spawn(move || {
                    for (i, p) in pieces.into_iter().enumerate() {
                        let due = start + Duration::from_millis(i as u64 * FEED_MS);
                        if let Some(w) = due.checked_duration_since(Instant::now()) {
                            std::thread::sleep(w);
                        }
                        let fed = (i as u64 + 1) * FEED_MS;
                        backlog.fetch_max(fed.saturating_sub(processed_ms.load(Ordering::SeqCst)), Ordering::SeqCst);
                        if tx.send(p).is_err() {
                            return;
                        }
                    }
                })
            };
            transcript::cpu::lower_to_background_priority();
            let mut compute = 0.0;
            let mut audio = 0.0;
            while let Ok(p) = rx.recv() {
                let t = Instant::now();
                if let Err(e) = d.lock().unwrap().push(&p) {
                    eprintln!("{name}: step failed: {e:#}");
                    break;
                }
                compute += t.elapsed().as_secs_f64();
                audio += p.len() as f64 / SR as f64;
                processed_ms.store((audio * 1000.0) as u64, Ordering::SeqCst);
            }
            let _ = feeder.join();
            InstanceStats { compute_s: compute, audio_s: audio, backlog_ms_max: backlog.load(Ordering::SeqCst) }
        }));
    }
    joins
}

/// Two Nemotron ASR live tracks at real-time pace for `seconds`; `during`
/// starts with the feed.
fn asr_phase(
    rt: &tokio::runtime::Runtime,
    asr_dir: &Path,
    tracks: [Vec<f32>; 2],
    seconds: f64,
    during: impl FnOnce(Instant) -> Vec<std::thread::JoinHandle<InstanceStats>>,
) -> Result<(AsrStats, Vec<InstanceStats>)> {
    use providers_local::nemotron::realtime::NemotronRealtime;
    use providers_local::streaming::live_metrics;
    use providers_realtime::{RealtimeEvent, RealtimeTranscriber};

    let stats = Arc::new(Mutex::new(AsrStats::default()));
    let (joins, ok) = rt.block_on(async {
        live_metrics::reset();
        let live = Arc::new(NemotronRealtime::new(asr_dir.to_path_buf(), "nemotron (room bench)"));
        let resident_before = providers_local::nemotron::model::resident();
        let mut runs = Vec::new();
        let mut feeds = Vec::new();
        for t in tracks {
            let pcm: Vec<i16> = t.iter().map(|&s| (s * 32768.0).clamp(-32768.0, 32767.0) as i16).collect();
            let (audio_tx, audio_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<i16>>();
            let (ev_tx, ev_rx) = tokio::sync::mpsc::channel::<RealtimeEvent>(1024);
            feeds.push((pcm, audio_tx, ev_rx));
            let live = Arc::clone(&live);
            runs.push(tokio::spawn(async move { live.run(16_000, audio_rx, ev_tx).await }));
        }
        while providers_local::nemotron::model::resident() < resident_before + 2 {
            if runs.iter().any(|r| r.is_finished()) {
                return (Vec::new(), false);
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        live_metrics::reset();
        let start = tokio::time::Instant::now();
        let joins = during(start.into_std());
        for (pcm, audio_tx, mut ev_rx) in feeds {
            tokio::spawn(async move {
                for (i, frame) in pcm.chunks(320).enumerate() {
                    tokio::time::sleep_until(start + Duration::from_millis(i as u64 * 20)).await;
                    if audio_tx.send(frame.to_vec()).is_err() {
                        return;
                    }
                }
            });
            let st = Arc::clone(&stats);
            tokio::spawn(async move {
                while let Some(ev) = ev_rx.recv().await {
                    let mut m = st.lock().unwrap();
                    match ev {
                        RealtimeEvent::Final { segment, .. } => {
                            m.finals += 1;
                            m.lags.push(start.elapsed().as_millis() as i64 - segment.end_ms as i64);
                        }
                        RealtimeEvent::FallingBehind => m.behind += 1,
                        RealtimeEvent::Error(e) => eprintln!("asr error: {e}"),
                        _ => {}
                    }
                }
            });
        }
        let st = Arc::clone(&stats);
        let poll = tokio::spawn(async move {
            let mut seen = 0u64;
            loop {
                tokio::time::sleep(Duration::from_millis(40)).await;
                let snap = live_metrics::snapshot();
                if snap.decodes != seen {
                    seen = snap.decodes;
                    st.lock().unwrap().decode_ms.push(snap.last_decode_ms);
                }
            }
        });
        tokio::time::sleep_until(start + Duration::from_secs_f64(seconds)).await;
        poll.abort();
        for r in runs {
            r.abort();
        }
        (joins, true)
    });
    if !ok {
        return Err(anyhow!("an ASR track ended before its model loaded"));
    }
    let inst = joins.into_iter().map(|j| j.join().map_err(|_| anyhow!("instance panicked"))).collect::<Result<Vec<_>>>()?;
    let s = stats.lock().unwrap().clone();
    Ok((s, inst))
}

/// Opts the process out of Windows execution-speed throttling, as the app
/// does at startup, unless `BENCH_NO_OPTOUT=1`. Returns the readback line, or
/// what was skipped.
fn opt_out() -> String {
    if std::env::var_os("BENCH_NO_OPTOUT").is_some_and(|v| v == "1") {
        return "skipped (BENCH_NO_OPTOUT=1)".into();
    }
    daisy_llm_protocol::process_power::opt_out_of_execution_throttling().unwrap_or_else(|| "n/a (not windows)".into())
}

fn main() -> Result<()> {
    let optout = opt_out();
    eprintln!("ecoqos opt-out: {optout}");
    let a: Vec<String> = std::env::args().skip(1).collect();
    let arg = |k: &str| a.iter().position(|x| x == k).and_then(|i| a.get(i + 1)).cloned();
    let need = |k: &str| arg(k).ok_or_else(|| anyhow!("missing {k}"));
    let diar_dir = PathBuf::from(need("--diar-dir")?);
    let asr_dir = PathBuf::from(need("--asr-dir")?);
    let mic = wav(Path::new(&need("--mic")?))?;
    let system = wav(Path::new(&need("--system")?))?;
    let seconds: f64 = arg("--seconds").and_then(|v| v.parse().ok()).unwrap_or(120.0);
    let out = PathBuf::from(need("--out")?);

    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(4).enable_all().build()?;
    let mut phases = Vec::new();
    let mut instances: Vec<Instance> = Vec::new();
    let mut asr_at = 0.0f64;
    for (phase, n) in [("asr_only", 0usize), ("asr_plus_system", 1), ("asr_plus_system_and_mic", 2)] {
        let loaded = Instant::now();
        while instances.len() < n {
            let (name, clip) = if instances.is_empty() { ("system", system.clone()) } else { ("mic", mic.clone()) };
            let d = LiveDiarizer::load(&diar_dir, DIAR_THREADS)?;
            instances.push(Instance { name, d: Arc::new(Mutex::new(d)), clip, at: 0 });
        }
        let load_s = loaded.elapsed().as_secs_f64();
        let rss_before = peak_rss_mb();
        let skip = (asr_at * SR as f64) as usize;
        let shift = |c: &[f32]| -> Vec<f32> {
            let l = looped(c, seconds + asr_at + 1.0);
            l[skip.min(l.len())..].to_vec()
        };
        let tracks = [shift(&mic), shift(&system)];
        asr_at += seconds;
        eprintln!("{phase}: {seconds} s");
        let cpu0 = cpu_seconds();
        let wall0 = Instant::now();
        let insts = &mut instances;
        let (asr, stats) = asr_phase(&rt, &asr_dir, tracks, seconds, |start| run_instances(insts, seconds, start))?;
        let wall = wall0.elapsed().as_secs_f64();
        let cpu = cpu_seconds() - cpu0;
        let mut d = asr.decode_ms.clone();
        d.sort_unstable();
        let mut l = asr.lags.clone();
        l.sort_unstable();
        let per: Vec<serde_json::Value> = instances
            .iter()
            .zip(&stats)
            .map(|(i, s)| {
                serde_json::json!({
                    "track": i.name,
                    "rtf": s.compute_s / s.audio_s.max(1e-9),
                    "audio_s": s.audio_s,
                    "compute_s": s.compute_s,
                    "backlog_ms_max": s.backlog_ms_max,
                })
            })
            .collect();
        phases.push(serde_json::json!({
            "phase": phase,
            "instances_loaded_in_s": load_s,
            "wall_s": wall,
            "process_cpu_pct_of_one_core": 100.0 * cpu / wall,
            "peak_rss_mb_before": rss_before,
            "peak_rss_mb_after": peak_rss_mb(),
            "asr_decode_ms_p50": pct(&d, 0.5, 0),
            "asr_decode_ms_p90": pct(&d, 0.9, 0),
            "asr_decodes": d.len(),
            "asr_final_lag_ms_p90": pct(&l, 0.9, -1),
            "asr_finals": asr.finals,
            "asr_falling_behind": asr.behind,
            "instances": per,
        }));
        eprintln!("{}", phases.last().unwrap());
    }
    let j = serde_json::json!({
        "host": std::process::Command::new("hostname").output().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default(),
        "cores": std::thread::available_parallelism().map(|n| n.get()).unwrap_or(0),
        "seconds_per_phase": seconds,
        "diar_threads": DIAR_THREADS,
        "ecoqos_opt_out": optout,
        "phases": phases,
    });
    std::fs::write(&out, serde_json::to_vec_pretty(&j)?)?;
    println!("{}", serde_json::to_string_pretty(&j)?);
    Ok(())
}
