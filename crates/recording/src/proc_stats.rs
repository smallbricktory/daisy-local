// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Samples of this process: CPU (percent of one core) and resident memory
//! on every OS, plus thread count and CPU per thread name on Linux and
//! macOS. Each sample covers the time since the previous one.

use crate::telemetry::ProcRecord;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Thread names reported per sample, busiest first.
const TOP_THREAD_NAMES: usize = 8;

pub struct ProcSampler {
    sys: sysinfo::System,
    pid: Option<sysinfo::Pid>,
    /// CPU ns per thread id at the previous sample.
    threads: HashMap<u64, u64>,
    /// Process CPU ns at the previous sample.
    cpu_ns: Option<u64>,
    last: Instant,
}

impl Default for ProcSampler {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcSampler {
    /// Starts the clock: the first [`Self::sample`] covers the time from
    /// here.
    pub fn new() -> Self {
        let mut me = Self {
            sys: sysinfo::System::new(),
            pid: sysinfo::get_current_pid().ok(),
            threads: HashMap::new(),
            cpu_ns: process_cpu_ns(),
            last: Instant::now(),
        };
        me.refresh();
        if let Some(t) = thread_times() {
            me.threads = t.into_iter().map(|(id, _, ns)| (id, ns)).collect();
        }
        me
    }

    fn refresh(&mut self) {
        if let Some(pid) = self.pid {
            self.sys.refresh_processes_specifics(
                sysinfo::ProcessesToUpdate::Some(&[pid]),
                false,
                sysinfo::ProcessRefreshKind::nothing().with_memory(),
            );
        }
    }

    pub fn sample(&mut self) -> ProcRecord {
        self.refresh();
        let wall_ns = self.last.elapsed().as_nanos().max(1) as f64;
        self.last = Instant::now();
        let rss_mb = self
            .pid
            .and_then(|pid| self.sys.process(pid))
            .map(|p| p.memory() as f32 / (1024.0 * 1024.0))
            .unwrap_or(0.0);
        let cpu_ns = process_cpu_ns();
        let cpu_pct = match (cpu_ns, self.cpu_ns) {
            (Some(now), Some(prev)) => ((now.saturating_sub(prev) as f64 / wall_ns * 1000.0).round() / 10.0) as f32,
            _ => 0.0,
        };
        self.cpu_ns = cpu_ns;
        let mut record = ProcRecord { cpu_pct, rss_mb, threads: None, thread_cpu: Vec::new() };
        if let Some(now) = thread_times() {
            record.threads = Some(now.len() as u32);
            let mut by_name: HashMap<String, u64> = HashMap::new();
            let mut next = HashMap::with_capacity(now.len());
            for (id, name, ns) in now {
                let delta = ns.saturating_sub(self.threads.get(&id).copied().unwrap_or(0));
                *by_name.entry(name).or_default() += delta;
                next.insert(id, ns);
            }
            self.threads = next;
            record.thread_cpu = top_names(by_name, wall_ns);
        }
        record
    }
}

/// Per-name CPU as percent of one core over `wall_ns`, busiest first,
/// rounded to 0.1 and without idle names.
fn top_names(by_name: HashMap<String, u64>, wall_ns: f64) -> Vec<(String, f32)> {
    let mut v: Vec<(String, f32)> = by_name
        .into_iter()
        .map(|(n, ns)| (n, ((ns as f64 / wall_ns * 1000.0).round() / 10.0) as f32))
        .filter(|(_, pct)| *pct > 0.0)
        .collect();
    v.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    v.truncate(TOP_THREAD_NAMES);
    v
}

/// User plus system CPU of the whole process, in ns.
#[cfg(unix)]
fn process_cpu_ns() -> Option<u64> {
    // SAFETY: getrusage fills the zeroed struct it is handed.
    let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
    if unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut ru) } != 0 {
        return None;
    }
    let us = |t: libc::timeval| t.tv_sec as u64 * 1_000_000 + t.tv_usec as u64;
    Some((us(ru.ru_utime) + us(ru.ru_stime)) * 1_000)
}

#[cfg(windows)]
fn process_cpu_ns() -> Option<u64> {
    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
    let (mut created, mut exited, mut kernel, mut user) =
        (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
    // SAFETY: the pseudo-handle needs no closing; the out-params are live locals.
    unsafe { GetProcessTimes(GetCurrentProcess(), &mut created, &mut exited, &mut kernel, &mut user) }.ok()?;
    let ns = |f: FILETIME| (((f.dwHighDateTime as u64) << 32) | f.dwLowDateTime as u64) * 100;
    Some(ns(kernel) + ns(user))
}

#[cfg(not(any(unix, windows)))]
fn process_cpu_ns() -> Option<u64> {
    None
}

/// `(thread id, name, CPU ns)` for every thread of this process.
#[cfg(target_os = "linux")]
fn thread_times() -> Option<Vec<(u64, String, u64)>> {
    // SAFETY: sysconf takes a constant argument and has no side effects.
    let tck = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    let ns_per_tick = 1_000_000_000 / if tck > 0 { tck as u64 } else { 100 };
    let mut out = Vec::new();
    for entry in std::fs::read_dir("/proc/self/task").ok()?.flatten() {
        let Some(id) = entry.file_name().to_str().and_then(|s| s.parse::<u64>().ok()) else { continue };
        let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else { continue };
        if let Some((name, ticks)) = parse_task_stat(&stat) {
            out.push((id, name, ticks * ns_per_tick));
        }
    }
    Some(out)
}

/// `(comm, utime + stime ticks)` from a `/proc/<pid>/task/<tid>/stat` line.
#[cfg(any(target_os = "linux", test))]
fn parse_task_stat(stat: &str) -> Option<(String, u64)> {
    let open = stat.find('(')?;
    let close = stat.rfind(')')?;
    let name = stat.get(open + 1..close)?.to_string();
    let fields: Vec<&str> = stat.get(close + 1..)?.split_whitespace().collect();
    let utime = fields.get(11)?.parse::<u64>().ok()?;
    let stime = fields.get(12)?.parse::<u64>().ok()?;
    Some((name, utime + stime))
}

#[cfg(target_os = "macos")]
fn thread_times() -> Option<Vec<(u64, String, u64)>> {
    use std::ffi::{c_int, c_void, CStr};
    const PROC_PIDLISTTHREADS: c_int = 6;
    let pid = std::process::id() as c_int;
    let mut ids: Vec<u64> = vec![0; 256];
    let count = loop {
        let bytes = (ids.len() * 8) as c_int;
        // SAFETY: the buffer holds `bytes` bytes and outlives the call.
        let got = unsafe { libc::proc_pidinfo(pid, PROC_PIDLISTTHREADS, 0, ids.as_mut_ptr() as *mut c_void, bytes) };
        if got <= 0 {
            return None;
        }
        if got < bytes {
            break got as usize / 8;
        }
        ids.resize(ids.len() * 2, 0);
    };
    let mut out = Vec::with_capacity(count);
    let size = std::mem::size_of::<libc::proc_threadinfo>() as c_int;
    for &id in &ids[..count] {
        // SAFETY: proc_threadinfo is plain data; the kernel fills `size` bytes.
        let mut info: libc::proc_threadinfo = unsafe { std::mem::zeroed() };
        let got = unsafe {
            libc::proc_pidinfo(pid, libc::PROC_PIDTHREADINFO, id, &mut info as *mut _ as *mut c_void, size)
        };
        if got != size {
            continue;
        }
        // SAFETY: pth_name is a NUL-terminated buffer filled by the kernel.
        let name = unsafe { CStr::from_ptr(info.pth_name.as_ptr()) }.to_string_lossy().into_owned();
        out.push((id, name, info.pth_user_time + info.pth_system_time));
    }
    Some(out)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn thread_times() -> Option<Vec<(u64, String, u64)>> {
    None
}

/// A thread writing a `proc` telemetry record every `every` until dropped.
pub struct ProcSamplerThread {
    stop: Option<std::sync::mpsc::Sender<()>>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl ProcSamplerThread {
    pub fn spawn(telemetry: Arc<crate::telemetry::Telemetry>, every: Duration) -> Self {
        let (stop, rx) = std::sync::mpsc::channel::<()>();
        let handle = std::thread::Builder::new()
            .name("daisy-proc-sampler".into())
            .spawn(move || {
                let mut sampler = ProcSampler::new();
                while let Err(std::sync::mpsc::RecvTimeoutError::Timeout) = rx.recv_timeout(every) {
                    telemetry.proc(&sampler.sample());
                }
                telemetry.proc(&sampler.sample());
            })
            .ok();
        Self { stop: Some(stop), handle }
    }
}

impl Drop for ProcSamplerThread {
    /// Writes a last sample, then joins the thread.
    fn drop(&mut self) {
        self.stop = None;
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_stat_lines_parse_with_spaces_and_parens_in_the_name() {
        let line = "4242 (daisy (live) pipe) S 1 2 3 4 5 6 7 8 9 10 150 25 0 0 20 0 1 0 100 0 0";
        assert_eq!(parse_task_stat(line), Some(("daisy (live) pipe".to_string(), 175)));
        assert_eq!(parse_task_stat("garbage"), None);
    }

    #[test]
    fn thread_names_rank_busiest_first_and_drop_idle_ones() {
        let by_name = HashMap::from([
            ("idle".to_string(), 0u64),
            ("asr".to_string(), 500_000_000),
            ("audio".to_string(), 50_000_000),
        ]);
        let top = top_names(by_name, 1_000_000_000.0);
        assert_eq!(top, vec![("asr".to_string(), 50.0), ("audio".to_string(), 5.0)]);
    }

    #[test]
    fn a_busy_thread_shows_in_the_sample() {
        let mut s = ProcSampler::new();
        let spin = std::thread::Builder::new()
            .name("spin-probe".into())
            .spawn(|| {
                let t = Instant::now();
                let mut x = 0u64;
                while t.elapsed() < Duration::from_millis(600) {
                    x = std::hint::black_box(x.wrapping_mul(31).wrapping_add(7));
                }
                x
            })
            .unwrap();
        std::thread::sleep(Duration::from_millis(300));
        let r = s.sample();
        spin.join().unwrap();
        assert!(r.rss_mb > 1.0, "rss {}", r.rss_mb);
        assert!(r.cpu_pct > 30.0, "cpu {}", r.cpu_pct);
        if cfg!(any(target_os = "linux", target_os = "macos")) {
            assert!(r.threads.unwrap_or(0) >= 2);
            let spin_pct = r.thread_cpu.iter().find(|(n, _)| n == "spin-probe").map(|(_, p)| *p);
            assert!(spin_pct.is_some_and(|p| p > 50.0), "{:?}", r.thread_cpu);
        }
    }
}
