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
/// parsed body) or None on a network/transport error. 8s timeout. Over https
/// only; a redirect is answered as its own status.
fn post_json(path: &str, body: serde_json::Value) -> Option<(u16, serde_json::Value)> {
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(8))
        .redirect(reqwest::redirect::Policy::none())
        .https_only(true)
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
    let replaced = take_seat(&mut rec, &canonical, &resp);
    // The key/seat is persisted before the first check-in.
    rec.save()?;
    // The seat a replaced key held is given back once, best effort; the new
    // license stands whatever the server answers.
    if let Some(mut old) = replaced {
        match release_seat(&mut old, post_json) {
            Ok(()) => log::info!("license: the replaced key's seat was released"),
            Err(_) => log::warn!("license: the replaced key's seat was not released"),
        }
    }
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

/// Licenses `rec` with `canonical` from a successful activate response.
/// Returns the record as it stood when it held another key, for releasing
/// that key's seat.
fn take_seat(rec: &mut InstallRecord, canonical: &str, resp: &Option<(u16, serde_json::Value)>) -> Option<InstallRecord> {
    let replaced = rec.license_key.as_deref().is_some_and(|old| old != canonical).then(|| rec.clone());
    rec.license_key = Some(canonical.to_string());
    rec.activation_receipt = receipt_of(resp);
    rec.revoked_reason = None;
    replaced
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
/// remove the license locally, reverting to trial/expired. The license stays
/// on this device until the server confirms the seat is free.
pub fn deactivate_license_impl() -> Result<LicenseStatus> {
    let _held = hold_record();
    let mut rec = InstallRecord::load_or_create()?;
    let released = release_seat(&mut rec, post_json);
    rec.save()?;
    released.map(|()| status_from(&rec))
}

/// Gives the seat `rec` holds back through `post`, then clears the key, the
/// stamp and the receipt. The record is cleared when the server releases the
/// seat or answers that no seat worth releasing stands under the key; any
/// other answer leaves the license in place and returns what to tell the user.
fn release_seat(
    rec: &mut InstallRecord,
    post: impl Fn(&str, serde_json::Value) -> Option<(u16, serde_json::Value)>,
) -> Result<()> {
    if let Some(key) = rec.license_key.clone() {
        let mut receipt = match rec.activation_receipt.clone() {
            Some(r) => Some(r),
            None => fetch_receipt(&key, &rec.install_id, &post)?,
        };
        let mut refetched = false;
        while let Some(r) = receipt.take() {
            rec.activation_receipt = Some(r.clone());
            let resp = post(
                DEACTIVATE_PATH,
                serde_json::json!({ "key": key, "install_id": rec.install_id, "receipt": r }),
            );
            match &resp {
                // The server releases idempotently: a seat already free answers
                // `{"ok":true}` too. A 2xx without it is a page in the way.
                Some((s, body)) if (200..300).contains(s) && body.get("ok") == Some(&serde_json::Value::Bool(true)) => {}
                // The server holds no such key, so no seat stands under it.
                _ if error_is(&resp, 400, &["invalid_key"]) => {}
                // A receipt the server no longer matches is fetched again, once.
                Some((401, _)) if !refetched => {
                    refetched = true;
                    receipt = fetch_receipt(&key, &rec.install_id, &post)?;
                }
                _ => return Err(deactivate_error(&resp)),
            }
        }
    }
    rec.license_key = None;
    rec.license_stamp = None;
    rec.activation_receipt = None;
    Ok(())
}

/// The receipt for `key`'s seat on this install, by activating again, which
/// the server answers for a seat this install holds without claiming
/// another. `None` when the server answers that no seat worth releasing
/// stands under the key: none held on a full license, or the key is
/// unknown, revoked, inactive or expired.
fn fetch_receipt(
    key: &str,
    install_id: &str,
    post: &impl Fn(&str, serde_json::Value) -> Option<(u16, serde_json::Value)>,
) -> Result<Option<String>> {
    let resp = post(ACTIVATE_PATH, activate_body(key, install_id, None));
    match &resp {
        Some((s, _)) if (200..300).contains(s) => receipt_of(&resp).map(Some).ok_or_else(|| deactivate_error(&resp)),
        Some((400, _)) => Ok(None),
        _ if error_is(&resp, 404, &["unknown_key"]) => Ok(None),
        _ if error_is(&resp, 403, &["seat_limit", "revoked", "inactive", "expired"]) => Ok(None),
        _ => Err(deactivate_error(&resp)),
    }
}

/// True when `resp` has `status` and one of `errors` as its `error`.
fn error_is(resp: &Option<(u16, serde_json::Value)>, status: u16, errors: &[&str]) -> bool {
    matches!(resp, Some((s, body)) if *s == status
        && body.get("error").and_then(|e| e.as_str()).is_some_and(|e| errors.contains(&e)))
}

/// What a deactivate the server did not confirm tells the user, whatever
/// the server answered; the answer goes to the log.
fn deactivate_error(resp: &Option<(u16, serde_json::Value)>) -> AppError {
    log::warn!("license: deactivate not confirmed ({:?}); the license stays", resp.as_ref().map(|(s, _)| *s));
    AppError::Config(DEACTIVATE_UNCONFIRMED.into())
}

const DEACTIVATE_UNCONFIRMED: &str = "The license server did not confirm that this computer's seat was released, so the license stays on this computer. Check your connection and click Deactivate this device again.";

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

    type Reply = Option<(u16, serde_json::Value)>;

    fn licensed() -> InstallRecord {
        InstallRecord { activation_receipt: Some("RECEIPT".into()), ..rec_at(Some("STAMP"), 0) }
    }

    /// Runs `release_seat` against a server that answers `answer` per path,
    /// returning the outcome and the calls made.
    fn deactivate_against(
        rec: &mut InstallRecord,
        answer: impl Fn(&str) -> Reply,
    ) -> (Result<()>, Vec<(String, serde_json::Value)>) {
        let calls = std::cell::RefCell::new(Vec::new());
        let out = release_seat(rec, |path, body| {
            calls.borrow_mut().push((path.to_string(), body));
            answer(path)
        });
        (out, calls.into_inner())
    }

    fn cleared(rec: &InstallRecord) -> bool {
        rec.license_key.is_none() && rec.license_stamp.is_none() && rec.activation_receipt.is_none()
    }

    fn kept(rec: &InstallRecord) -> bool {
        rec.license_key.as_deref() == Some("KEY") && rec.license_stamp.as_deref() == Some("STAMP")
    }

    #[test]
    fn a_released_seat_clears_the_license() {
        let mut rec = licensed();
        let (out, calls) = deactivate_against(&mut rec, |_| Some((200, serde_json::json!({ "ok": true }))));
        assert!(out.is_ok());
        assert!(cleared(&rec));
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, DEACTIVATE_PATH);
        assert_eq!(calls[0].1, serde_json::json!({ "key": "KEY", "install_id": "id", "receipt": "RECEIPT" }));
    }

    #[test]
    fn a_seat_the_server_no_longer_holds_clears_the_license() {
        let mut rec = licensed();
        let (out, _) = deactivate_against(&mut rec, |_| Some((400, serde_json::json!({ "error": "invalid_key" }))));
        assert!(out.is_ok(), "the server holds no such key");
        assert!(cleared(&rec));

        // An install with no stored receipt asks for one; a full license
        // answers that this install holds none of its seats.
        let mut rec = InstallRecord { activation_receipt: None, ..licensed() };
        let (out, calls) = deactivate_against(&mut rec, |_| Some((403, serde_json::json!({ "error": "seat_limit" }))));
        assert!(out.is_ok());
        assert!(cleared(&rec));
        assert_eq!(calls.iter().map(|(p, _)| p.as_str()).collect::<Vec<_>>(), [ACTIVATE_PATH]);

        let mut rec = InstallRecord { activation_receipt: None, ..licensed() };
        let (out, calls) = deactivate_against(&mut rec, |_| Some((400, serde_json::json!({ "error": "invalid_key" }))));
        assert!(out.is_ok(), "the server holds no such key");
        assert!(cleared(&rec));
        assert_eq!(calls.len(), 1);
    }

    #[test]
    fn a_dead_key_at_the_receipt_step_clears_the_license() {
        let dead = [
            (403, serde_json::json!({ "error": "revoked" })),
            (403, serde_json::json!({ "error": "inactive" })),
            (403, serde_json::json!({ "error": "expired" })),
            (404, serde_json::json!({ "error": "unknown_key" })),
            (400, serde_json::json!({})),
        ];
        for (status, body) in dead {
            let mut rec = InstallRecord { activation_receipt: None, ..licensed() };
            let (out, calls) = deactivate_against(&mut rec, |_| Some((status, body.clone())));
            assert!(out.is_ok(), "{status} {body}");
            assert!(cleared(&rec), "{status} {body}");
            assert_eq!(calls.len(), 1, "no release is sent without a receipt");
        }
    }

    #[test]
    fn a_receipt_step_that_cannot_answer_keeps_the_license() {
        let cases: [Reply; 5] = [
            None,
            Some((429, serde_json::json!({ "error": "rate_limited" }))),
            Some((503, serde_json::json!({ "error": "unavailable" }))),
            Some((404, serde_json::Value::Null)),
            Some((200, serde_json::json!({ "ok": true }))),
        ];
        for reply in cases {
            let mut rec = InstallRecord { activation_receipt: None, ..licensed() };
            let (out, calls) = deactivate_against(&mut rec, |_| reply.clone());
            assert!(out.unwrap_err().to_string().contains(DEACTIVATE_UNCONFIRMED), "{reply:?}");
            assert!(kept(&rec), "{reply:?}");
            assert!(rec.activation_receipt.is_none());
            assert_eq!(calls.len(), 1);
        }
    }

    #[test]
    fn a_receipt_the_server_no_longer_matches_is_fetched_again_once() {
        let deactivates = std::cell::Cell::new(0);
        let mut rec = licensed();
        let (out, calls) = deactivate_against(&mut rec, |path| match path {
            ACTIVATE_PATH => Some((200, serde_json::json!({ "ok": true, "receipt": "FRESH" }))),
            _ => {
                deactivates.set(deactivates.get() + 1);
                match deactivates.get() {
                    1 => Some((401, serde_json::json!({ "error": "bad_receipt" }))),
                    _ => Some((200, serde_json::json!({ "ok": true }))),
                }
            }
        });
        assert!(out.is_ok());
        assert!(cleared(&rec));
        let paths: Vec<&str> = calls.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(paths, [DEACTIVATE_PATH, ACTIVATE_PATH, DEACTIVATE_PATH]);
        assert_eq!(calls[0].1["receipt"], "RECEIPT");
        assert_eq!(calls[2].1["receipt"], "FRESH");

        // Refused again: the license stays, with the fresh receipt, and no
        // third release is tried.
        let mut rec = licensed();
        let (out, calls) = deactivate_against(&mut rec, |path| match path {
            ACTIVATE_PATH => Some((200, serde_json::json!({ "ok": true, "receipt": "FRESH" }))),
            _ => Some((401, serde_json::json!({ "error": "bad_receipt" }))),
        });
        assert!(out.unwrap_err().to_string().contains(DEACTIVATE_UNCONFIRMED));
        assert!(kept(&rec));
        assert_eq!(rec.activation_receipt.as_deref(), Some("FRESH"));
        assert_eq!(calls.len(), 3);

        // The fetch answers that the key is dead: nothing is left to release.
        let mut rec = licensed();
        let (out, _) = deactivate_against(&mut rec, |path| match path {
            ACTIVATE_PATH => Some((403, serde_json::json!({ "error": "revoked" }))),
            _ => Some((401, serde_json::json!({ "error": "bad_receipt" }))),
        });
        assert!(out.is_ok());
        assert!(cleared(&rec));
    }

    #[test]
    fn a_2xx_that_is_not_the_servers_answer_keeps_the_license() {
        for body in [serde_json::Value::Null, serde_json::json!({}), serde_json::json!({ "ok": false })] {
            let mut rec = licensed();
            let (out, _) = deactivate_against(&mut rec, |_| Some((200, body.clone())));
            assert!(out.is_err(), "{body}: a captive portal's page releases nothing");
            assert!(kept(&rec));
        }
    }

    #[test]
    fn a_server_error_keeps_the_license_for_a_retry() {
        let cases: [Reply; 7] = [
            Some((500, serde_json::json!({}))),
            Some((503, serde_json::json!({ "error": "unavailable" }))),
            Some((429, serde_json::json!({ "error": "rate_limited" }))),
            Some((401, serde_json::json!({ "error": "bad_receipt" }))),
            Some((400, serde_json::json!({}))),
            Some((404, serde_json::json!({}))),
            Some((302, serde_json::Value::Null)),
        ];
        for reply in cases {
            let mut rec = licensed();
            let (out, _) = deactivate_against(&mut rec, |_| reply.clone());
            assert!(out.unwrap_err().to_string().contains(DEACTIVATE_UNCONFIRMED), "{reply:?}");
            assert!(kept(&rec), "{reply:?} left the license in place");
            assert_eq!(rec.activation_receipt.as_deref(), Some("RECEIPT"));
        }
    }

    #[test]
    fn a_network_error_keeps_the_license_for_a_retry() {
        let mut rec = licensed();
        let (out, _) = deactivate_against(&mut rec, |_| None);
        assert!(out.unwrap_err().to_string().contains(DEACTIVATE_UNCONFIRMED));
        assert!(kept(&rec));
        assert_eq!(rec.activation_receipt.as_deref(), Some("RECEIPT"));

        let mut rec = InstallRecord { activation_receipt: None, ..licensed() };
        let (out, calls) = deactivate_against(&mut rec, |_| None);
        assert!(out.unwrap_err().to_string().contains(DEACTIVATE_UNCONFIRMED));
        assert!(kept(&rec));
        assert_eq!(calls.len(), 1, "no release is sent without a receipt");
    }

    #[test]
    fn a_receipt_fetched_for_the_release_is_kept_when_the_release_fails() {
        let mut rec = InstallRecord { activation_receipt: None, ..licensed() };
        let (out, calls) = deactivate_against(&mut rec, |path| match path {
            ACTIVATE_PATH => Some((200, serde_json::json!({ "ok": true, "receipt": "FRESH" }))),
            _ => Some((503, serde_json::json!({ "error": "unavailable" }))),
        });
        assert!(out.is_err());
        assert!(kept(&rec));
        assert_eq!(rec.activation_receipt.as_deref(), Some("FRESH"), "a retry sends it without claiming again");
        assert_eq!(calls[1].1["receipt"], "FRESH");

        // An activate that answers without a receipt leaves nothing to release with.
        let mut rec = InstallRecord { activation_receipt: None, ..licensed() };
        let (out, calls) = deactivate_against(&mut rec, |_| Some((200, serde_json::json!({ "ok": true }))));
        assert!(out.is_err());
        assert!(kept(&rec));
        assert_eq!(calls.len(), 1);
    }

    #[test]
    fn a_replaced_keys_seat_is_released() {
        let mut rec = licensed();
        let old = take_seat(&mut rec, "NEW", &Some((200, serde_json::json!({ "ok": true, "receipt": "NEW-RECEIPT" }))));
        let mut old = old.expect("the replaced key comes back");
        let (out, calls) = deactivate_against(&mut old, |_| Some((200, serde_json::json!({ "ok": true }))));
        assert!(out.is_ok());
        assert_eq!(calls[0].1, serde_json::json!({ "key": "KEY", "install_id": "id", "receipt": "RECEIPT" }));
        assert_eq!(rec.license_key.as_deref(), Some("NEW"));
        assert_eq!(rec.activation_receipt.as_deref(), Some("NEW-RECEIPT"));
    }

    #[test]
    fn a_replaced_seat_the_server_keeps_leaves_the_new_key_active() {
        let mut rec = licensed();
        let mut old = take_seat(&mut rec, "NEW", &Some((200, serde_json::json!({ "ok": true, "receipt": "NEW-RECEIPT" })))).unwrap();
        let (out, _) = deactivate_against(&mut old, |_| Some((503, serde_json::json!({ "error": "unavailable" }))));
        assert!(out.is_err());
        assert_eq!(rec.license_key.as_deref(), Some("NEW"));
        assert_eq!(rec.activation_receipt.as_deref(), Some("NEW-RECEIPT"));

        let mut old = InstallRecord { activation_receipt: None, ..licensed() };
        let (out, _) = deactivate_against(&mut old, |_| None);
        assert!(out.is_err(), "unreachable");
        assert_eq!(rec.license_key.as_deref(), Some("NEW"));
    }

    #[test]
    fn a_replaced_dead_key_needs_no_release() {
        let mut rec = InstallRecord { activation_receipt: None, ..licensed() };
        let mut old = take_seat(&mut rec, "NEW", &Some((200, serde_json::json!({ "ok": true, "receipt": "NEW-RECEIPT" })))).unwrap();
        let (out, calls) = deactivate_against(&mut old, |_| Some((403, serde_json::json!({ "error": "revoked" }))));
        assert!(out.is_ok());
        assert_eq!(calls.iter().map(|(p, _)| p.as_str()).collect::<Vec<_>>(), [ACTIVATE_PATH]);
        assert_eq!(calls[0].1["key"], "KEY");
        assert_eq!(rec.license_key.as_deref(), Some("NEW"));
    }

    #[test]
    fn the_same_key_again_replaces_nothing() {
        let mut rec = licensed();
        assert!(take_seat(&mut rec, "KEY", &Some((200, serde_json::json!({ "ok": true, "receipt": "RECEIPT" })))).is_none());
        let mut rec = InstallRecord { license_key: None, ..licensed() };
        assert!(take_seat(&mut rec, "NEW", &Some((200, serde_json::json!({ "ok": true, "receipt": "R" })))).is_none());
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
