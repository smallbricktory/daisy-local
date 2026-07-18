//! Periodic system-resource sampler for diagnostics. Emits one structured
//! line (log target `perf`) per interval with memory/swap/CPU and the
//! heaviest processes. Cross-platform via `sysinfo`. Spawned only when
//! `debug_logging` is on.
//!
//! Process names are not logged. Each top process is identified by a
//! per-session salted hash (`pXXXXXXXX`); the same process keeps the same id
//! across samples within a session. The salt is random per run and never
//! logged; ids are not comparable across sessions.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// True while a recording/call is in progress. The sampler emits only while
/// this is set. Toggled by the recording start/stop path.
pub static RECORDING_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Mark a recording as started/stopped, gating perf sampling.
pub fn set_recording_active(on: bool) {
    RECORDING_ACTIVE.store(on, Ordering::Relaxed);
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProcInfo {
    /// Per-session salted-hash id (`pXXXXXXXX`), not the process name.
    /// Stable for the same process within one session.
    pub name: String,
    pub rss_mb: u64,
    pub cpu: f32,
}

/// Salted, non-reversible per-session id for a process name. The same
/// (salt, name) pair yields the same id.
fn hash_name(salt: u64, name: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    salt.hash(&mut h);
    name.hash(&mut h);
    format!("p{:08x}", h.finish() & 0xffff_ffff)
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct PerfSnapshot {
    pub mem_used_mb: u64,
    pub mem_total_mb: u64,
    pub mem_avail_mb: u64,
    pub swap_used_mb: u64,
    pub swap_total_mb: u64,
    pub load_one: f64,
    pub self_rss_mb: u64,
    pub self_cpu: f32,
    /// Heaviest processes by memory.
    pub top: Vec<ProcInfo>,
    /// Live whisper decoder metrics. `None` when no local-whisper decode has
    /// run this session.
    pub whisper: Option<providers_local::streaming::live_metrics::LiveMetrics>,
}

/// Renders a snapshot as a single log line.
pub fn format_perf_line(s: &PerfSnapshot) -> String {
    let mut line = format!(
        "mem={}/{}MB avail={}MB swap={}/{}MB load={:.2} self={}MB/{:.1}%",
        s.mem_used_mb,
        s.mem_total_mb,
        s.mem_avail_mb,
        s.swap_used_mb,
        s.swap_total_mb,
        s.load_one,
        s.self_rss_mb,
        s.self_cpu
    );
    if !s.top.is_empty() {
        let top: Vec<String> = s
            .top
            .iter()
            .map(|p| format!("{} {}MB/{:.0}%", p.name, p.rss_mb, p.cpu))
            .collect();
        line.push_str(&format!(" top=[{}]", top.join(", ")));
    }
    if let Some(w) = s.whisper {
        line.push_str(&format!(
            " whisper=[decode:{}ms wait:{}ms win:{}ms backlog:{}ms(max {}) n:{} hop:{}ms rho:{:.2}{}]",
            w.last_decode_ms,
            w.wait_ms,
            w.window_ms,
            w.backlog_ms,
            w.max_backlog_ms,
            w.decodes,
            w.hop_ms,
            w.rho_x100 as f64 / 100.0,
            if w.floor { " FLOOR" } else { "" }
        ));
    }
    line
}

fn gather(sys: &mut sysinfo::System, salt: u64) -> PerfSnapshot {
    use sysinfo::ProcessesToUpdate;
    sys.refresh_memory();
    sys.refresh_processes(ProcessesToUpdate::All, true);
    let mb = |b: u64| b / 1024 / 1024;

    let (self_rss_mb, self_cpu) = sysinfo::get_current_pid()
        .ok()
        .and_then(|pid| sys.process(pid))
        .map(|p| (mb(p.memory()), p.cpu_usage()))
        .unwrap_or((0, 0.0));

    let mut procs: Vec<_> = sys.processes().values().collect();
    procs.sort_by_key(|p| std::cmp::Reverse(p.memory()));
    let top: Vec<ProcInfo> = procs
        .iter()
        .take(5)
        .map(|p| ProcInfo {
            // Salted hash, never the raw name.
            name: hash_name(salt, &p.name().to_string_lossy()),
            rss_mb: mb(p.memory()),
            cpu: p.cpu_usage(),
        })
        .collect();

    PerfSnapshot {
        mem_used_mb: mb(sys.used_memory()),
        mem_total_mb: mb(sys.total_memory()),
        mem_avail_mb: mb(sys.available_memory()),
        swap_used_mb: mb(sys.used_swap()),
        swap_total_mb: mb(sys.total_swap()),
        load_one: sysinfo::System::load_average().one,
        self_rss_mb,
        self_cpu,
        top,
        whisper: {
            let m = providers_local::streaming::live_metrics::snapshot();
            // Surfaced only when local whisper has decoded this session.
            (m.decodes > 0).then_some(m)
        },
    }
}

/// Spawns the sampler thread. Returns a stop flag; setting it halts sampling.
/// The thread also exits with the process. Samples every `interval_secs`
/// seconds.
pub fn spawn(interval_secs: u64) -> Arc<AtomicBool> {
    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = Arc::clone(&stop);
    let _ = std::thread::Builder::new()
        .name("daisy-perf".into())
        .spawn(move || {
            // Random per-session salt for process-id hashing (never logged).
            let salt = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0)
                ^ (std::process::id() as u64).rotate_left(32);
            let mut sys = sysinfo::System::new();
            // CPU usage needs two samples spaced by the minimum interval.
            sys.refresh_all();
            std::thread::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL);
            while !stop2.load(Ordering::Relaxed) {
                if RECORDING_ACTIVE.load(Ordering::Relaxed) {
                    let snap = gather(&mut sys, salt);
                    log::info!(target: "perf", "{}", format_perf_line(&snap));
                }
                // Sleeps in 250ms steps; a stop request is observed within 250ms.
                let steps = (interval_secs * 4).max(1);
                for _ in 0..steps {
                    if stop2.load(Ordering::Relaxed) {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(250));
                }
            }
        });
    stop
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_line_without_top() {
        let s = PerfSnapshot {
            mem_used_mb: 12345,
            mem_total_mb: 16384,
            mem_avail_mb: 2048,
            swap_used_mb: 512,
            swap_total_mb: 2048,
            load_one: 3.44,
            self_rss_mb: 192,
            self_cpu: 4.2,
            top: vec![],
            whisper: None,
        };
        assert_eq!(
            format_perf_line(&s),
            "mem=12345/16384MB avail=2048MB swap=512/2048MB load=3.44 self=192MB/4.2%"
        );
    }

    #[test]
    fn formats_line_with_top() {
        let s = PerfSnapshot {
            top: vec![
                ProcInfo { name: "p1a2b3c4d".into(), rss_mb: 1850, cpu: 8.0 },
                ProcInfo { name: "pdeadbeef".into(), rss_mb: 192, cpu: 4.0 },
            ],
            ..Default::default()
        };
        let l = format_perf_line(&s);
        assert!(l.contains("top=[p1a2b3c4d 1850MB/8%, pdeadbeef 192MB/4%]"), "got: {l}");
    }

    #[test]
    fn formats_line_with_whisper_metrics() {
        let s = PerfSnapshot {
            whisper: Some(providers_local::streaming::live_metrics::LiveMetrics {
                last_decode_ms: 1800,
                window_ms: 28000,
                backlog_ms: 4200,
                max_backlog_ms: 9000,
                decodes: 412,
                hop_ms: 3000,
                rho_x100: 168,
                floor: true,
                wait_ms: 640,
            }),
            ..Default::default()
        };
        let l = format_perf_line(&s);
        assert!(
            l.contains(
                "whisper=[decode:1800ms wait:640ms win:28000ms backlog:4200ms(max 9000) n:412 hop:3000ms rho:1.68 FLOOR]"
            ),
            "got: {l}"
        );
    }

    #[test]
    fn hash_name_is_salted_stable_and_not_plaintext() {
        // Deterministic for the same salt; differs across salts; never the name.
        assert_eq!(hash_name(42, "Microsoft Teams"), hash_name(42, "Microsoft Teams"));
        assert_ne!(hash_name(42, "Microsoft Teams"), hash_name(43, "Microsoft Teams"));
        assert_ne!(hash_name(42, "Microsoft Teams"), hash_name(42, "Bitwarden"));
        let h = hash_name(42, "Microsoft Teams");
        assert!(h.starts_with('p') && h.len() == 9 && !h.contains("Teams"));
    }
}
