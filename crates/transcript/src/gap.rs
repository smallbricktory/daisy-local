//! Compute the uncovered time spans of a live transcript. Mirror of
//! [`crate::promote::coverage_is_lossless`]: same gap/tolerance rules, but
//! returns the intervals to patch. Gaps `<= tolerance` are treated as
//! silence and skipped.

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

#[cfg(test)]
mod tests {
    use super::*;

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
