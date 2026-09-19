// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Tauri commands for settings + audio source enumeration.

use crate::error::{AppError, Result};
use crate::settings::Settings;
use crate::state::AppState;
use audio_engine::source::{list_sources, SourceKind};
use serde::Serialize;

pub fn read_settings_impl(app: &AppState) -> Result<Settings> {
    Ok(Settings::load_or_default(&app.profile.settings_path()))
}

pub fn write_settings_impl(app: &AppState, mut settings: Settings) -> Result<()> {
    // Properties this build does not know exist only on disk. A UI payload
    // is built from the properties the UI knows about, so saving it as-is
    // would delete whatever a newer build had written.
    let path = app.profile.settings_path();
    for (k, v) in Settings::load_or_default(&path).unknown {
        settings.unknown.entry(k).or_insert(v);
    }
    settings.save(&path)?;
    Ok(())
}

/// Applies the Settings-UI-owned fields of an incoming whole-struct machine
/// settings snapshot onto the stored block, under the process-wide lock.
/// Backend-owned fields (live captions choice + bench results, diarizer,
/// whisper model path, hop ladder) keep their stored values, so a stale UI
/// snapshot can't wipe them. Returns the merged block as saved.
pub fn apply_ui_machine_settings(
    app: &AppState,
    incoming: &crate::settings::MachineSettings,
) -> std::io::Result<crate::settings::MachineSettings> {
    // Mic contract with the UI: a picked mic arrives as (id: Some, key: None)
    // — the picker nulls the key so a fresh numeric id is authoritative. An
    // untouched selection round-trips whatever view it was given, key intact,
    // so an unplugged (unresolvable) mic survives unrelated settings writes.
    // (None, None) is an explicit clear.
    enum MicWrite {
        Set(String),
        Keep,
        Clear,
    }
    let mic_write = match (incoming.default_mic_source_id, incoming.default_mic_key.as_deref()) {
        (None, Some(_)) => MicWrite::Keep,
        (None, None) => MicWrite::Clear,
        (Some(id), _) => {
            let cached = MIC_VIEW_CACHE.lock().unwrap().clone();
            match cached {
                // Unchanged mic (the id we last handed out): no enumeration.
                Some((k, cid)) if cid == id => MicWrite::Set(k),
                _ => match mic_id_to_key(Some(id), &list_sources().unwrap_or_default()) {
                    Some(k) => {
                        *MIC_VIEW_CACHE.lock().unwrap() = Some((k.clone(), id));
                        MicWrite::Set(k)
                    }
                    // The id didn't resolve (stale picker list): never
                    // destroy the stored preference on a resolution failure.
                    None => MicWrite::Keep,
                },
            }
        }
    };
    app.update_machine_settings(|m| {
        match &mic_write {
            MicWrite::Set(k) => m.default_mic_key = Some(k.clone()),
            MicWrite::Clear => m.default_mic_key = None,
            MicWrite::Keep => {}
        }
        m.default_mic_source_id = None;
        m.aec_mode_override = incoming.aec_mode_override;
        m.show_live_captions = incoming.show_live_captions;
        // Tunables are UI-owned; the active model id is written only by
        // set_active_asr_model and survives a stale UI snapshot.
        m.asr = crate::settings::AsrSettings {
            active: m.asr.active.clone(),
            ..incoming.asr.clone()
        };
        m.denoise_enabled = incoming.denoise_enabled;
        m.debug_level = incoming.debug_level;
        m.ui_zoom = incoming.ui_zoom;
        m.css_theme = incoming.css_theme.clone();
        m.appearance = incoming.appearance.clone();
        m.mcp_enabled = incoming.mcp_enabled;
        m.mcp_port = incoming.mcp_port;
        m.mcp_allow_write = incoming.mcp_allow_write;
        m.meeting_prompt = incoming.meeting_prompt.clone();
        m.record_hotkey = incoming.record_hotkey.clone();
        m.nerd_mode = incoming.nerd_mode;
    })
}

/// UI-picked runtime id → stable device key (persisted truth).
pub fn mic_id_to_key(id: Option<u32>, sources: &[audio_engine::source::Source]) -> Option<String> {
    sources
        .iter()
        .find(|s| Some(s.id) == id)
        .map(|s| s.key.0.clone())
}

/// Single-entry key↔id cache behind the resolved mic view. Device
/// enumeration is an audio-daemon round-trip, and the settings commands run
/// on hot UI paths (the zoom shortcut persists per keystroke) — so the
/// key↔id pair is resolved once and reused until the device world changes
/// (every `audio-sources-changed` push clears it).
static MIC_VIEW_CACHE: std::sync::Mutex<Option<(String, u32)>> = std::sync::Mutex::new(None);

pub fn invalidate_mic_view_cache() {
    *MIC_VIEW_CACHE.lock().unwrap() = None;
}

/// The (key, id) pair the view last handed out, if any. Lets the record
/// start path tell "this stale id meant the stored default" apart from an
/// explicit pick of some other device.
pub fn cached_mic_view() -> Option<(String, u32)> {
    MIC_VIEW_CACHE.lock().unwrap().clone()
}

/// Fills the transient numeric view for the frontend: the stored key,
/// resolved against the current device list. A pre-migration numeric value
/// passes through untouched.
pub fn resolve_mic_view(m: &mut crate::settings::MachineSettings) {
    if m.default_mic_source_id.is_some() {
        return;
    }
    let Some(key) = m.default_mic_key.as_deref() else { return };
    if let Some((k, id)) = MIC_VIEW_CACHE.lock().unwrap().as_ref() {
        if k == key {
            m.default_mic_source_id = Some(*id);
            return;
        }
    }
    let resolved = list_sources()
        .ok()
        .and_then(|ss| ss.into_iter().find(|s| s.key.0 == key))
        .map(|s| s.id);
    if let Some(id) = resolved {
        *MIC_VIEW_CACHE.lock().unwrap() = Some((key.to_string(), id));
    }
    m.default_mic_source_id = resolved;
}

/// This machine's settings with the mic view resolved — the one-call form
/// every frontend-facing getter must use.
pub fn machine_settings_view(app: &AppState) -> crate::settings::MachineSettings {
    let mut m = app.machine_settings();
    resolve_mic_view(&mut m);
    m
}

#[derive(Debug, Serialize)]
pub struct AudioSourceInfo {
    pub id: u32,
    pub kind: &'static str,
    pub node_name: String,
    pub description: String,
}

/// A generated reference header + the built-in design tokens, written to
/// `<profile>/css/default.css` when the css dir is first materialized.
/// Themes are OVERRIDE layers: the built-in stylesheet always loads and
/// the selected file loads after it, so a theme only changes what it
/// names and survives app updates.
fn default_theme_contents() -> String {
    // The bundled stylesheet, compiled into the binary; the `:root` token
    // block is extracted as the editable reference.
    const WORKSPACE_CSS: &str =
        include_str!("../../../../apps/frontend/src/styles/workspace.css");
    let tokens = WORKSPACE_CSS
        .find(":root")
        .and_then(|start| WORKSPACE_CSS[start..].find('}').map(|end| &WORKSPACE_CSS[start..start + end + 1]))
        .unwrap_or(":root {\n}");
    format!(
        "/* Daisy theme override.\n\
         \n\
         Files in this folder load AFTER the built-in stylesheet — override\n\
         only what you name. The block below is this build's design tokens\n\
         for reference; uncomment and change values, or add any CSS rules.\n\
         Copy this file to create new themes; every .css file here appears\n\
         in Settings → Display. */\n\n/*\n{tokens}\n*/\n"
    )
}

/// Theme file stems in `<profile>/css`, creating the dir + a generated
/// `default.css` reference on first use.
pub fn list_css_themes_impl(app: &AppState) -> Result<Vec<String>> {
    let dir = app.profile.css_dir();
    syncsafe::create_dir_all(&dir).map_err(|e| AppError::Io(format!("css dir: {e}")))?;
    let default = dir.join("default.css");
    if !default.is_file() {
        let _ = syncsafe::write(&default, default_theme_contents().as_bytes());
    }
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .map_err(|e| AppError::Io(format!("css dir: {e}")))?
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let p = e.path();
            (p.extension().and_then(|x| x.to_str()) == Some("css"))
                .then(|| p.file_stem()?.to_str().map(str::to_owned))
                .flatten()
        })
        .collect();
    names.sort();
    Ok(names)
}

/// A theme file's contents by stem. The stem is validated (no separators),
/// bounded, and resolved inside the css dir only.
pub fn read_css_theme_impl(app: &AppState, name: &str) -> Result<String> {
    const MAX_THEME_BYTES: u64 = 512 * 1024;
    if !valid_theme_name(name) {
        return Err(AppError::Config(format!("invalid theme name {name:?}")));
    }
    let path = app.profile.css_dir().join(format!("{name}.css"));
    let meta = std::fs::metadata(&path)
        .map_err(|_| AppError::Config(format!("theme {name:?} not found")))?;
    if meta.len() > MAX_THEME_BYTES {
        return Err(AppError::Config(format!("theme {name:?} exceeds 512 KB")));
    }
    syncsafe::read_to_string(&path).map_err(|e| AppError::Io(format!("read theme: {e}")))
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

/// A theme stem may only name a file directly inside the css dir.
fn valid_theme_name(name: &str) -> bool {
    !name.is_empty()
        && !name.contains("..")
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ' ' | '.'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_names_reject_traversal_and_separators() {
        for bad in ["", "../etc/passwd", "a/b", "a\\b", "x..y", "café"] {
            assert!(!valid_theme_name(bad), "{bad:?} must be rejected");
        }
        for good in ["default", "high-contrast", "my_theme 2", "v1.2"] {
            assert!(valid_theme_name(good), "{good:?} must be accepted");
        }
    }

    #[test]
    fn default_theme_reference_contains_tokens_commented_out() {
        let css = default_theme_contents();
        assert!(css.contains(":root"), "token block extracted");
        assert!(css.contains("load AFTER the built-in stylesheet"));
        // The token block is inside a comment — applying default.css as-is
        // changes nothing.
        assert!(css.find("/*").unwrap() < css.find(":root").unwrap());
    }
}
