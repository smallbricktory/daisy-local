//! In-app local-whisper speed benchmark, using the same `WhisperContext`
//! path as production: full-file decode + one 30s-window decode.

use std::path::Path;
use std::time::Instant;

pub const SR: usize = 16_000;

#[derive(Debug, Clone, PartialEq)]
pub struct BenchReport {
    pub backend: &'static str,
    pub threads: i32,
    pub audio_secs: f64,
    pub model_load_secs: f64,
    /// Full-file decode (the batch path ≈ finalize).
    pub full_wall_secs: f64,
    pub full_xrt: f64,
    /// One 30s-window decode (the streaming latency floor / encoder cost).
    pub window30_wall_secs: f64,
}

impl BenchReport {
    /// One-line human summary (also used for the toast).
    pub fn summary(&self) -> String {
        format!(
            "{} · {} threads · full-file {:.1}x realtime ({:.2}s for {:.0}s audio) · 30s window {:.2}s · model load {:.2}s",
            self.backend,
            self.threads,
            self.full_xrt,
            self.full_wall_secs,
            self.audio_secs,
            self.window30_wall_secs,
            self.model_load_secs,
        )
    }
}

/// Compile-time backend label. Backends are unconditional per platform
/// (see Cargo.toml target deps): Metal on macOS, Vulkan elsewhere — with
/// runtime CPU fallback when no Vulkan device is present.
pub fn backend_label() -> &'static str {
    if cfg!(target_os = "macos") {
        "Metal GPU"
    } else {
        "Vulkan GPU (CPU fallback)"
    }
}

fn decode(ctx: &whisper_rs::WhisperContext, samples: &[f32], n_threads: i32) -> anyhow::Result<f64> {
    let mut state = ctx
        .create_state()
        .map_err(|e| anyhow::anyhow!("create_state: {e}"))?;
    let mut params =
        whisper_rs::FullParams::new(whisper_rs::SamplingStrategy::Greedy { best_of: 1 });
    params.set_n_threads(n_threads);
    params.set_translate(false);
    params.set_print_special(false);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);
    params.set_language(Some("en"));
    let t = Instant::now();
    state
        .full(params, samples)
        .map_err(|e| anyhow::anyhow!("whisper full: {e}"))?;
    Ok(t.elapsed().as_secs_f64())
}

/// Run a compact speed benchmark: full-file decode + one 30s-window decode.
pub fn run_speed_bench(
    model_path: &Path,
    samples: &[f32],
    n_threads: i32,
) -> anyhow::Result<BenchReport> {
    let audio_secs = samples.len() as f64 / SR as f64;

    let t = Instant::now();
    let ctx = whisper_rs::WhisperContext::new_with_params(
        model_path
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("model path not UTF-8"))?,
        whisper_rs::WhisperContextParameters::default(),
    )
    .map_err(|e| anyhow::anyhow!("load model {}: {e}", model_path.display()))?;
    let model_load_secs = t.elapsed().as_secs_f64();

    let full_wall = decode(&ctx, samples, n_threads)?;

    // 30s window starting a quarter in.
    let start = (samples.len() / 4).min(samples.len());
    let end = (start + 30 * SR).min(samples.len());
    let window30_wall = if end > start {
        decode(&ctx, &samples[start..end], n_threads)?
    } else {
        0.0
    };

    Ok(BenchReport {
        backend: backend_label(),
        threads: n_threads,
        audio_secs,
        model_load_secs,
        full_wall_secs: full_wall,
        full_xrt: if full_wall > 0.0 { audio_secs / full_wall } else { 0.0 },
        window30_wall_secs: window30_wall,
    })
}

/// Deterministic ~`secs`-second 16 kHz mono speech-like test signal: a voiced
/// buzz (pitch + harmonics spanning three formant bands) under a ~3.5 Hz
/// syllabic envelope that dips to near-silence (pauses). No RNG.
pub fn synthetic_test_audio(secs: usize) -> Vec<f32> {
    use std::f32::consts::PI;
    let n = secs * SR;
    let mut out = Vec::with_capacity(n);
    // (frequency Hz, gain) partials: f0 buzz + three rough formant bands.
    let partials = [(120.0_f32, 0.6_f32), (500.0, 1.0), (1500.0, 0.5), (2500.0, 0.25)];
    let norm = 0.09 / partials.iter().map(|&(_, g)| g).sum::<f32>(); // peak ≈ -21 dBFS
    for i in 0..n {
        let t = i as f32 / SR as f32;
        let env = (0.5 - 0.5 * (2.0 * PI * 3.5 * t).cos()).powf(1.5); // syllabic, with pauses
        let mut s = 0.0_f32;
        for &(f, g) in &partials {
            s += g * (2.0 * PI * f * t).sin();
        }
        out.push(s * env * norm);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthetic_audio_length_and_range() {
        let a = synthetic_test_audio(2);
        assert_eq!(a.len(), 2 * SR);
        assert!(a.iter().all(|&x| x.abs() <= 0.1001));
        assert!(a.iter().any(|&x| x.abs() > 0.001)); // not all zero
    }

    #[test]
    fn report_summary_mentions_backend_and_xrt() {
        let r = BenchReport {
            backend: "CPU",
            threads: 6,
            audio_secs: 60.0,
            model_load_secs: 0.5,
            full_wall_secs: 3.0,
            full_xrt: 20.0,
            window30_wall_secs: 1.6,
        };
        let s = r.summary();
        assert!(s.contains("CPU") && s.contains("20.0x") && s.contains("6 threads"), "{s}");
    }
}
