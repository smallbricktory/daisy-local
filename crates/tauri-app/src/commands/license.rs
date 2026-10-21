// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Licensing — activation, deactivation and check-in against the license
//! server.
//!
//! Licensing verification lives in the `license` crate; this module is the HTTP + Tauri side and re-exports what the rest of the app uses.

use crate::error::{AppError, Result};
pub use ::license::{
    apply_checkin_response, canonicalize_key, features_enabled, gateway_entitled,
    license_status_impl, perpetual_upgrade_offered, status_from, InstallRecord, LicenseStatus, ENT_DAISY_CLOUD,
    LICENSE_API_BASE, TRIAL_DAYS,
};

const ACTIVATE_PATH: &str = "/api/activate";
const DEACTIVATE_PATH: &str = "/api/deactivate";
/// Check-in (heartbeat) endpoint — returns a fresh signed validity stamp.
const REFRESH_PATH: &str = "/api/license/refresh";
/// Stripe Checkout for Perpetual, replacing the subscription key in the body.
const UPGRADE_CHECKOUT_PATH: &str = "/api/license/upgrade-checkout";
/// Minimum spacing between check-in attempts.
const CHECKIN_THROTTLE_SECS: i64 = 86_400;

/// Held across each license command's load, server call and save.
static RECORD: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn hold_record() -> std::sync::MutexGuard<'static, ()> {
    RECORD.lock().unwrap_or_else(|e| e.into_inner())
}

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
    let _held = hold_record();
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
    rec.revoked_reason = None;
    // The key/seat is persisted before the first check-in.
    rec.save()?;
    // Check in for the first stamp; `last_checkin_unix` is set only when a
    // stamp is stored.
    let resp = post_json(REFRESH_PATH, serde_json::json!({ "key": canonical }));
    if apply_checkin_response(&mut rec, resp) {
        if rec.license_stamp.is_some() {
            rec.last_checkin_unix = now_unix();
        }
        rec.save()?;
    }
    Ok(status_from(&rec))
}

/// Register (or refresh) this install's public key by re-activating the same
/// install_id with `install_pubkey`. Requires an active license key.
pub fn register_gateway_install_impl(pubkey_b64: &str) -> Result<()> {
    let _held = hold_record();
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
    let _held = hold_record();
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

/// The Stripe Checkout URL in an upgrade-checkout response. Only an https
/// `checkout.stripe.com` page is opened.
pub fn upgrade_checkout_url(resp: Option<(u16, serde_json::Value)>) -> Result<String> {
    match resp {
        Some((s, body)) if (200..300).contains(&s) => {
            let url = body.get("url").and_then(|u| u.as_str()).unwrap_or_default();
            // The parsed form is what opens, so the browser reads the host checked here.
            match reqwest::Url::parse(url) {
                Ok(u) if u.scheme() == "https"
                    && u.host_str() == Some("checkout.stripe.com")
                    && u.username().is_empty()
                    && u.password().is_none()
                    && u.port().is_none() => Ok(u.to_string()),
                _ => {
                    log::warn!("license upgrade: HTTP {s} without a Stripe Checkout url");
                    Err(AppError::Config("The license server didn't return a checkout page. Try again later.".into()))
                }
            }
        }
        Some((409, _)) => Err(AppError::Config("This license is already Perpetual or isn't a subscription.".into())),
        Some((403, _)) => Err(AppError::Config("This key has been revoked.".into())),
        Some((400 | 404, _)) => Err(AppError::Config("That key isn't recognized.".into())),
        Some((429, _)) => Err(AppError::Config("Too many attempts — try again in a moment.".into())),
        Some((502 | 503, _)) => Err(AppError::Config("Buy Perpetual isn't available right now. Try again later.".into())),
        Some((s, _)) => Err(AppError::Config(format!("License server error (HTTP {s})."))),
        None => Err(AppError::Config(
            "Couldn't reach the license server. Check your connection and try again.".into(),
        )),
    }
}

/// A Stripe Checkout URL for Perpetual that replaces this device's
/// subscription key. The key travels only in the POST body.
pub fn upgrade_checkout_impl() -> Result<String> {
    let _held = hold_record();
    let rec = InstallRecord::load_or_create()?;
    if !perpetual_upgrade_offered(&status_from(&rec)) {
        return Err(AppError::Config("Perpetual replaces an active subscription license.".into()));
    }
    let Some(key) = rec.license_key.clone() else {
        return Err(AppError::Config("Perpetual replaces an active subscription license.".into()));
    };
    upgrade_checkout_url(post_json(UPGRADE_CHECKOUT_PATH, serde_json::json!({ "key": key })))
}

/// Whether a check-in is due at `now`: a licensed record once per
/// `CHECKIN_THROTTLE_SECS`, a lapsed one (key kept, no stamp) every time, so
/// a renewal shows at once. No key, no check-in.
fn checkin_due(rec: &InstallRecord, now: i64) -> bool {
    rec.license_key.is_some()
        && (rec.license_stamp.is_none() || now - rec.last_checkin_unix >= CHECKIN_THROTTLE_SECS)
}

/// Heartbeat check-in when [`checkin_due`]: at launch, every 6 h, and when
/// the license screen opens. Runs regardless of the stamp's expiry window.
pub fn checkin_if_needed_impl() -> Result<LicenseStatus> {
    let _held = hold_record();
    let mut rec = InstallRecord::load_or_create()?;
    let now = now_unix();
    if !checkin_due(&rec, now) {
        return Ok(status_from(&rec));
    }
    let Some(key) = rec.license_key.clone() else {
        return Ok(status_from(&rec));
    };
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
    fn upgrade_checkout_opens_only_a_stripe_checkout_page() {
        let ok = |url: &str| upgrade_checkout_url(Some((200, serde_json::json!({ "ok": true, "url": url }))));
        assert_eq!(ok("https://checkout.stripe.com/c/pay/cs_live_abc").unwrap(), "https://checkout.stripe.com/c/pay/cs_live_abc");
        assert!(ok("http://checkout.stripe.com/c/pay/cs_live_abc").is_err(), "plain http");
        assert!(ok("https://checkout.stripe.com.evil.example/c").is_err(), "lookalike host");
        assert!(ok("https://example.com/").is_err());
        assert!(ok("https://checkout.stripe.com@evil.example/").is_err(), "userinfo");
        assert!(ok("https://user@checkout.stripe.com/c").is_err(), "userinfo on the right host");
        assert!(ok("https://checkout.stripe.com:8443/c").is_err(), "port");
        // A backslash reads as a slash; what opens is the normalized URL.
        assert_eq!(ok("https://checkout.stripe.com\\@evil.example/x").unwrap(), "https://checkout.stripe.com/@evil.example/x");
        assert!(upgrade_checkout_url(Some((200, serde_json::json!({})))).is_err(), "no url");
    }

    #[test]
    fn upgrade_checkout_failures_name_what_went_wrong() {
        let msg = |r: Option<(u16, serde_json::Value)>| upgrade_checkout_url(r).unwrap_err().to_string();
        assert!(msg(None).contains("Couldn't reach the license server"));
        assert!(msg(Some((429, serde_json::json!({})))).contains("Too many attempts"));
        assert!(msg(Some((409, serde_json::json!({ "ok": false, "error": "not_subscription" })))).contains("already Perpetual"));
        assert!(msg(Some((403, serde_json::json!({ "ok": false, "error": "revoked" })))).contains("revoked"));
        assert!(msg(Some((404, serde_json::json!({ "ok": false, "error": "unknown_key" })))).contains("isn't recognized"));
        assert!(msg(Some((400, serde_json::json!({ "ok": false, "error": "invalid_key" })))).contains("isn't recognized"));
        assert!(msg(Some((503, serde_json::json!({ "error": "upgrade_unavailable" })))).contains("isn't available right now"));
        assert!(msg(Some((503, serde_json::json!({ "ok": false, "error": "store_unavailable" })))).contains("isn't available right now"));
        assert!(msg(Some((502, serde_json::json!({ "error": "bad_upstream" })))).contains("isn't available right now"));
        assert!(msg(Some((500, serde_json::json!({})))).contains("HTTP 500"));
    }

    fn rec_at(stamp: Option<&str>, last_checkin_unix: i64) -> InstallRecord {
        InstallRecord {
            install_id: "id".into(), key_hex: "00".into(),
            trial_started_unix: 0, last_seen_unix: 0,
            license_key: Some("KEY".into()),
            license_stamp: stamp.map(Into::into),
            activation_receipt: None, last_checkin_unix,
            gateway_pubkey: None, revoked_reason: None,
        }
    }

    #[test]
    fn a_licensed_record_checks_in_once_a_day() {
        let now = 1_000_000;
        assert!(!checkin_due(&rec_at(Some("STAMP"), now - 60), now), "a minute after the last");
        assert!(!checkin_due(&rec_at(Some("STAMP"), now - CHECKIN_THROTTLE_SECS + 1), now));
        assert!(checkin_due(&rec_at(Some("STAMP"), now - CHECKIN_THROTTLE_SECS), now));
    }

    #[test]
    fn a_lapsed_record_checks_in_at_every_launch() {
        let now = 1_000_000;
        assert!(checkin_due(&rec_at(None, now), now), "right after the lapse");
        assert!(checkin_due(&rec_at(None, now - 60), now));
    }

    #[test]
    fn a_record_without_a_key_never_checks_in() {
        let mut rec = rec_at(None, 0);
        rec.license_key = None;
        assert!(!checkin_due(&rec, 1_000_000));
    }

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
