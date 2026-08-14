//! GCC-PHAT cross-correlation core, shared by the live AEC alignment probe
//! (`aec::align`) and the finalize-side echo-direction oracle
//! (`transcript::echo_direction`). Callers keep their own units, thresholds,
//! and pre-gates.

use rustfft::{num_complex::Complex, FftPlanner};

thread_local! {
    static PLANNER: std::cell::RefCell<FftPlanner<f32>> =
        std::cell::RefCell::new(FftPlanner::<f32>::new());
}

/// Lag (in samples) of `a`'s content relative to `b`, at the strongest
/// GCC-PHAT peak within `±max_lag`, with the peak-to-mean confidence ratio.
/// Positive lag means `a` TRAILS `b` by that many samples (swap the
/// arguments to flip the sign convention).
///
/// `min_rms`: when set, either input below that RMS carries no usable
/// signal and the probe returns `None`. Also `None` when the inputs are
/// empty, `max_lag` is 0, or the correlation degenerates.
pub fn gcc_phat_core(
    a: &[f32],
    b: &[f32],
    max_lag: usize,
    min_rms: Option<f32>,
) -> Option<(i64, f32)> {
    let len = a.len().max(b.len());
    if len == 0 || max_lag == 0 {
        return None;
    }
    if let Some(floor) = min_rms {
        if rms(a) < floor || rms(b) < floor {
            return None;
        }
    }
    let n = (len + max_lag).next_power_of_two();
    // The planner caches its twiddle factors; a fresh one per call recomputes
    // them every time. Thread-local because the probe runs on several.
    let (fft, ifft) = PLANNER.with(|p| {
        let mut p = p.borrow_mut();
        (p.plan_fft_forward(n), p.plan_fft_inverse(n))
    });

    let mut fa: Vec<Complex<f32>> = a
        .iter()
        .map(|&x| Complex::new(x, 0.0))
        .chain(std::iter::repeat(Complex::ZERO))
        .take(n)
        .collect();
    let mut fb: Vec<Complex<f32>> = b
        .iter()
        .map(|&x| Complex::new(x, 0.0))
        .chain(std::iter::repeat(Complex::ZERO))
        .take(n)
        .collect();
    fft.process(&mut fa);
    fft.process(&mut fb);

    // PHAT weighting: keep phase only — robust to level/spectral differences
    // between the two signals.
    let mut r: Vec<Complex<f32>> = fa
        .iter()
        .zip(&fb)
        .map(|(x, y)| {
            let c = x * y.conj();
            let m = c.norm();
            if m > 1e-12 { c / m } else { Complex::ZERO }
        })
        .collect();
    ifft.process(&mut r);

    // Circular correlation: lags [-max_lag, max_lag] live at the wrap point.
    let mut best_idx = 0usize;
    let mut best = 0.0f32;
    let mut sum = 0.0f32;
    for (k, idx) in (0..=2 * max_lag).map(|k| (k, (n - max_lag + k) % n)) {
        let v = r[idx].norm();
        sum += v;
        if v > best {
            best = v;
            best_idx = k;
        }
    }
    let mean = sum / (2 * max_lag + 1) as f32;
    if mean <= 0.0 {
        return None;
    }
    Some((best_idx as i64 - max_lag as i64, best / mean))
}

fn rms(s: &[f32]) -> f32 {
    if s.is_empty() {
        return 0.0;
    }
    (s.iter().map(|&x| x * x).sum::<f32>() / s.len() as f32).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noise(n: usize, seed: u64) -> Vec<f32> {
        // Deterministic pseudo-noise (xorshift) — broadband, correlation-friendly.
        let mut x = seed | 1;
        (0..n)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                ((x as i32) as f32 / i32::MAX as f32) * 0.3
            })
            .collect()
    }

    #[test]
    fn recovers_a_known_delay_with_sign() {
        let src = noise(16_000, 7);
        let mut delayed = vec![0.0f32; 800];
        delayed.extend(src.iter().map(|&x| x * 0.2));
        delayed.truncate(16_000);
        let (lag, ratio) = gcc_phat_core(&delayed, &src, 4_000, None).expect("confident lag");
        assert_eq!(lag, 800, "delayed copy trails the source by +800");
        assert!(ratio > 8.0, "clean synthetic delay must be decisive, got {ratio}");
        // Swapping the arguments flips the sign.
        let (lag, _) = gcc_phat_core(&src, &delayed, 4_000, None).expect("confident lag");
        assert_eq!(lag, -800);
    }

    #[test]
    fn rms_gate_rejects_silence() {
        assert!(gcc_phat_core(&vec![0.0; 16_000], &noise(16_000, 3), 4_000, Some(0.002)).is_none());
        assert!(gcc_phat_core(&noise(16_000, 3), &vec![0.0; 16_000], 4_000, Some(0.002)).is_none());
        // Without a gate, silence still yields no confident peak on
        // unrelated inputs, but the call itself is allowed.
        assert!(gcc_phat_core(&[], &noise(16, 3), 4, None).is_none());
        assert!(gcc_phat_core(&noise(16, 3), &noise(16, 5), 0, None).is_none());
    }
}
