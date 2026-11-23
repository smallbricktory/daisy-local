// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Periodic self-process CPU/RSS telemetry logged to the regular log stream,
//! from the same sampler that writes each recording's `telemetry.jsonl`.

use std::time::Duration;

const SAMPLE_INTERVAL_SECONDS: u64 = 15;

/// Spawns the sampler thread. Logs one line every SAMPLE_INTERVAL_SECONDS at
/// info! level. Called once during app startup; the thread lives for the
/// app's lifetime.
pub fn spawn() {
    let _ = std::thread::Builder::new().name("daisy-telemetry-log".into()).spawn(|| {
        log::info!("telemetry: starting sampler every {SAMPLE_INTERVAL_SECONDS}s");
        let mut sampler = recording::proc_stats::ProcSampler::new();
        loop {
            std::thread::sleep(Duration::from_secs(SAMPLE_INTERVAL_SECONDS));
            log::info!("telemetry: {}", format_line(&sampler.sample()));
        }
    });
}

fn format_line(s: &recording::telemetry::ProcRecord) -> String {
    let threads = s.threads.map(|n| n.to_string()).unwrap_or_else(|| "?".into());
    format!("cpu={:.1}% rss={} MB threads={threads}", s.cpu_pct, s.rss_mb.round() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_log_line_keeps_its_shape() {
        let s = recording::telemetry::ProcRecord { cpu_pct: 4.25, rss_mb: 812.6, threads: Some(41), thread_cpu: vec![] };
        assert_eq!(format_line(&s), "cpu=4.2% rss=813 MB threads=41");
        let s = recording::telemetry::ProcRecord { threads: None, ..s };
        assert_eq!(format_line(&s), "cpu=4.2% rss=813 MB threads=?");
    }
}
