// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! ONNX Runtime thread manager whose threads drop to background priority
//! before running their work.

/// Pool threads [`BackgroundThreads`] has started in this test process.
#[cfg(test)]
pub(crate) static SPAWNED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

pub struct BackgroundThreads;

impl ort::environment::ThreadManager for BackgroundThreads {
    type Thread = std::thread::JoinHandle<()>;

    fn create(&self, work: impl FnOnce() + Send + 'static) -> ort::Result<Self::Thread> {
        std::thread::Builder::new()
            .name("ort-background".into())
            .spawn(move || {
                transcript::cpu::lower_to_background_priority();
                #[cfg(test)]
                SPAWNED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                work()
            })
            .map_err(|e| ort::Error::new(format!("spawn ORT thread: {e}")))
    }

    fn join(thread: Self::Thread) -> ort::Result<()> {
        thread.join().map_err(|_| ort::Error::new("ORT thread panicked"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ort::environment::ThreadManager;

    #[test]
    fn background_thread_manager_runs_the_work() {
        let (tx, rx) = std::sync::mpsc::channel();
        let t = BackgroundThreads.create(move || tx.send(std::thread::current().id()).unwrap()).unwrap();
        <BackgroundThreads as ThreadManager>::join(t).unwrap();
        assert_ne!(rx.recv().unwrap(), std::thread::current().id());
    }
}
