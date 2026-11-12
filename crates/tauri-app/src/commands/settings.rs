// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Tauri commands for settings + audio source enumeration.

use crate::error::{AppError, Result};
use crate::settings::Settings;
use crate::state::{AppState, SettingsFile};
use audio_engine::source::{list_sources, SourceKind};
use serde::{Deserialize, Serialize};

/// The Settings-UI-owned fields of the profile's settings.json. An absent
/// field keeps its stored value; the rest of the file (backend-owned fields,
/// properties a newer build wrote) is never part of a patch.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SettingsPatch {
    #[serde(default, deserialize_with = "present")]
    pub default_summary_prompt_id: Option<Option<String>>,
    #[serde(default, deserialize_with = "present")]
    pub user_display_name: Option<Option<String>>,
    pub nav_order: Option<Vec<String>>,
    pub auto_update_check: Option<bool>,
    pub voice_match_tolerance: Option<u8>,
}

/// Merges `patch` into the stored settings under the lock. Returns the
/// settings as saved.
pub fn update_settings_impl(app: &AppState, patch: SettingsPatch) -> Result<Settings> {
    app.update_settings(|s| {
        if let Some(v) = patch.default_summary_prompt_id {
            s.default_summary_prompt_id = v;
        }
        if let Some(v) = patch.user_display_name {
            s.user_display_name = v;
        }
        if let Some(v) = patch.nav_order {
            s.nav_order = v;
        }
        if let Some(v) = patch.auto_update_check {
            s.auto_update_check = v;
        }
        if let Some(v) = patch.voice_match_tolerance {
            s.voice_match_tolerance = v;
        }
    })
    .map_err(|e| AppError::Config(format!("save settings: {e}")))
}

/// The Settings-UI-owned fields of this machine's settings block. An absent field keeps its stored value. Backend-owned fields (the AI provider, the active ASR model, the stored mic key) have no field here.
///
/// The mic arrives as the resolved numeric view: `default_mic_source_id: n`
/// picks device `n` and stores its stable key; `null` clears the choice;
/// absent keeps it.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MachinePatch {
    #[serde(default, deserialize_with = "present")]
    pub default_mic_source_id: Option<Option<u32>>,
    pub aec_mode_override: Option<crate::settings::AecModeOverride>,
    pub show_live_captions: Option<bool>,
    pub live_speaker_names: Option<bool>,
    /// Tunables only; `asr.active` keeps its stored value, written by
    /// `set_active_asr_model` alone.
    pub asr: Option<crate::settings::AsrSettings>,
    pub denoise_enabled: Option<bool>,
    pub debug_level: Option<crate::settings::DebugLevel>,
    pub ui_zoom: Option<f32>,
    #[serde(default, deserialize_with = "present")]
    pub css_theme: Option<Option<String>>,
    pub appearance: Option<String>,
    pub show_tray_icon: Option<bool>,
    pub minimize_on_close: Option<bool>,
    pub mcp_enabled: Option<bool>,
    pub mcp_port: Option<u16>,
    pub mcp_allow_write: Option<bool>,
    pub meeting_prompt: Option<crate::settings::MeetingPrompt>,
    #[serde(default, deserialize_with = "present")]
    pub record_hotkey: Option<Option<String>>,
    pub nerd_mode: Option<bool>,
}

/// A field that is present deserializes to `Some`, even when it is `null`.
fn present<'de, T, D>(d: D) -> std::result::Result<Option<T>, D::Error>
where
    T: Deserialize<'de>,
    D: serde::Deserializer<'de>,
{
    T::deserialize(d).map(Some)
}

/// Merges `patch` into this machine's stored block under the process-wide
/// lock. Returns the block as saved.
pub fn apply_machine_patch(
    app: &AppState,
    patch: MachinePatch,
) -> std::io::Result<crate::settings::MachineSettings> {
    enum MicWrite {
        Set(String),
        Keep,
        Clear,
    }
    let mic_write = match patch.default_mic_source_id {
        None => MicWrite::Keep,
        Some(None) => MicWrite::Clear,
        Some(Some(id)) => {
            let cached = MIC_VIEW_CACHE.lock().unwrap().clone();
            match cached {
                // Unchanged mic (the id we last handed out): no enumeration.
                Some((k, cid)) if cid == id => MicWrite::Set(k),
                _ => match mic_id_to_key(Some(id), &list_sources().unwrap_or_default()) {
                    Some(k) => {
                        *MIC_VIEW_CACHE.lock().unwrap() = Some((k.clone(), id));
                        MicWrite::Set(k)
                    }
                    // The id didn't resolve (stale picker list): the stored preference is kept.
                    None => MicWrite::Keep,
                },
            }
        }
    };
    app.update_machine_settings(|m| {
        match mic_write {
            MicWrite::Set(k) => m.default_mic_key = Some(k),
            MicWrite::Clear => m.default_mic_key = None,
            MicWrite::Keep => {}
        }
        m.default_mic_source_id = None;
        if let Some(asr) = patch.asr {
            m.asr = crate::settings::AsrSettings { active: m.asr.active.clone(), ..asr };
        }
        if let Some(v) = patch.minimize_on_close {
            m.minimize_on_close = Some(v);
        }
        macro_rules! take {
            ($($field:ident),*) => { $(if let Some(v) = patch.$field { m.$field = v; })* };
        }
        take!(
            aec_mode_override, show_live_captions, live_speaker_names, denoise_enabled, debug_level, ui_zoom,
            css_theme, appearance, show_tray_icon, mcp_enabled, mcp_port, mcp_allow_write, meeting_prompt,
            record_hotkey, nerd_mode
        );
    })
}

/// This machine's settings block + the machine name it is stored under: what
/// `machine_settings` answers and `machine-settings:changed` carries.
#[derive(Debug, Clone, Serialize)]
pub struct MachineSettingsStatus {
    pub machine: String,
    pub settings: crate::settings::MachineSettings,
}

pub fn machine_settings_status(app: &AppState) -> MachineSettingsStatus {
    MachineSettingsStatus {
        machine: crate::hardware::machine_name(),
        settings: machine_settings_view(app),
    }
}

/// The events every window follows its settings by.
pub const SETTINGS_CHANGED: &str = "settings:changed";
pub const MACHINE_SETTINGS_CHANGED: &str = "machine-settings:changed";

/// Serializes reads with their emits.
static EMITTING: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Tells every window what `which` holds now. Reads it on its own thread: the
/// machine block's mic view may enumerate devices.
pub fn emit_changed<R: tauri::Runtime>(handle: &tauri::AppHandle<R>, which: SettingsFile) {
    let handle = handle.clone();
    std::thread::spawn(move || {
        use tauri::{Emitter, Manager};
        let _order = EMITTING.lock().unwrap_or_else(|e| e.into_inner());
        let Some(app) = handle.try_state::<AppState>() else { return };
        let sent = match which {
            SettingsFile::Profile => handle.emit(SETTINGS_CHANGED, app.settings()),
            SettingsFile::Machine => {
                handle.emit(MACHINE_SETTINGS_CHANGED, machine_settings_status(&app))
            }
        };
        if let Err(e) = sent {
            log::warn!("settings: change not emitted: {e}");
        }
    });
}

/// The device list changed: the mic view resolves afresh, and every window
/// hears about both the list and the machine block it renumbers.
pub fn audio_sources_changed<R: tauri::Runtime>(handle: &tauri::AppHandle<R>) {
    use tauri::{Emitter, Manager};
    invalidate_mic_view_cache();
    let _ = handle.emit("audio-sources-changed", ());
    if let Some(app) = handle.try_state::<AppState>() {
        app.settings_changed(SettingsFile::Machine);
    }
}

/// UI-picked runtime id → stable device key (persisted truth).
pub fn mic_id_to_key(id: Option<u32>, sources: &[audio_engine::source::Source]) -> Option<String> {
    sources
        .iter()
        .find(|s| Some(s.id) == id)
        .map(|s| s.key.0.clone())
}

/// Single-entry key↔id cache behind the resolved mic view, cleared by every `audio-sources-changed` push.
static MIC_VIEW_CACHE: std::sync::Mutex<Option<(String, u32)>> = std::sync::Mutex::new(None);

pub fn invalidate_mic_view_cache() {
    *MIC_VIEW_CACHE.lock().unwrap() = None;
}

/// The (key, id) pair the view last handed out, if any.
pub fn cached_mic_view() -> Option<(String, u32)> {
    MIC_VIEW_CACHE.lock().unwrap().clone()
}

/// Fills the transient numeric view for the frontend: the stored key, resolved against the current device list.
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

/// A generated reference header + the built-in design tokens, written to `<profile>/css/default.css` when the css dir is first materialized. Themes are override layers: the built-in stylesheet loads first and the selected file loads after it.
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
