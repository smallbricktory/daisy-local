//! Cross-correlation helpers used by the dual-stream capture invariant tests.
//!
//! Pure-function module — no I/O, no async, no hardware access. Operates on
//! `&[f32]` slices in [-1, 1] range (caller converts from int16 if needed).

/// Pearson correlation between `a` and `b` shifted by `lag` samples relative to
/// `a`. Positive lag means `b` is "advanced" — `b[i + lag]` aligns with `a[i]`.
/// Negative lag means `b` trails `a`.
///
/// Returns 0.0 when one of the slices has near-zero variance or when the
/// overlap window is empty.
pub fn cross_correlation_at_lag(a: &[f32], b: &[f32], lag: i32) -> f32 {
    let n = a.len().min(b.len());
    if n == 0 {
        return 0.0;
    }
    let (a_start, b_start, len) = if lag >= 0 {
        let l = lag as usize;
        if l >= n {
            return 0.0;
        }
        (0, l, n - l)
    } else {
        let l = (-lag) as usize;
        if l >= n {
            return 0.0;
        }
        (l, 0, n - l)
    };

    let a_w = &a[a_start..a_start + len];
    let b_w = &b[b_start..b_start + len];
    pearson(a_w, b_w)
}

/// Search for the lag in `[-search_samples, +search_samples]` that maximizes
/// `|cross_correlation_at_lag(a, b, lag)|`. Returns `(best_lag, signed_peak)`.
pub fn best_correlation(a: &[f32], b: &[f32], search_samples: i32) -> (i32, f32) {
    let mut best_lag = 0i32;
    let mut best_abs = 0f32;
    let mut best_signed = 0f32;
    for lag in -search_samples..=search_samples {
        let c = cross_correlation_at_lag(a, b, lag);
        if c.abs() > best_abs {
            best_abs = c.abs();
            best_signed = c;
            best_lag = lag;
        }
    }
    (-best_lag, best_signed)
}

fn pearson(a: &[f32], b: &[f32]) -> f32 {
    debug_assert_eq!(a.len(), b.len());
    let n = a.len() as f32;
    if n == 0.0 {
        return 0.0;
    }
    let mean_a = a.iter().sum::<f32>() / n;
    let mean_b = b.iter().sum::<f32>() / n;
    let mut num = 0.0f32;
    let mut den_a = 0.0f32;
    let mut den_b = 0.0f32;
    for (&x, &y) in a.iter().zip(b.iter()) {
        let dx = x - mean_a;
        let dy = y - mean_b;
        num += dx * dy;
        den_a += dx * dx;
        den_b += dy * dy;
    }
    let den = (den_a * den_b).sqrt();
    if den < f32::EPSILON {
        return 0.0;
    }
    num / den
}
