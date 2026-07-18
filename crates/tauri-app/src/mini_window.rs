//! Floating mini-window + system tray lifecycle.
//!
//! The mini-window is a second webview (label `MINI_LABEL`) that loads the
//! same frontend bundle; `main.tsx` renders `<MiniWindow/>` when it detects
//! that label. The main window is hidden, not closed, while the mini is up.

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
}
