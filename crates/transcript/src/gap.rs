// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Compute the uncovered time spans of a live transcript: the intervals to
//! patch. Gaps `<= tolerance` are treated as silence and skipped.

/// Uncovered `(start_ms, end_ms)` intervals within `[0, total_ms]` where no
/// covered span exists and the hole exceeds `gap_tolerance_ms`. Input spans need
/// not be sorted or disjoint. An empty result means the transcript is fully
/// covered.
pub fn uncovered_spans(
    spans: &[(u32, u32)],
    total_ms: u32,
    gap_tolerance_ms: u32,
) -> Vec<(u32, u32)> {
    let mut out = Vec::new();
    if total_ms == 0 {
        return out;
    }
    let mut spans: Vec<(u32, u32)> = spans.iter().copied().filter(|(s, e)| e > s).collect();
    spans.sort_by_key(|s| s.0);

    let mut cursor: u32 = 0;
    for &(s, e) in &spans {
        if s > cursor.saturating_add(gap_tolerance_ms) {
            out.push((cursor, s));
        }
        cursor = cursor.max(e);
    }
    if total_ms.saturating_sub(cursor) > gap_tolerance_ms {
        out.push((cursor, total_ms));
    }
    out
}

/// Pad each `(start, end)` region by `pad_ms` on both sides, clip to `range`,
/// and merge regions whose padded edges overlap or touch. The result is
/// sorted and disjoint; no regions give no slices.
pub fn pad_and_merge(regions: &[(u32, u32)], pad_ms: u32, range: (u32, u32)) -> Vec<(u32, u32)> {
    let mut out: Vec<(u32, u32)> = Vec::new();
    for &(s, e) in regions {
        let (s, e) = (s.saturating_sub(pad_ms).max(range.0), e.saturating_add(pad_ms).min(range.1));
        if e <= s {
            continue;
        }
        match out.last_mut() {
            Some(last) if s <= last.1 => last.1 = last.1.max(e),
            _ => out.push((s, e)),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pad_and_merge_pads_clips_and_joins_overlapping_regions() {
        let range = (10_000, 60_000);
        // Padding stays inside the range.
        assert_eq!(pad_and_merge(&[(10_000, 11_000)], 1_000, range), vec![(10_000, 12_000)]);
        assert_eq!(pad_and_merge(&[(59_500, 60_000)], 1_000, range), vec![(58_500, 60_000)]);
        // Regions whose padded edges overlap or touch merge.
        assert_eq!(
            pad_and_merge(&[(20_000, 21_000), (23_000, 24_000)], 1_000, range),
            vec![(19_000, 25_000)]
        );
        // Regions further apart stay separate.
        assert_eq!(
            pad_and_merge(&[(20_000, 21_000), (25_000, 26_000)], 1_000, range),
            vec![(19_000, 22_000), (24_000, 27_000)]
        );
        assert!(pad_and_merge(&[], 1_000, range).is_empty());
    }

    #[test]
    fn no_gaps_when_contiguous() {
        let s = vec![(0u32, 20_000u32), (20_000, 60_000)];
        assert!(uncovered_spans(&s, 60_000, 10_000).is_empty());
    }

    #[test]
    fn returns_internal_gap_over_tolerance() {
        // 15.3s hole.
        let s = vec![(0u32, 326_800u32), (342_100, 2_574_000)];
        let g = uncovered_spans(&s, 2_574_000, 10_000);
        assert_eq!(g, vec![(326_800, 342_100)]);
    }

    #[test]
    fn ignores_small_gaps_as_silence() {
        let s = vec![(0u32, 10_000u32), (15_000, 40_000)]; // 5s < tol
        assert!(uncovered_spans(&s, 40_000, 10_000).is_empty());
    }

    #[test]
    fn returns_tail_gap() {
        let s = vec![(0u32, 20_000u32)];
        assert_eq!(uncovered_spans(&s, 60_000, 10_000), vec![(20_000, 60_000)]);
    }

    #[test]
    fn returns_leading_gap() {
        let s = vec![(30_000u32, 60_000u32)];
        assert_eq!(uncovered_spans(&s, 60_000, 10_000), vec![(0, 30_000)]);
    }

    #[test]
    fn overlapping_unsorted_spans_are_normalized() {
        let s = vec![(40_000u32, 60_000u32), (0, 20_000), (10_000, 45_000)];
        // union covers 0..60000 contiguously → no gaps.
        assert!(uncovered_spans(&s, 60_000, 10_000).is_empty());
    }

    #[test]
    fn empty_or_zero_total() {
        assert!(uncovered_spans(&[], 0, 10_000).is_empty());
        // No coverage at all over a real span → one big gap.
        assert_eq!(uncovered_spans(&[], 60_000, 10_000), vec![(0, 60_000)]);
    }
}
