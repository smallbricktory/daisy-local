// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Linux: the OpenBLAS pool threads a background finalize job creates by
//! raising the cap start at normal priority, so live transcription reuses
//! them at normal priority. Its own test binary: it counts every thread in
//! the process.
#![cfg(target_os = "linux")]

fn niced_threads() -> usize {
    std::fs::read_dir("/proc/self/task")
        .expect("/proc/self/task")
        .filter_map(|e| std::fs::read_to_string(e.ok()?.path().join("stat")).ok())
        .filter(|stat| {
            // Field 19 (nice) is the 17th after the ") " that ends the comm.
            let rest = &stat[stat.rfind(')').map(|i| i + 2).unwrap_or(0)..];
            rest.split(' ').nth(16) == Some("10")
        })
        .count()
}

#[test]
fn finalize_cap_creates_the_openblas_pool_at_normal_priority() {
    tauri_app_core::openblas::cap_openblas_threads(1);
    transcript::cpu::start_normal_priority_runner();
    transcript::cpu::run_in_background(|| {
        let _cap = tauri_app_core::openblas::finalize_cap();
    });
    assert_eq!(niced_threads(), 0, "no thread outlives the job at background priority");
}
