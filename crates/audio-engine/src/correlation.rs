// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
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
///
/// Every lag's Pearson coefficient comes from one FFT cross-correlation plus
/// prefix sums over the overlap windows, in f64: O(n log n) for any search
/// width.
pub fn best_correlation(a: &[f32], b: &[f32], search_samples: i32) -> (i32, f32) {
    let n = a.len().min(b.len());
    let mut best_lag = 0i32;
    let mut best_abs = 0f32;
    let mut best_signed = 0f32;
    if n == 0 {
        return (0, 0.0);
    }
    let (a, b) = (&a[..n], &b[..n]);
    let cross = cross_sums(a, b);
    let (sa, saa) = prefix_sums(a);
    let (sb, sbb) = prefix_sums(b);
    let max_lag = search_samples.min(n as i32 - 1);
    for lag in -max_lag..=max_lag {
        let l = lag.unsigned_abs() as usize;
        let len = n - l;
        // Overlap windows as in `cross_correlation_at_lag`.
        let (a0, b0) = if lag >= 0 { (0, l) } else { (l, 0) };
        let m = len as f64;
        let sum_a = sa[a0 + len] - sa[a0];
        let sum_b = sb[b0 + len] - sb[b0];
        let num = cross[lag.rem_euclid(cross.len() as i32) as usize] - sum_a * sum_b / m;
        let den_a = saa[a0 + len] - saa[a0] - sum_a * sum_a / m;
        let den_b = sbb[b0 + len] - sbb[b0] - sum_b * sum_b / m;
        let den = (den_a.max(0.0) * den_b.max(0.0)).sqrt();
        let c = if den < f32::EPSILON as f64 { 0.0 } else { (num / den) as f32 };
        if c.abs() > best_abs {
            best_abs = c.abs();
            best_signed = c;
            best_lag = lag;
        }
    }
    (-best_lag, best_signed)
}

/// `out[lag mod len] = Σ_i a[i] · b[i + lag]` for every lag in `-(n-1)..n`.
fn cross_sums(a: &[f32], b: &[f32]) -> Vec<f64> {
    use rustfft::{num_complex::Complex, FftPlanner};
    let size = (2 * a.len()).next_power_of_two();
    let mut planner = FftPlanner::<f64>::new();
    let fwd = planner.plan_fft_forward(size);
    let inv = planner.plan_fft_inverse(size);
    let spectrum = |x: &[f32]| {
        let mut buf = vec![Complex::new(0.0, 0.0); size];
        for (d, &s) in buf.iter_mut().zip(x) {
            d.re = s as f64;
        }
        fwd.process(&mut buf);
        buf
    };
    let fa = spectrum(a);
    let mut fb = spectrum(b);
    for (y, x) in fb.iter_mut().zip(&fa) {
        *y *= x.conj();
    }
    inv.process(&mut fb);
    fb.iter().map(|c| c.re / size as f64).collect()
}

/// Running Σx and Σx², with a leading zero.
fn prefix_sums(x: &[f32]) -> (Vec<f64>, Vec<f64>) {
    let mut s = Vec::with_capacity(x.len() + 1);
    let mut ss = Vec::with_capacity(x.len() + 1);
    let (mut acc, mut acc2) = (0.0f64, 0.0f64);
    s.push(0.0);
    ss.push(0.0);
    for &v in x {
        acc += v as f64;
        acc2 += v as f64 * v as f64;
        s.push(acc);
        ss.push(acc2);
    }
    (s, ss)
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
