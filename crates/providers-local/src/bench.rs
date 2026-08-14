//! Whisper backend identification for the diagnostics examples.

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

