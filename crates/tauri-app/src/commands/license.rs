// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Licensing — activation, deactivation and check-in against the license
//! server.
//!
//! Licensing verification lives in the `license` crate; this module is the HTTP + Tauri side and re-exports what the rest of the app uses.

use crate::error::{AppError, Result};
pub use ::license::{
    apply_checkin_response, canonicalize_key, features_enabled, gateway_entitled,
    license_status_impl, status_from, InstallRecord, LicenseStatus, ENT_DAISY_CLOUD,
    LICENSE_API_BASE, TRIAL_DAYS,
};

const ACTIVATE_PATH: &str = "/api/activate";
const DEACTIVATE_PATH: &str = "/api/deactivate";
/// Check-in (heartbeat) endpoint — returns a fresh signed validity stamp.
const REFRESH_PATH: &str = "/api/license/refresh";
/// Minimum spacing between check-in attempts.
const CHECKIN_THROTTLE_SECS: i64 = 86_400;

use crate::now_unix;

/// POST a JSON body to `path` on the license server, returning (HTTP status,
/// parsed body) or None on a network/transport error. 8s timeout.
fn post_json(path: &str, body: serde_json::Value) -> Option<(u16, serde_json::Value)> {
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(8))
        .build()
        .ok()?;
    let resp = client.post(format!("{LICENSE_API_BASE}{path}")).json(&body).send().ok()?;
    let status = resp.status().as_u16();
    let json = resp.json::<serde_json::Value>().unwrap_or(serde_json::Value::Null);
    Some((status, json))
}

fn activate_body(key: &str, install_id: &str, install_pubkey: Option<&str>) -> serde_json::Value {
    let mut body = serde_json::json!({ "key": key, "install_id": install_id });
    if let Some(pk) = install_pubkey {
        body["install_pubkey"] = serde_json::Value::String(pk.to_string());
    }
    body
}

fn post_activate(
    key: &str,
    install_id: &str,
    install_pubkey: Option<&str>,
) -> Option<(u16, serde_json::Value)> {
    post_json(ACTIVATE_PATH, activate_body(key, install_id, install_pubkey))
}

/// The deactivation receipt of a successful activate response.
fn receipt_of(resp: &Option<(u16, serde_json::Value)>) -> Option<String> {
    match resp {
        Some((s, body)) if (200..300).contains(s) => {
            body.get("receipt").and_then(|r| r.as_str()).map(String::from)
        }
        _ => None,
    }
}

/// Map an activate response to a user-facing error, or `None` if the seat
/// was claimed.
fn activate_error(resp: &Option<(u16, serde_json::Value)>) -> Option<AppError> {
    match resp {
        Some((s, _)) if (200..300).contains(s) => None,
        Some((400, _)) => Some(AppError::Config("That doesn't look like a valid key.".into())),
        Some((404, _)) => Some(AppError::Config("That key isn't recognized.".into())),
        Some((403, body)) => {
            let err = body.get("error").and_then(|e| e.as_str()).unwrap_or("");
            Some(AppError::Config(match err {
                "revoked" => "This key has been revoked.".into(),
                "expired" => "This subscription has expired — renew it to reactivate.".into(),
                "seat_limit" => {
                    "All 3 license seats are in use. Deactivate this license on another device first.".into()
                }
                _ => "The license server refused this key.".into(),
            }))
        }
        Some((429, _)) => Some(AppError::Config("Too many attempts — try again in a moment.".into())),
        Some((s, _)) => Some(AppError::Config(format!("License server error (HTTP {s})."))),
        None => Some(AppError::Config(
            "Couldn't reach the license server. Check your connection and try again.".into(),
        )),
    }
}

/// `install_pubkey` is this install's base64 Ed25519 public key, registered
/// with the seat on activate. `None` when the vault is locked.
pub fn activate_license_impl(key: String, install_pubkey: Option<String>) -> Result<LicenseStatus> {
    let canonical = canonicalize_key(&key);
    if canonical.is_empty() {
        return Err(AppError::Config("Enter your license key.".into()));
    }
    let mut rec = InstallRecord::load_or_create()?;
    // Claim the seat and register the install's public key. A successful
    // server response is required to license this device.
    let resp = post_activate(&canonical, &rec.install_id, install_pubkey.as_deref());
    if let Some(e) = activate_error(&resp) {
        return Err(e);
    }
    let receipt = receipt_of(&resp);
    rec.license_key = Some(canonical.clone());
    rec.activation_receipt = receipt;
    // The key/seat is persisted before the first check-in.
    rec.save()?;
    // Check in for the first stamp; `last_checkin_unix` is set only when a
    // stamp is stored.
    let resp = post_json(REFRESH_PATH, serde_json::json!({ "key": canonical }));
    if apply_checkin_response(&mut rec, resp) {
        rec.last_checkin_unix = now_unix();
        rec.save()?;
    }
    Ok(status_from(&rec))
}

/// Register (or refresh) this install's public key by re-activating the same
/// install_id with `install_pubkey`. Requires an active license key.
pub fn register_gateway_install_impl(pubkey_b64: &str) -> Result<()> {
    let mut rec = InstallRecord::load_or_create()?;
    if rec.gateway_pubkey.as_deref() == Some(pubkey_b64) {
        log::debug!("daisy cloud: this install key is already registered");
        return Ok(());
    }
    let key = rec.license_key.clone().ok_or_else(|| {
        AppError::Config("Activate your license before enabling Daisy Cloud.".into())
    })?;
    let resp = post_activate(&key, &rec.install_id, Some(pubkey_b64));
    match activate_error(&resp) {
        None => {
            rec.gateway_pubkey = Some(pubkey_b64.to_string());
            rec.save()?;
            log::info!("daisy cloud: install key registered");
            Ok(())
        }
        Some(_) if matches!(&resp, Some((403, _))) => {
            log::warn!("daisy cloud: registration refused (403)");
            Err(AppError::Config(
                "Seat limit reached — deactivate Daisy on another device first.".into(),
            ))
        }
        Some(e) => {
            log::warn!("daisy cloud: registration failed: {e}");
            Err(e)
        }
    }
}

/// Release this device's seat (server-side, authenticated by the receipt) and
/// remove the license locally, reverting to trial/expired. The network call
/// is best-effort.
pub fn deactivate_license_impl() -> Result<LicenseStatus> {
    let mut rec = InstallRecord::load_or_create()?;
    if let Some(key) = rec.license_key.clone() {
        // Without a stored receipt, activating again (idempotent for the seat
        // this install holds) returns it.
        let receipt = rec
            .activation_receipt
            .clone()
            .or_else(|| receipt_of(&post_activate(&key, &rec.install_id, None)));
        let resp = post_json(
            DEACTIVATE_PATH,
            serde_json::json!({
                "key": key,
                "install_id": rec.install_id,
                "receipt": receipt,
            }),
        );
        if !matches!(resp, Some((200..=299, _))) {
            log::warn!("license: deactivate not confirmed ({:?}); the seat may stay held", resp.map(|(s, _)| s));
        }
    }
    rec.license_key = None;
    rec.license_stamp = None;
    rec.activation_receipt = None;
    rec.save()?;
    Ok(status_from(&rec))
}

/// Heartbeat check-in, throttled to once per `CHECKIN_THROTTLE_SECS`. Runs
/// regardless of the stamp's expiry window.
pub fn checkin_if_needed_impl() -> Result<LicenseStatus> {
    let mut rec = InstallRecord::load_or_create()?;
    let Some(key) = rec.license_key.clone() else {
        return Ok(status_from(&rec));
    };
    let now = now_unix();
    if now - rec.last_checkin_unix < CHECKIN_THROTTLE_SECS {
        return Ok(status_from(&rec));
    }
    // The throttle timestamp is updated even when the call fails.
    rec.last_checkin_unix = now;
    rec.save()?;
    let resp = post_json(REFRESH_PATH, serde_json::json!({ "key": key }));
    if apply_checkin_response(&mut rec, resp) {
        rec.save()?;
    }
    Ok(status_from(&rec))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activate_body_includes_pubkey_only_when_present() {
        let bare = activate_body("KEY", "INST", None);
        assert_eq!(bare["key"], "KEY");
        assert!(bare.get("install_pubkey").is_none());
        let reg = activate_body("KEY", "INST", Some("PUB"));
        assert_eq!(reg["install_pubkey"], "PUB");
    }

    #[test]
    fn receipt_only_from_a_successful_activate() {
        let ok = Some((200, serde_json::json!({ "ok": true, "receipt": "r" })));
        assert_eq!(receipt_of(&ok).as_deref(), Some("r"));
        assert_eq!(receipt_of(&Some((200, serde_json::json!({ "ok": true })))), None);
        assert_eq!(receipt_of(&Some((403, serde_json::json!({ "receipt": "r" })))), None);
        assert_eq!(receipt_of(&None), None);
    }

    #[test]
    fn activate_error_matrix() {
        assert!(activate_error(&Some((200, serde_json::json!({ "receipt": "r" })))).is_none());
        assert!(activate_error(&Some((400, serde_json::json!({ "error": "invalid_key" })))).is_some());
        assert!(activate_error(&Some((404, serde_json::json!({ "error": "unknown_key" })))).is_some());
        assert!(activate_error(&Some((403, serde_json::json!({ "error": "revoked" })))).is_some());
        let expired = activate_error(&Some((403, serde_json::json!({ "error": "expired" })))).unwrap();
        assert!(expired.to_string().contains("expired"), "{expired}");
        assert!(activate_error(&Some((403, serde_json::json!({ "error": "seat_limit" })))).is_some());
        assert!(activate_error(&Some((429, serde_json::json!({ "error": "rate_limited" })))).is_some());
        assert!(activate_error(&None).is_some());
    }
}
