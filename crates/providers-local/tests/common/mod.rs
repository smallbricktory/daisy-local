// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Helpers shared by the integration tests.

/// Word error rate of `h` against the reference `r`: word-level edit
/// distance over the reference length.
pub fn wer(r: &[String], h: &[String]) -> f64 {
    let mut prev: Vec<usize> = (0..=h.len()).collect();
    for (i, rw) in r.iter().enumerate() {
        let mut cur = vec![i + 1; h.len() + 1];
        for (j, hw) in h.iter().enumerate() {
            cur[j + 1] = (prev[j] + usize::from(rw != hw)).min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        prev = cur;
    }
    prev[h.len()] as f64 / r.len().max(1) as f64
}
