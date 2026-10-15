// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Licensing against the public license test stack. Network.
//!
//! ```sh
//! cargo test -p tauri-app --features license-test --test license_live
//! ```
//!
//! The tests take turns, and every activation is released on the way out,
//! pass or fail.
#![cfg(feature = "license-test")]

use tauri_app_core::commands::license::{
    activate_license_impl, deactivate_license_impl, InstallRecord, LicenseStatus,
    LICENSE_API_BASE,
};

/// The signer behind the test web deployment, called directly.
const SIGNER: &str = "https://6lrnqmlyu0.execute-api.ca-central-1.amazonaws.com";
/// Stamp lifetime the signer issues (`STAMP_TTL_SECS`).
const STAMP_TTL_SECS: i64 = 35 * 86_400;

const ACTIVE: &str = "0iCNidYixKTBojfYK48S";
const PERPETUAL: &str = "5skfEkTnjSmEtG6XxQBH";
const EXPIRED: &str = "5E4EPuyFPdglrf93gUjk";
const REVOKED: &str = "BuYeyDThghWCUtkdqO3C";
const LIFETIME: &str = "TtjIWvRuuSkHhHahKXrY";
const SANDBOX: &str = "sSzJlOGiLUvGxQh330Wl";
const GOOD: [&str; 4] = [ACTIVE, PERPETUAL, LIFETIME, SANDBOX];

fn alone() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// A machine that has never run Daisy, with its config dir redirected.
fn fresh_machine() -> tempfile::TempDir {
    let td = tempfile::tempdir().unwrap();
    std::env::set_var("DAISY_CONFIG_DIR", td.path());
    assert_eq!(
        license::config_dir_path().unwrap().canonicalize().unwrap(),
        td.path().canonicalize().unwrap(),
        "DAISY_CONFIG_DIR is being ignored — tauri-app needs license/ui-test in its dev-dependencies."
    );
    td
}

/// A record holding `key` and nothing else, never written to disk.
fn record_for(key: &str) -> InstallRecord {
    let now = license::now_unix();
    InstallRecord {
        install_id: "license-live-test".into(),
        key_hex: "00".repeat(32),
        trial_started_unix: now,
        last_seen_unix: now,
        license_key: Some(key.into()),
        license_stamp: None,
        activation_receipt: None,
        last_checkin_unix: 0,
        gateway_pubkey: None,
    }
}

/// POST `{ key }` to the signer's `/checkin`, optionally pinning its clock.
fn signer_checkin(key: &str, now: Option<i64>) -> Option<(u16, serde_json::Value)> {
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .unwrap();
    let mut req = client.post(format!("{SIGNER}/checkin")).json(&serde_json::json!({ "key": key }));
    if let Some(t) = now {
        req = req.header("x-daisy-test-now", t.to_string());
    }
    let resp = req.send().ok()?;
    let status = resp.status().as_u16();
    Some((status, resp.json().unwrap_or(serde_json::Value::Null)))
}

/// Releases the seat of whatever is activated in the redirected config dir
/// with one deactivate call, whose answer is returned, then clears the license
/// locally. Runs on drop too, so a failing test still gives its seat back. The
/// flag records that the seat was already released.
struct Seat(bool);
impl Seat {
    fn release(&mut self) -> Option<u16> {
        if std::mem::replace(&mut self.0, true) {
            return None;
        }
        let mut rec = InstallRecord::load_or_create().ok()?;
        let key = rec.license_key.clone()?;
        let status = reqwest::blocking::Client::new()
            .post(format!("{LICENSE_API_BASE}/api/deactivate"))
            .json(&serde_json::json!({
                "key": key,
                "install_id": rec.install_id,
                "receipt": rec.activation_receipt,
            }))
            .send()
            .map(|r| r.status().as_u16())
            .ok();
        rec.license_key = None;
        rec.license_stamp = None;
        rec.activation_receipt = None;
        if let Err(e) = rec.save() {
            eprintln!("local license clear failed: {e}");
        }
        if !matches!(status, Some(200..=299)) {
            eprintln!("deactivate answered {status:?}; a fixture seat of {key} may be held");
        }
        status
    }
}
impl Drop for Seat {
    fn drop(&mut self) {
        self.release();
    }
}

/// Waits for the start of the next minute.
fn fresh_window() {
    let into = license::now_unix().rem_euclid(60) as u64;
    std::thread::sleep(std::time::Duration::from_secs(61 - into));
}

#[test]
fn builds_pin_the_test_server() {
    assert_eq!(LICENSE_API_BASE, "https://smbr-daisy-web-test.vercel.app");
}

#[test]
fn test_stamps_verify_for_every_good_fixture() {
    for key in GOOD {
        let mut rec = record_for(key);
        let resp = signer_checkin(key, None);
        assert!(
            matches!(resp, Some((200, _))),
            "{key}: check-in answered {resp:?}"
        );
        assert!(
            license::apply_checkin_response(&mut rec, resp),
            "{key}: stamp did not verify against the test key"
        );
        assert!(
            matches!(license::status_from(&rec), LicenseStatus::Licensed { key: k, .. } if k == key),
            "{key}: not licensed after a verified stamp"
        );
    }
}

#[test]
fn expired_and_revoked_fixtures_are_refused() {
    let resp = signer_checkin(EXPIRED, None);
    assert!(
        matches!(&resp, Some((403, b)) if b["error"] == "inactive"),
        "expired: {resp:?}"
    );
    let mut rec = record_for(EXPIRED);
    assert!(!license::apply_checkin_response(&mut rec, resp));
    assert!(rec.license_stamp.is_none());

    let resp = signer_checkin(REVOKED, None);
    assert!(
        matches!(&resp, Some((403, b)) if b["error"] == "revoked"),
        "revoked: {resp:?}"
    );
    let mut rec = record_for(REVOKED);
    license::apply_checkin_response(&mut rec, resp);
    assert!(rec.license_key.is_none(), "403 revoked must drop the key");
}

#[test]
fn a_stamp_lapses_at_its_expiry() {
    // The signer's clock pinned an hour back: the stamp's expiry follows it.
    let pinned = license::now_unix() - 3_600;
    let mut rec = record_for(ACTIVE);
    assert!(license::apply_checkin_response(
        &mut rec,
        signer_checkin(ACTIVE, Some(pinned))
    ));
    let LicenseStatus::Licensed { expires: Some(expires), .. } = license::status_at(&rec, pinned)
    else {
        panic!("stamp did not license at its issue time");
    };
    assert_eq!(expires, pinned + STAMP_TTL_SECS, "x-daisy-test-now did not pin the stamp");
    assert!(matches!(license::status_at(&rec, expires - 1), LicenseStatus::Licensed { .. }));
    assert!(!matches!(license::status_at(&rec, expires), LicenseStatus::Licensed { .. }));
}

#[test]
fn activate_then_deactivate_through_the_test_server() {
    let _one = alone();
    fresh_window();
    for key in GOOD {
        let _home = fresh_machine();
        let mut seat = Seat(false);
        match activate_license_impl(key.into(), None) {
            Ok(LicenseStatus::Licensed { key: k, .. }) => assert_eq!(k, key),
            other => panic!("{key}: activate gave {other:?}"),
        }
        let rec = InstallRecord::load_or_create().unwrap();
        assert!(rec.activation_receipt.is_some(), "{key}: no activation receipt");
        let status = seat.release();
        assert!(matches!(status, Some(200..=299)), "{key}: deactivate answered {status:?}");
        let rec = InstallRecord::load_or_create().unwrap();
        assert!(rec.license_key.is_none() && rec.license_stamp.is_none());
    }
}

#[test]
fn activate_refuses_expired_and_revoked_through_the_test_server() {
    let _one = alone();
    fresh_window();
    for (key, says) in [(EXPIRED, "expired"), (REVOKED, "revoked")] {
        let _home = fresh_machine();
        let _seat = Seat(false);
        let err = activate_license_impl(key.into(), None)
            .expect_err(&format!("{key}: activation succeeded"))
            .to_string();
        assert!(err.contains(says), "{key}: {err}");
    }
}

#[test]
fn deactivate_without_a_stored_receipt_completes() {
    let _one = alone();
    fresh_window();
    let _home = fresh_machine();
    let mut seat = Seat(false);
    assert!(matches!(
        activate_license_impl(ACTIVE.into(), None),
        Ok(LicenseStatus::Licensed { .. })
    ));
    let mut rec = InstallRecord::load_or_create().unwrap();
    let receipt = rec.activation_receipt.take().expect("activation receipt");
    rec.save().unwrap();

    deactivate_license_impl().unwrap();
    let rec = InstallRecord::load_or_create().unwrap();
    assert!(rec.license_key.is_none() && rec.activation_receipt.is_none());

    // Backup release with the receipt activate handed out.
    seat.0 = true;
    let _ = reqwest::blocking::Client::new()
        .post(format!("{LICENSE_API_BASE}/api/deactivate"))
        .json(&serde_json::json!({ "key": ACTIVE, "install_id": rec.install_id, "receipt": receipt }))
        .send();
}
