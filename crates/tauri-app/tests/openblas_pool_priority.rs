// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Linux: the OpenBLAS pool threads a background finalize job creates by
//! raising the cap start at normal priority, so live transcription reuses
//! them at normal priority. Its own test binary: it counts every thread in
//! the process.
#![cfg(target_os = "linux")]

/// `tid comm state` for each thread of this process at nice 10.
fn niced_threads() -> Vec<String> {
    std::fs::read_dir("/proc/self/task")
        .expect("/proc/self/task")
        .filter_map(|e| {
            let e = e.ok()?;
            let stat = std::fs::read_to_string(e.path().join("stat")).ok()?;
            let open = stat.find('(')?;
            let close = stat.rfind(')')?;
            // Field 19 (nice) is the 17th after the ") " that ends the comm.
            let rest: Vec<&str> = stat[close + 2..].split(' ').collect();
            (rest.get(16) == Some(&"10")).then(|| {
                format!("{} {} {}", e.file_name().to_string_lossy(), &stat[open + 1..close], rest[0])
            })
        })
        .collect()
}

#[test]
fn finalize_cap_creates_the_openblas_pool_at_normal_priority() {
    tauri_app_core::openblas::cap_openblas_threads(1);
    transcript::cpu::start_normal_priority_runner();
    transcript::cpu::run_in_background(|| {
        let _cap = tauri_app_core::openblas::finalize_cap();
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    let mut niced = niced_threads();
    while !niced.is_empty() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(10));
        niced = niced_threads();
    }
    assert!(niced.is_empty(), "threads still at background priority 2 s after the job: {niced:?}");
}
