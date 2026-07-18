//! Tauri commands for settings + audio source enumeration.

use crate::error::{AppError, Result};
use crate::settings::Settings;
use crate::state::AppState;
use audio_engine::source::{list_sources, SourceKind};
use serde::Serialize;

pub fn read_settings_impl(app: &AppState) -> Result<Settings> {
    Ok(Settings::load_or_default(&app.profile.settings_path()))
}

pub fn write_settings_impl(app: &AppState, settings: Settings) -> Result<()> {
    settings.save(&app.profile.settings_path())?;
    Ok(())
}

#[derive(Debug, Serialize)]
pub struct AudioSourceInfo {
    pub id: u32,
    pub kind: &'static str,
    pub node_name: String,
    pub description: String,
}

pub fn list_audio_sources_impl() -> Result<Vec<AudioSourceInfo>> {
    let sources = list_sources()
        .map_err(|e| AppError::Recording(format!("list sources: {e}")))?;
    Ok(sources
        .into_iter()
        .map(|s| AudioSourceInfo {
            id: s.id,
            kind: match s.kind {
                SourceKind::Mic => "mic",
                SourceKind::Monitor => "monitor",
            },
            node_name: s.node_name,
            description: s.description,
        })
        .collect())
}
