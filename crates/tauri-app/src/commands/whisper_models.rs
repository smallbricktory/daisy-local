//! Local Whisper model management: list installed/available sizes, switch the
//! active model, delete downloaded ones. The download itself lives in
//! `main.rs` (`download_whisper_model`).
//!
//! Layout: `base.en` ships bundled in the app's Resources (read-only, never
//! deletable). User-downloaded models live in
//! `<profile>/models/ggml-<size>.bin`. The active model is
//! `settings.whisper_model_path` (None = the bundled default).

use crate::error::{AppError, Result};
use crate::state::AppState;
use serde::Serialize;

/// The size that ships in the app bundle. Always present, never deletable.
pub const BUNDLED_SIZE: &str = "base.en";

#[derive(Debug, Serialize)]
pub struct WhisperModelInfo {
    /// e.g. `"base.en"`, `"large-v3-turbo"`.
    pub size: String,
    pub installed: bool,
    pub active: bool,
    /// Ships with the app (Resources); cannot be deleted.
    pub bundled: bool,
    /// Multilingual (not an English-only `.en` build).
    pub multilingual: bool,
    /// On-disk size of the downloaded file, when installed in the profile.
    pub size_bytes: Option<u64>,
}

/// `"…/ggml-base.en.bin"` → `"base.en"`.
fn size_from_path(p: &str) -> Option<String> {
    let stem = std::path::Path::new(p).file_stem()?.to_str()?; // "ggml-base.en"
    stem.strip_prefix("ggml-").map(|s| s.to_string())
}

/// The size currently selected for transcription — from
/// `settings.whisper_model_path`, else the bundled default.
fn active_size(app: &AppState) -> String {
    crate::settings::Settings::load_or_default(&app.profile.settings_path())
        .whisper_model_path
        .as_deref()
        .and_then(size_from_path)
        .unwrap_or_else(|| BUNDLED_SIZE.to_string())
}

pub fn list_whisper_models_impl(app: &AppState) -> Result<Vec<WhisperModelInfo>> {
    let active = active_size(app);
    let models_dir = app.profile.models_dir();
    let out = providers_local::KNOWN_MODELS
        .iter()
        .map(|&size| {
            let bundled = size == BUNDLED_SIZE;
            let pf = models_dir.join(format!("ggml-{size}.bin"));
            let meta = std::fs::metadata(&pf)
                .ok()
                .filter(|m| m.is_file() && m.len() > 0);
            WhisperModelInfo {
                size: size.to_string(),
                installed: bundled || meta.is_some(),
                active: size == active,
                bundled,
                multilingual: !size.ends_with(".en"),
                size_bytes: meta.map(|m| m.len()),
            }
        })
        .collect();
    Ok(out)
}

pub fn set_active_whisper_model_impl(app: &AppState, size: &str) -> Result<()> {
    if !providers_local::KNOWN_MODELS.contains(&size) {
        return Err(AppError::Config(format!("unknown whisper model {size:?}")));
    }
    let path = app.profile.settings_path();
    let mut s = crate::settings::Settings::load_or_default(&path);
    if size == BUNDLED_SIZE {
        // Clearing the override selects the bundled Resources copy.
        s.whisper_model_path = None;
    } else {
        let p = app.profile.models_dir().join(format!("ggml-{size}.bin"));
        if !p.is_file() {
            return Err(AppError::Config(format!("model {size} is not installed")));
        }
        s.whisper_model_path = Some(p.to_string_lossy().into_owned());
    }
    s.save(&path).map_err(|e| AppError::Io(e.to_string()))?;
    Ok(())
}

pub fn delete_whisper_model_impl(app: &AppState, size: &str) -> Result<()> {
    if size == BUNDLED_SIZE {
        return Err(AppError::Config(
            "the bundled model can't be deleted".into(),
        ));
    }
    // If it is the active model, switch to the bundled default first.
    if active_size(app) == size {
        set_active_whisper_model_impl(app, BUNDLED_SIZE)?;
    }
    let p = app.profile.models_dir().join(format!("ggml-{size}.bin"));
    if p.is_file() {
        syncsafe::remove_file(&p).map_err(|e| AppError::Io(format!("delete model: {e}")))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn size_parsing() {
        assert_eq!(size_from_path("/x/models/ggml-base.en.bin").as_deref(), Some("base.en"));
        assert_eq!(size_from_path("ggml-large-v3-turbo.bin").as_deref(), Some("large-v3-turbo"));
        assert_eq!(size_from_path("not-a-model.bin"), None);
    }
}
