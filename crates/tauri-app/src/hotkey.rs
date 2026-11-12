// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! The global start-recording shortcut: validating an accelerator and
//! applying it to the OS.

use std::str::FromStr;
use tauri_plugin_global_shortcut::Shortcut;

/// Accepts an accelerator the OS can hold: one non-modifier key preceded by
/// at least one of Control, Alt, Shift or Super.
pub fn validate(accel: &str) -> Result<(), String> {
    let shortcut = Shortcut::from_str(accel).map_err(|e| e.to_string())?;
    if shortcut.mods.is_empty() {
        return Err(format!(
            "{accel} needs a modifier (Control, Alt, Shift or Super)"
        ));
    }
    Ok(())
}

use crate::error::AppError;

/// The error a combination the OS refuses produces.
fn taken_error(accel: &str) -> AppError {
    AppError::Config(format!(
        "Another app already uses {accel}. Pick a different combination."
    ))
}

/// The error an accelerator this app will not hold produces.
fn invalid_error(accel: &str) -> AppError {
    match validate(accel) {
        Ok(()) => taken_error(accel),
        Err(reason) => AppError::Config(reason),
    }
}

/// Drops whatever is registered and takes `accel` instead. `None` leaves the
/// app with no shortcut.
pub fn apply(app: &tauri::AppHandle, accel: Option<&str>) -> Result<(), AppError> {
    use tauri_plugin_global_shortcut::GlobalShortcutExt;
    let _ = app.global_shortcut().unregister_all();
    let Some(accel) = accel else { return Ok(()) };
    if validate(accel).is_err() {
        return Err(invalid_error(accel));
    }
    let shortcut = Shortcut::from_str(accel).map_err(|_| invalid_error(accel))?;
    app.global_shortcut()
        .register(shortcut)
        .map_err(|e| {
            log::warn!("record hotkey: the OS refused {accel}: {e}");
            taken_error(accel)
        })?;
    log::info!("record hotkey: registered {accel}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_accelerator_needs_a_modifier_and_one_key() {
        assert!(validate("Control+Alt+R").is_ok());
        assert!(validate("Shift+Alt+F9").is_ok());
        assert!(validate("Control+Shift+ArrowUp").is_ok());
        assert!(validate("R").is_err(), "a bare key is not a shortcut");
        assert!(validate("Control+Alt").is_err(), "modifiers alone are not a shortcut");
        assert!(validate("").is_err());
        assert!(validate("Control+Nonsense").is_err());
    }

    #[test]
    fn a_refused_shortcut_names_itself() {
        let e = taken_error("Control+Alt+R");
        assert!(matches!(e, crate::error::AppError::Config(_)));
        assert_eq!(
            e.to_string(),
            "config: Another app already uses Control+Alt+R. Pick a different combination."
        );
    }

    #[test]
    fn a_rejected_accelerator_reports_the_reason() {
        let e = invalid_error("Control+Alt");
        assert!(matches!(e, crate::error::AppError::Config(_)));
        assert!(e.to_string().contains("Control+Alt"));
    }
}
