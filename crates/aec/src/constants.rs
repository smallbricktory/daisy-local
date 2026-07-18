//! Compile-time constants for the DTLN-aec 256-variant.

use std::path::PathBuf;

/// Sample rate the model expects.
pub const SAMPLE_RATE: u32 = 16_000;

/// Block size for FFT (samples). 512 samples = 32 ms at 16 kHz.
pub const BLOCK_SIZE: usize = 512;

/// Block hop / shift (samples). 128 samples = 8 ms at 16 kHz; 75 % overlap.
pub const BLOCK_SHIFT: usize = 128;

/// Number of bins in the real FFT of a `BLOCK_SIZE` frame.
/// = BLOCK_SIZE / 2 + 1 = 257.
pub const FFT_BINS: usize = BLOCK_SIZE / 2 + 1;

/// LSTM hidden size for the 256 variant.
pub const STATE_SIZE: usize = 256;

/// State tensor shape: (batch=1, layers=2, hidden=STATE_SIZE, h_or_c=2).
pub const STATE_SHAPE: [usize; 4] = [1, 2, STATE_SIZE, 2];

/// Resolve the model directory.
///
/// Priority:
///   1. the `DAISY_MODEL_DIR` env var
///   2. `models/dtln-aec/` next to the executable
///   3. repo-relative `models/dtln-aec/`
pub fn model_dir() -> PathBuf {
    if let Ok(p) = std::env::var("DAISY_MODEL_DIR") {
        return PathBuf::from(p);
    }
    if let Some(p) = exe_relative_model_dir("dtln-aec") {
        return p;
    }
    // crates/aec/src/constants.rs → repo root via two parents
    let crate_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    crate_dir
        .parent() // crates/
        .and_then(|p| p.parent()) // repo root
        .map(|p| p.join("models/dtln-aec"))
        .unwrap_or_else(|| PathBuf::from("models/dtln-aec"))
}

/// `models/<sub>/` next to the running executable. Returns it only if it
/// exists.
fn exe_relative_model_dir(sub: &str) -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?.join("models").join(sub);
    if dir.is_dir() {
        Some(dir)
    } else {
        None
    }
}

/// ONNX intra-op thread count. Default `min(4, max(2, cpus / 2))`; the
/// `DAISY_AEC_THREADS` env var (positive integer) overrides it.
pub fn intra_op_threads() -> usize {
    if let Ok(s) = std::env::var("DAISY_AEC_THREADS") {
        if let Ok(n) = s.parse::<usize>() {
            if n > 0 {
                return n;
            }
        }
    }
    let cpus = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    (cpus / 2).clamp(2, 4)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_dir_resolves_to_repo_models_dir() {
        let dir = model_dir();
        // Either an absolute path ending in "models/dtln-aec", or the
        // env-var override took precedence.
        let s = dir.to_string_lossy();
        assert!(
            s.ends_with("models/dtln-aec"),
            "model_dir() = {dir:?}, expected to end with models/dtln-aec"
        );
    }

    #[test]
    fn state_shape_matches_state_size() {
        assert_eq!(STATE_SHAPE, [1, 2, STATE_SIZE, 2]);
    }

    #[test]
    fn fft_bins_correct() {
        assert_eq!(FFT_BINS, BLOCK_SIZE / 2 + 1);
        assert_eq!(FFT_BINS, 257);
    }
}
