// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! CPU budget and scheduling priority for background finalize work: every
//! stage after Stop, orphan recovery, and re-transcribe / re-diarize jobs.
//! Live capture and live ASR keep their own budgets.
//!
//! Priority per OS:
//! - Linux: `setpriority` nice 10 on the calling thread. Threads it creates
//!   afterwards inherit the nice value.
//! - macOS: `QOS_CLASS_UTILITY` on the calling thread. Threads it creates
//!   start at the default QoS, so each worker lowers itself.
//! - Windows: `THREAD_PRIORITY_LOWEST` on the calling thread. Threads it
//!   creates start at normal priority, so each worker lowers itself.
//!
//! Long-lived threads and processes that also serve interactive work (the
//! OpenBLAS pool, the daisy-llm worker) are created through
//! [`run_at_normal_priority`].

use std::cell::Cell;
use std::sync::{mpsc, Mutex, OnceLock};

thread_local! {
    static BACKGROUND: Cell<bool> = const { Cell::new(false) };
}

/// Threads background finalize work may keep busy at once: half the logical
/// cores, at least one.
pub fn finalize_cpu_budget() -> usize {
    half_cores(std::thread::available_parallelism().map(|n| n.get()).unwrap_or(2))
}

fn half_cores(cores: usize) -> usize {
    (cores / 2).max(1)
}

/// Drops the calling thread to background scheduling priority.
/// Applies to CPU only; I/O is left unthrottled.
pub fn lower_to_background_priority() {
    #[cfg(target_os = "linux")]
    // Per-thread on Linux: `who = 0` is the calling thread.
    unsafe {
        libc::setpriority(libc::PRIO_PROCESS, 0, 10);
    }

    #[cfg(target_os = "macos")]
    unsafe {
        // In libSystem, no libc binding. 0x11 = QOS_CLASS_UTILITY.
        unsafe extern "C" {
            fn pthread_set_qos_class_self_np(
                qos_class: std::ffi::c_uint,
                relative_priority: std::ffi::c_int,
            ) -> std::ffi::c_int;
        }
        pthread_set_qos_class_self_np(0x11, 0);
    }

    #[cfg(target_os = "windows")]
    unsafe {
        use windows::Win32::System::Threading::{
            GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_LOWEST,
        };
        let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_LOWEST);
    }

    BACKGROUND.with(|b| b.set(true));
}

fn is_background() -> bool {
    BACKGROUND.with(|b| b.get())
}

/// Held by the one background job running at a time.
static BACKGROUND_JOB: Mutex<()> = Mutex::new(());

/// Runs `f` as a background job and returns its result. On a thread already
/// at background priority `f` runs inline. Otherwise `f` waits for the
/// running background job to finish, then runs on a fresh thread that is
/// lowered first and ends with `f`; one background job runs at a time.
/// A panic in `f` resumes on the caller.
pub fn run_in_background<T: Send>(f: impl FnOnce() -> T + Send) -> T {
    if is_background() {
        return f();
    }
    let _job = BACKGROUND_JOB.lock().unwrap_or_else(|e| e.into_inner());
    std::thread::scope(|s| {
        s.spawn(|| {
            lower_to_background_priority();
            f()
        })
        .join()
    })
    .unwrap_or_else(|p| std::panic::resume_unwind(p))
}

/// The process-wide rayon pool of `finalize_cpu_budget()` workers, each
/// lowered to background priority as it starts.
pub fn finalize_pool() -> &'static rayon::ThreadPool {
    static POOL: OnceLock<rayon::ThreadPool> = OnceLock::new();
    POOL.get_or_init(|| {
        rayon::ThreadPoolBuilder::new()
            .num_threads(finalize_cpu_budget())
            .thread_name(|i| format!("finalize-{i}"))
            .start_handler(|_| lower_to_background_priority())
            .build()
            .expect("build the finalize thread pool")
    })
}

type Job = Box<dyn FnOnce() + Send>;

static NORMAL_RUNNER: OnceLock<mpsc::Sender<Job>> = OnceLock::new();

/// Starts the normal-priority runner thread from the calling thread. Call it
/// early from a normal-priority thread (the app's main thread); a background
/// caller starts nothing. Idempotent.
pub fn start_normal_priority_runner() {
    if is_background() {
        return;
    }
    NORMAL_RUNNER.get_or_init(|| {
        let (tx, rx) = mpsc::channel::<Job>();
        std::thread::Builder::new()
            .name("normal-priority-runner".into())
            .spawn(move || {
                for job in rx {
                    job();
                }
            })
            .expect("spawn the normal-priority runner");
        tx
    });
}

/// Runs `f` at normal scheduling priority and returns its result, so the
/// threads and processes `f` creates start at normal priority: inline on a
/// normal-priority caller, otherwise on the runner thread from
/// [`start_normal_priority_runner`]. A background caller runs `f` inline
/// when no runner was started. A panic in `f` resumes on the caller.
pub fn run_at_normal_priority<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    if !is_background() {
        start_normal_priority_runner();
        return f();
    }
    let Some(runner) = NORMAL_RUNNER.get() else {
        log::warn!("normal-priority runner not started; running at background priority");
        return f();
    };
    let (done_tx, done_rx) = mpsc::sync_channel(1);
    let job: Job = Box::new(move || {
        let _ = done_tx.send(std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)));
    });
    if runner.send(job).is_err() {
        panic!("normal-priority runner stopped");
    }
    match done_rx.recv() {
        Ok(Ok(v)) => v,
        Ok(Err(p)) => std::panic::resume_unwind(p),
        Err(_) => panic!("normal-priority runner dropped the job"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_is_half_the_cores_at_least_one() {
        assert_eq!(half_cores(0), 1);
        assert_eq!(half_cores(1), 1);
        assert_eq!(half_cores(2), 1);
        assert_eq!(half_cores(3), 1);
        assert_eq!(half_cores(4), 2);
        assert_eq!(half_cores(12), 6);
        assert_eq!(half_cores(16), 8);
        assert_eq!(half_cores(64), 32);
        let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap();
        assert_eq!(finalize_cpu_budget(), (cores / 2).max(1));
    }

    #[test]
    fn pool_is_budget_sized_and_marked_background() {
        let pool = finalize_pool();
        assert_eq!(pool.current_num_threads(), finalize_cpu_budget());
        assert!(pool.install(|| BACKGROUND.with(|b| b.get())));
    }

    #[test]
    fn run_in_background_spawns_once_then_runs_inline() {
        let caller = std::thread::current().id();
        let (outer, inner) = run_in_background(|| {
            let outer = std::thread::current().id();
            let inner = run_in_background(|| std::thread::current().id());
            (outer, inner)
        });
        assert_ne!(outer, caller, "the caller's thread must stay at its own priority");
        assert_eq!(inner, outer, "nested calls run inline on the background thread");
        assert!(!BACKGROUND.with(|b| b.get()), "the caller is never marked background");
    }

    #[test]
    fn background_jobs_run_one_at_a_time() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static RUNNING: AtomicUsize = AtomicUsize::new(0);
        static PEAK: AtomicUsize = AtomicUsize::new(0);
        let jobs: Vec<_> = (0..4)
            .map(|_| {
                std::thread::spawn(|| {
                    run_in_background(|| {
                        let now = RUNNING.fetch_add(1, Ordering::SeqCst) + 1;
                        PEAK.fetch_max(now, Ordering::SeqCst);
                        std::thread::sleep(std::time::Duration::from_millis(20));
                        RUNNING.fetch_sub(1, Ordering::SeqCst);
                    })
                })
            })
            .collect();
        for j in jobs {
            j.join().unwrap();
        }
        assert_eq!(PEAK.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn normal_priority_work_leaves_the_background_thread() {
        start_normal_priority_runner();
        let (background, normal, result) = run_in_background(|| {
            let background = std::thread::current().id();
            let (normal, flagged) = run_at_normal_priority(|| (std::thread::current().id(), is_background()));
            (background, normal, flagged)
        });
        assert_ne!(background, normal, "work moves to the runner thread");
        assert!(!result, "the runner thread runs at normal priority");
        let here = std::thread::current().id();
        assert_eq!(run_at_normal_priority(move || std::thread::current().id()), here, "a normal caller runs inline");
    }

    #[test]
    fn run_at_normal_priority_resumes_panics_on_the_caller() {
        start_normal_priority_runner();
        let r = std::panic::catch_unwind(|| run_in_background(|| run_at_normal_priority(|| panic!("boom"))));
        assert!(r.is_err());
        assert_eq!(run_in_background(|| run_at_normal_priority(|| 7)), 7, "the runner survives a panic");
    }

    /// The nice value the test process started at, read on a test thread,
    /// which is never lowered. Refuses a start at nice 10 or above.
    #[cfg(target_os = "linux")]
    fn starting_nice() -> i32 {
        let nice = unsafe { libc::getpriority(libc::PRIO_PROCESS, 0) };
        assert!(nice < 10, "the test process starts at nice {nice}; run it below nice 10");
        nice
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_threads_created_through_the_runner_are_not_niced() {
        let start = starting_nice();
        start_normal_priority_runner();
        let (own, child) = run_in_background(|| {
            run_at_normal_priority(|| {
                let own = unsafe { libc::getpriority(libc::PRIO_PROCESS, 0) };
                let child = std::thread::spawn(|| unsafe { libc::getpriority(libc::PRIO_PROCESS, 0) })
                    .join()
                    .unwrap();
                (own, child)
            })
        });
        assert_eq!((own, child), (start, start));
    }

    #[test]
    fn run_in_background_resumes_panics_on_the_caller() {
        let r = std::panic::catch_unwind(|| run_in_background(|| panic!("boom")));
        assert!(r.is_err());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_background_thread_and_its_children_are_niced() {
        let start = starting_nice();
        let (own, child) = run_in_background(|| {
            let own = unsafe { libc::getpriority(libc::PRIO_PROCESS, 0) };
            let child = std::thread::spawn(|| unsafe { libc::getpriority(libc::PRIO_PROCESS, 0) })
                .join()
                .unwrap();
            (own, child)
        });
        assert_eq!(own, 10);
        assert_eq!(child, 10, "threads created after the drop inherit nice");
        assert_eq!(unsafe { libc::getpriority(libc::PRIO_PROCESS, 0) }, start);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_pool_workers_are_niced_and_the_caller_is_not() {
        let start = starting_nice();
        let pool = finalize_pool();
        let nice = pool.install(|| unsafe { libc::getpriority(libc::PRIO_PROCESS, 0) });
        assert_eq!(nice, 10);
        assert_eq!(unsafe { libc::getpriority(libc::PRIO_PROCESS, 0) }, start);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_background_thread_runs_at_utility_qos() {
        unsafe extern "C" {
            fn qos_class_self() -> std::ffi::c_uint;
        }
        let (own, child) = run_in_background(|| {
            let own = unsafe { qos_class_self() };
            let child = std::thread::spawn(|| unsafe { qos_class_self() }).join().unwrap();
            (own, child)
        });
        assert_eq!(own, 0x11, "QOS_CLASS_UTILITY");
        assert_ne!(child, 0x11, "QoS stays on the lowered thread; children lower themselves");
        let pool = finalize_pool();
        assert_eq!(pool.install(|| unsafe { qos_class_self() }), 0x11);
    }
}
