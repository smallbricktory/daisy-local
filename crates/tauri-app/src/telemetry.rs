//! Periodic self-process CPU/RSS telemetry logged to the regular log stream.
//!
//! Linux-only (parses /proc/self/{stat,status}); on other platforms `spawn`
//! is a no-op.

use std::time::{Duration, Instant};

const SAMPLE_INTERVAL_SECONDS: u64 = 15;

/// Spawns the telemetry sampler on the tokio runtime. Logs one line every
/// SAMPLE_INTERVAL_SECONDS at info! level. Called once during app startup;
/// the task lives for the app's lifetime.
#[cfg(target_os = "linux")]
pub fn spawn() {
    tauri::async_runtime::spawn(async move {
        run_linux().await;
    });
}

#[cfg(not(target_os = "linux"))]
pub fn spawn() {
    log::debug!("telemetry: not implemented on this platform (skipping)");
}

#[cfg(target_os = "linux")]
async fn run_linux() {
    let clk_tck = clock_ticks_per_sec();
    let page_size_kb = page_size_kb();
    let mut last_total_ticks: Option<u64> = None;
    let mut last_wall: Instant = Instant::now();
    log::info!(
        "telemetry: starting Linux sampler every {SAMPLE_INTERVAL_SECONDS}s (clk_tck={clk_tck} page_kb={page_size_kb})"
    );
    loop {
        tokio::time::sleep(Duration::from_secs(SAMPLE_INTERVAL_SECONDS)).await;
        let now = Instant::now();
        let wall_ms = now.duration_since(last_wall).as_millis().max(1) as u64;
        last_wall = now;

        let stat = match read_proc_self_stat() {
            Some(s) => s,
            None => {
                log::debug!("telemetry: /proc/self/stat unavailable");
                continue;
            }
        };
        let rss_kb = read_rss_kb_from_status().unwrap_or(stat.rss_pages * page_size_kb);
        let total_ticks = stat.utime + stat.stime;
        let cpu_pct = match last_total_ticks {
            Some(prev) => {
                let delta_ticks = total_ticks.saturating_sub(prev) as f64;
                let cpu_seconds = delta_ticks / (clk_tck as f64);
                let wall_seconds = (wall_ms as f64) / 1000.0;
                (cpu_seconds / wall_seconds) * 100.0
            }
            None => 0.0,
        };
        last_total_ticks = Some(total_ticks);

        log::info!(
            "telemetry: cpu={cpu:.1}% rss={rss} MB threads={threads}",
            cpu = cpu_pct,
            rss = rss_kb / 1024,
            threads = stat.num_threads,
        );
    }
}

#[cfg(target_os = "linux")]
struct ProcStat {
    utime: u64,
    stime: u64,
    num_threads: i64,
    rss_pages: u64,
}

#[cfg(target_os = "linux")]
fn read_proc_self_stat() -> Option<ProcStat> {
    let raw = syncsafe::read_to_string("/proc/self/stat").ok()?;
    // The (comm) field can contain spaces — split on the last ')'.
    let close = raw.rfind(')')?;
    let tail = &raw[close + 1..];
    let fields: Vec<&str> = tail.split_whitespace().collect();
    // `tail` starts after "pid (comm)"; man-page field N (1-indexed) lives at
    // index N-3:
    //   man field 14 utime    -> idx 11
    //   man field 15 stime    -> idx 12
    //   man field 20 num_threads -> idx 17
    //   man field 24 rss      -> idx 21 (in pages)
    let utime = fields.get(11)?.parse::<u64>().ok()?;
    let stime = fields.get(12)?.parse::<u64>().ok()?;
    let num_threads = fields.get(17)?.parse::<i64>().ok()?;
    let rss_pages = fields.get(21)?.parse::<u64>().ok()?;
    Some(ProcStat { utime, stime, num_threads, rss_pages })
}

#[cfg(target_os = "linux")]
fn read_rss_kb_from_status() -> Option<u64> {
    let raw = syncsafe::read_to_string("/proc/self/status").ok()?;
    for line in raw.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            let kb = rest
                .split_whitespace()
                .next()
                .and_then(|s| s.parse::<u64>().ok())?;
            return Some(kb);
        }
    }
    None
}

#[cfg(target_os = "linux")]
fn clock_ticks_per_sec() -> u64 {
    // SAFETY: sysconf is async-signal-safe and takes a constant arg.
    let v = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    if v <= 0 { 100 } else { v as u64 }
}

#[cfg(target_os = "linux")]
fn page_size_kb() -> u64 {
    let v = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if v <= 0 { 4 } else { (v as u64) / 1024 }
}
