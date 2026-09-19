// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Floating mini-window + system tray lifecycle.
//!
//! The mini-window is a second webview (label `MINI_LABEL`) that loads the
//! same frontend bundle; `main.tsx` renders `<MiniWindow/>` when it detects
//! that label. The main window is hidden, not closed, while the mini is up.

use serde::Serialize;

pub const MINI_LABEL: &str = "mini";
pub const MAIN_LABEL: &str = "main";
/// Meeting-start reminder popup (a third pre-rendered webview).
pub const REMINDER_LABEL: &str = "reminder";

/// Frameless always-on-top dimensions for the Card layout.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MiniWindowConfig {
    pub width: f64,
    pub height: f64,
    pub decorations: bool,
    pub always_on_top: bool,
    pub resizable: bool,
    pub skip_taskbar: bool,
}

impl MiniWindowConfig {
    pub fn card() -> Self {
        Self {
            width: 320.0,
            height: 108.0,
            decorations: false,
            always_on_top: true,
            resizable: false,
            skip_taskbar: true,
        }
    }

    /// Frameless reminder-popup dimensions.
    pub fn reminder() -> Self {
        Self {
            width: 300.0,
            height: 116.0,
            decorations: false,
            always_on_top: true,
            resizable: false,
            skip_taskbar: true,
        }
    }

    /// Frameless meeting-prompt dimensions.
    pub fn reminder_meeting() -> Self {
        Self {
            width: 300.0,
            height: 152.0,
            decorations: false,
            always_on_top: true,
            resizable: false,
            skip_taskbar: true,
        }
    }
}

/// What the reminder popup is showing.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ReminderPayload {
    Calendar { title: String },
    Meeting,
}

/// A button the reminder popup offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReminderChoice {
    Open,
    Dismiss,
    Start,
    Snooze30m,
    Snooze1h,
    SnoozeToday,
    Never,
}

impl ReminderChoice {
    pub fn parse(s: &str) -> Option<ReminderChoice> {
        Some(match s {
            "open" => ReminderChoice::Open,
            "dismiss" => ReminderChoice::Dismiss,
            "start" => ReminderChoice::Start,
            "snooze_30m" => ReminderChoice::Snooze30m,
            "snooze_1h" => ReminderChoice::Snooze1h,
            "snooze_today" => ReminderChoice::SnoozeToday,
            "never" => ReminderChoice::Never,
            _ => return None,
        })
    }
}

/// The unix-seconds timestamp `choice` snoozes the meeting prompt until.
/// `None` for a choice that does not snooze.
pub fn snooze_until(choice: ReminderChoice, now_unix: i64) -> Option<i64> {
    match choice {
        ReminderChoice::Snooze30m => Some(now_unix + 1_800),
        ReminderChoice::Snooze1h => Some(now_unix + 3_600),
        ReminderChoice::SnoozeToday => Some(crate::settings::local_midnight_after(now_unix)),
        ReminderChoice::Open
        | ReminderChoice::Dismiss
        | ReminderChoice::Start
        | ReminderChoice::Never => None,
    }
}

/// Tray tooltip/title text for a given recording state string
/// (matches `RecordingSnapshot::state`: "idle"|"recording"|"paused"|"stopped").
pub fn tray_status_text(state: &str) -> String {
    match state {
        "recording" => "Daisy \u{2014} recording".to_string(),
        "paused" => "Daisy \u{2014} paused".to_string(),
        // "idle", "stopped", and any future/unknown state all read as idle.
        _ => "Daisy \u{2014} idle".to_string(),
    }
}

/// Brings the main window forward for a recording that started from outside
/// the app (hotkey, meeting prompt). When the OS keeps the main window out of
/// focus, the always-on-top mini card appears as well.
pub async fn surface_recording(app: &tauri::AppHandle) {
    use tauri::Manager;
    let _ = show_main(app);
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    let focused = app
        .get_webview_window(MAIN_LABEL)
        .is_some_and(|w| w.is_focused().unwrap_or(false));
    if focused {
        return;
    }
    if let Some(mini) = app.get_webview_window(MINI_LABEL) {
        let _ = mini.show();
    }
}

/// Shows + unminimizes + focuses the main window and hides the mini-window.
/// On macOS, hiding the whole app (Cmd+H) leaves every window's own
/// show/focus a no-op until the app itself is unhidden, so that runs first.
pub fn show_main(app: &tauri::AppHandle) -> Result<(), String> {
    use tauri::Manager;
    #[cfg(target_os = "macos")]
    let _ = app.show();
    if let Some(mini) = app.get_webview_window(MINI_LABEL) {
        mini.hide().map_err(|e| format!("hide mini window: {e}"))?;
    }
    if let Some(main) = app.get_webview_window(MAIN_LABEL) {
        if main.is_minimized().unwrap_or(false) {
            main.unminimize().map_err(|e| format!("unminimize main window: {e}"))?;
        }
        main.show().map_err(|e| format!("show main window: {e}"))?;
        main.set_focus().map_err(|e| format!("focus main window: {e}"))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn card_config_is_frameless_always_on_top() {
        let c = MiniWindowConfig::card();
        assert_eq!(c.width, 320.0);
        assert_eq!(c.height, 108.0);
        assert!(!c.decorations);
        assert!(c.always_on_top);
        assert!(!c.resizable);
        assert!(c.skip_taskbar);
    }

    #[test]
    fn tray_text_reflects_state() {
        assert_eq!(tray_status_text("recording"), "Daisy \u{2014} recording");
        assert_eq!(tray_status_text("paused"), "Daisy \u{2014} paused");
        assert_eq!(tray_status_text("idle"), "Daisy \u{2014} idle");
        assert_eq!(tray_status_text("stopped"), "Daisy \u{2014} idle");
    }

    #[test]
    fn reminder_meeting_config_is_the_taller_popup() {
        let c = MiniWindowConfig::reminder_meeting();
        assert_eq!(c.width, 300.0);
        assert_eq!(c.height, 152.0);
        assert!(c.height > MiniWindowConfig::reminder().height);
        assert!(!c.decorations);
        assert!(c.always_on_top);
    }

    #[test]
    fn the_payload_names_its_kind() {
        let calendar = ReminderPayload::Calendar { title: "Standup".into() };
        assert_eq!(
            serde_json::to_string(&calendar).unwrap(),
            r#"{"kind":"calendar","title":"Standup"}"#
        );
        assert_eq!(
            serde_json::to_string(&ReminderPayload::Meeting).unwrap(),
            r#"{"kind":"meeting"}"#
        );
    }

    #[test]
    fn every_reminder_choice_parses() {
        for (s, want) in [
            ("open", ReminderChoice::Open),
            ("dismiss", ReminderChoice::Dismiss),
            ("start", ReminderChoice::Start),
            ("snooze_30m", ReminderChoice::Snooze30m),
            ("snooze_1h", ReminderChoice::Snooze1h),
            ("snooze_today", ReminderChoice::SnoozeToday),
            ("never", ReminderChoice::Never),
        ] {
            assert_eq!(ReminderChoice::parse(s), Some(want), "{s}");
        }
        assert_eq!(ReminderChoice::parse("snooze"), None);
        assert_eq!(ReminderChoice::parse(""), None);
    }

    #[test]
    fn snooze_until_covers_every_choice() {
        let now = 1_757_000_000_i64;
        assert_eq!(snooze_until(ReminderChoice::Snooze30m, now), Some(now + 1_800));
        assert_eq!(snooze_until(ReminderChoice::Snooze1h, now), Some(now + 3_600));

        let today = snooze_until(ReminderChoice::SnoozeToday, now).expect("today snoozes");
        assert!(today >= now && today <= now + 90_000);

        for choice in [
            ReminderChoice::Never,
            ReminderChoice::Start,
            ReminderChoice::Dismiss,
            ReminderChoice::Open,
        ] {
            assert_eq!(snooze_until(choice, now), None, "{choice:?}");
        }
    }
}
