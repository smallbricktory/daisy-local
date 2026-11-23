// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Nemotron speech streaming (English, 0.6B, 560 ms cache-aware RNNT) on
//! ONNX Runtime CPU: log-mel front end, encoder/decoder/joiner sessions,
//! greedy decode with per-token times, word assembly, and the live and batch
//! transcribers.

pub mod batch;
pub mod error;
pub mod features;
pub mod model;
pub mod realtime;
pub mod stream;
pub mod words;

pub use error::NemotronError;

/// Session configuration for one model instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NemotronOpts {
    /// Intra-op threads for the encoder session.
    pub threads: usize,
    /// ONNX Runtime pool threads start at background priority.
    pub background: bool,
}

impl NemotronOpts {
    /// One live track: a quarter of the cores, between two and four threads.
    pub fn live() -> Self {
        let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
        Self { threads: live_threads(cores), background: false }
    }

    /// Batch decoding inside the background finalize budget.
    pub fn background() -> Self {
        Self { threads: transcript::cpu::finalize_cpu_budget(), background: true }
    }
}

fn live_threads(cores: usize) -> usize {
    (cores / 4).clamp(2, 4)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_threads_scale_with_cores_within_two_to_four() {
        assert_eq!(live_threads(2), 2);
        assert_eq!(live_threads(8), 2);
        assert_eq!(live_threads(12), 3);
        assert_eq!(live_threads(16), 4);
        assert_eq!(live_threads(64), 4);
    }

    #[test]
    fn background_opts_take_the_finalize_budget() {
        let o = NemotronOpts::background();
        assert_eq!(o.threads, transcript::cpu::finalize_cpu_budget());
        assert!(o.background);
        assert!(!NemotronOpts::live().background);
    }
}
