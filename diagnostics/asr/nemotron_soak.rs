// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Two-track live soak: a mic and a system wav, each looped, through the
//! Nemotron live backend at real-time pace, one run per track as a recording
//! does. The audio paths are arguments.
//!
//!   cargo run --release -p providers-local --example nemotron_soak -- \
//!       MODEL_DIR MIC.wav SYSTEM.wav [MINUTES]
//!
//! Prints one JSON line per minute: minute, rss_mb (current), cpu_s (process
//! CPU seconds that minute), backlog_ms_max (largest backlog either track
//! drained that minute), decode_ms_p50/p90/max (per chunk decode wall time),
//! decodes, final_lag_ms_p50/p90 (Final receipt minus segment end on the feed
//! clock), finals, behind (FallingBehind events), errors. RSS and CPU are
//! reported on Linux and Windows. Feeding starts once both tracks hold their
//! model; the load time prints to stderr.
//!

use providers_local::nemotron::realtime::NemotronRealtime;
use providers_local::streaming::live_metrics;
use providers_realtime::{RealtimeEvent, RealtimeTranscriber};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Default)]
struct Minute {
    lags: Vec<i64>,
    decode_ms: Vec<u64>,
    finals: u64,
    behind: u64,
    errors: u64,
    backlog_max: u64,
}

/// utime + stime in clock ticks from a /proc/<pid>/stat line. Fields after the
/// parenthesised comm start at the state letter (field 3), so utime is field 14
/// (index 11) and stime field 15 (index 12).
#[cfg(any(target_os = "linux", test))]
fn stat_cpu_ticks(stat: &str) -> f64 {
    let after = stat.rsplit_once(')').map_or("", |(_, r)| r);
    let f: Vec<&str> = after.split_whitespace().collect();
    let at = |i: usize| f.get(i).and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0);
    at(11) + at(12)
}

#[cfg(target_os = "linux")]
fn rss_mb_and_cpu_s() -> (f64, f64) {
    extern "C" {
        fn sysconf(name: i32) -> i64;
    }
    const SC_CLK_TCK: i32 = 2;
    const SC_PAGESIZE: i32 = 30;
    let (tick, page) = unsafe { (sysconf(SC_CLK_TCK).max(1) as f64, sysconf(SC_PAGESIZE).max(1) as f64) };
    let statm = std::fs::read_to_string("/proc/self/statm").unwrap_or_default();
    let pages: f64 = statm.split_whitespace().nth(1).and_then(|v| v.parse().ok()).unwrap_or(0.0);
    let stat = std::fs::read_to_string("/proc/self/stat").unwrap_or_default();
    (pages * page / 1_048_576.0, stat_cpu_ticks(&stat) / tick)
}

#[cfg(windows)]
fn rss_mb_and_cpu_s() -> (f64, f64) {
    #[repr(C)]
    #[allow(dead_code)]
    struct Pmc {
        cb: u32,
        faults: u32,
        peak_ws: usize,
        ws: usize,
        rest: [usize; 6],
    }
    #[repr(C)]
    #[derive(Default, Clone, Copy)]
    struct Ft {
        lo: u32,
        hi: u32,
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GetCurrentProcess() -> isize;
        fn K32GetProcessMemoryInfo(h: isize, p: *mut Pmc, cb: u32) -> i32;
        fn GetProcessTimes(h: isize, c: *mut Ft, e: *mut Ft, k: *mut Ft, u: *mut Ft) -> i32;
    }
    let mut p: Pmc = unsafe { std::mem::zeroed() };
    p.cb = std::mem::size_of::<Pmc>() as u32;
    let (mut c, mut e, mut k, mut u) = (Ft::default(), Ft::default(), Ft::default(), Ft::default());
    unsafe {
        K32GetProcessMemoryInfo(GetCurrentProcess(), &mut p, p.cb);
        GetProcessTimes(GetCurrentProcess(), &mut c, &mut e, &mut k, &mut u);
    }
    let secs = |t: Ft| (((t.hi as u64) << 32) | t.lo as u64) as f64 / 10_000_000.0;
    (p.ws as f64 / 1_048_576.0, secs(k) + secs(u))
}

#[cfg(not(any(target_os = "linux", windows)))]
fn rss_mb_and_cpu_s() -> (f64, f64) {
    (0.0, 0.0)
}

fn looped(path: &str, samples: usize) -> Vec<i16> {
    let one: Vec<i16> = providers_local::wav::decode_wav_16k_mono_f32(std::path::Path::new(path))
        .expect("wav")
        .iter()
        .map(|&s| (s * 32768.0).clamp(-32768.0, 32767.0) as i16)
        .collect();
    assert!(!one.is_empty(), "empty wav: {path}");
    one.iter().copied().cycle().take(samples).collect()
}

fn pct<T: Copy + Ord>(sorted: &[T], p: f64, empty: T) -> T {
    if sorted.is_empty() {
        empty
    } else {
        sorted[((sorted.len() - 1) as f64 * p).round() as usize]
    }
}

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    if a.len() < 3 {
        eprintln!("usage: nemotron_soak MODEL_DIR MIC.wav SYSTEM.wav [MINUTES]");
        std::process::exit(2);
    }
    let minutes: u64 = a.get(3).and_then(|v| v.parse().ok()).unwrap_or(60);
    let total = (minutes * 60 * 16_000) as usize;
    let tracks = [looped(&a[1], total), looped(&a[2], total)];
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(4).enable_all().build().unwrap();
    rt.block_on(async move {
        live_metrics::reset();
        let live = Arc::new(NemotronRealtime::new(a[0].clone().into(), "nemotron (soak)"));
        let stats = Arc::new(Mutex::new(Minute::default()));
        let resident_before = providers_local::nemotron::model::resident();
        let load_started = tokio::time::Instant::now();
        let mut runs = Vec::new();
        let mut feeds = Vec::new();
        for pcm in tracks {
            let (audio_tx, audio_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<i16>>();
            let (ev_tx, ev_rx) = tokio::sync::mpsc::channel::<RealtimeEvent>(1024);
            feeds.push((pcm, audio_tx, ev_rx));
            let live = Arc::clone(&live);
            runs.push(tokio::spawn(async move { live.run(16_000, audio_rx, ev_tx).await }));
        }
        while providers_local::nemotron::model::resident() < resident_before + 2 {
            if runs.iter().any(|r| r.is_finished()) {
                eprintln!("a track ended before its model loaded");
                std::process::exit(1);
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        eprintln!("models loaded in {:.1} s", load_started.elapsed().as_secs_f64());
        live_metrics::reset();
        let start = tokio::time::Instant::now();
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
                        RealtimeEvent::Error(e) => {
                            m.errors += 1;
                            eprintln!("error: {e}");
                        }
                        _ => {}
                    }
                }
            });
        }
        let st = Arc::clone(&stats);
        tokio::spawn(async move {
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
        let mut cpu_prev = rss_mb_and_cpu_s().1;
        for minute in 1..=minutes {
            tokio::time::sleep_until(start + Duration::from_secs(minute * 60)).await;
            let (rss, cpu) = rss_mb_and_cpu_s();
            let backlog_max = live_metrics::snapshot().max_backlog_ms;
            live_metrics::reset();
            let mut m = std::mem::take(&mut *stats.lock().unwrap());
            m.backlog_max = backlog_max;
            m.lags.sort_unstable();
            m.decode_ms.sort_unstable();
            println!(
                "{{\"minute\":{minute},\"rss_mb\":{rss:.0},\"cpu_s\":{:.1},\"backlog_ms_max\":{},\"decode_ms_p50\":{},\"decode_ms_p90\":{},\"decode_ms_max\":{},\"decodes\":{},\"final_lag_ms_p50\":{},\"final_lag_ms_p90\":{},\"finals\":{},\"behind\":{},\"errors\":{}}}",
                cpu - cpu_prev,
                m.backlog_max,
                pct(&m.decode_ms, 0.5, 0),
                pct(&m.decode_ms, 0.9, 0),
                pct(&m.decode_ms, 1.0, 0),
                m.decode_ms.len(),
                pct(&m.lags, 0.5, -1),
                pct(&m.lags, 0.9, -1),
                m.finals,
                m.behind,
                m.errors
            );
            cpu_prev = cpu;
        }
        for r in runs {
            let _ = r.await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::stat_cpu_ticks;

    #[test]
    fn cpu_ticks_are_utime_plus_stime_after_the_comm() {
        let line = "4242 (my (odd) app) S 1 4242 4242 0 -1 4194560 100 0 0 0 700 50 9 9 20 0 4 0 12345 1000 200";
        assert_eq!(stat_cpu_ticks(line), 750.0);
    }
}
