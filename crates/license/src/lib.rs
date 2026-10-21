// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Trial + license verification — opaque-key model.
//!
//! A license is an opaque key (`XXXX-XXXX-XXXX-XXXX-XXXX`); it carries no
//! data. On check-in the server returns a short-lived Ed25519-signed
//! validity stamp `{ key, name, email, subscription_type, expires }`. The
//! client caches the stamp and, offline, treats the license as valid while
//! `now < stamp.expires`, the signature verifies, and the stamp's `key`
//! matches the stored key. The stamp TTL is enforced entirely by the client.
//!
//! A check-in answered `403 revoked` drops the key and stamp. One answered
//! `403 inactive` or `403 expired` drops the stamp, so the license lapses
//! at once; the key stays, and a check-in after a renewal stamps it again.
//! Any other failure (offline, 5xx, 429) keeps the cached stamp, which
//! lapses at its expiry.
//!
//! Verification only: no HTTP, no Tauri. The activation/check-in calls live
//! in `tauri-app`, which feeds their responses to
//! [`apply_checkin_response`]. Both the app and `daisy-cli` gate on
//! [`features_enabled`].

use base64::{engine::general_purpose::STANDARD as B64, Engine};
use ed25519_dalek::{Signature, VerifyingKey};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Trial length in days.
pub const TRIAL_DAYS: i64 = 30;

/// Entitlement string for an optional provider.
pub const ENT_DAISY_CLOUD: &str = "daisy_cloud";

/// Vendor public key (base64 of 32 raw bytes) that stamp signatures are
/// verified against, and the license server base URL. The `license-test`
/// feature pins the public test stack's pair at compile time; nothing at run
/// time can swap either value.
#[cfg(not(feature = "license-test"))]
const LICENSE_PUBKEY_B64: &str = "B0VAcPcVxjlEZHeeK1FsccGMo927fu9L5SPm0LQ1d9Q=";
#[cfg(not(feature = "license-test"))]
pub const LICENSE_API_BASE: &str = "https://daisy.smbr.app";
#[cfg(feature = "license-test")]
const LICENSE_PUBKEY_B64: &str = "QvXRKCyutJyyIbA9V7YcQNRNlZgZKpBDsT47wjBV8sk=";
#[cfg(feature = "license-test")]
pub const LICENSE_API_BASE: &str = "https://smbr-daisy-web-test.vercel.app";

#[derive(Debug)]
pub enum LicenseError {
    Config(String),
    Io(String),
}

impl std::fmt::Display for LicenseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LicenseError::Config(m) => write!(f, "config: {m}"),
            LicenseError::Io(m) => write!(f, "io: {m}"),
        }
    }
}

impl std::error::Error for LicenseError {}

pub type Result<T> = std::result::Result<T, LicenseError>;

pub fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// The per-machine config directory path holding `eula.json`,
/// `consent.json`, `install.json` and `bootstrap.json` (the key vault lives
/// in the profile directory). Under `cfg(test)` or the `ui-test` feature, a
/// non-empty `DAISY_CONFIG_DIR` overrides the platform default. A relative
/// `DAISY_CONFIG_DIR` returns `InvalidInput`. Computes the path only.
pub fn config_dir_path() -> std::io::Result<PathBuf> {
    #[cfg(any(test, feature = "ui-test"))]
    if let Ok(s) = std::env::var("DAISY_CONFIG_DIR") {
        if !s.is_empty() {
            let p = PathBuf::from(s);
            if !p.is_absolute() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!("DAISY_CONFIG_DIR must be an absolute path, got {}", p.display()),
                ));
            }
            return Ok(p);
        }
    }
    Ok(directories::ProjectDirs::from("ai", "daisy", "Daisy")
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::Other,
                "could not determine config directory",
            )
        })?
        .config_dir()
        .to_path_buf())
}

/// [`config_dir_path`], created on disk. Write-side callers use this.
pub fn config_dir() -> std::io::Result<PathBuf> {
    let dir = config_dir_path()?;
    syncsafe::create_dir_all(&dir)?;
    Ok(dir)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstallRecord {
    pub install_id: String,
    /// Per-install random key (hex, 32 bytes) — anchors profile binding.
    pub key_hex: String,
    pub trial_started_unix: i64,
    pub last_seen_unix: i64,
    /// The opaque license key in canonical form (de-dashed, whitespace-stripped,
    /// case preserved). None = not licensed.
    #[serde(default)]
    pub license_key: Option<String>,
    /// Latest signed validity stamp (`payload_b64.sig_b64`). Re-fetched on
    /// check-in; gone/expired → falls back to trial/expired.
    #[serde(default)]
    pub license_stamp: Option<String>,
    /// Opaque activation receipt; sent with deactivate to release this
    /// device's seat.
    #[serde(default)]
    pub activation_receipt: Option<String>,
    /// Unix seconds of the last check-in attempt.
    #[serde(default)]
    pub last_checkin_unix: i64,
    /// The install public key the license server has on file for this seat.
    #[serde(default)]
    pub gateway_pubkey: Option<String>,
    /// Why the server revoked the last key (`403 revoked` with a `reason`),
    /// e.g. `replaced_by_lifetime`. Cleared when a key activates.
    #[serde(default)]
    pub revoked_reason: Option<String>,
}

impl InstallRecord {
    fn path() -> Result<PathBuf> {
        let dir = config_dir().map_err(|e| LicenseError::Config(e.to_string()))?;
        Ok(dir.join("install.json"))
    }

    /// Load the existing record or create a fresh one. Always bumps
    /// `last_seen` and persists. Unknown fields in install.json are ignored.
    pub fn load_or_create() -> Result<Self> {
        let p = Self::path()?;
        let mut rec = match syncsafe::read(&p).ok().and_then(|b| serde_json::from_slice::<Self>(&b).ok()) {
            Some(r) => r,
            None => {
                let key: [u8; 32] = rand::random();
                let now = now_unix();
                Self {
                    install_id: uuid::Uuid::new_v4().to_string(),
                    key_hex: hex_encode(&key),
                    trial_started_unix: now,
                    last_seen_unix: now,
                    license_key: None,
                    license_stamp: None,
                    activation_receipt: None,
                    last_checkin_unix: 0,
                    gateway_pubkey: None,
                    revoked_reason: None,
                }
            }
        };
        rec.last_seen_unix = now_unix();
        rec.save()?;
        Ok(rec)
    }

    pub fn save(&self) -> Result<()> {
        let p = Self::path()?;
        syncsafe::write_atomic(&p, serde_json::to_vec_pretty(self).map_err(|e| LicenseError::Config(e.to_string()))?)
            .map_err(|e| LicenseError::Io(e.to_string()))?;
        Ok(())
    }
}

fn hex_encode(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Canonical form of an opaque key: strips dashes and all whitespace;
/// preserves case (the alphabet is case-sensitive base62).
pub fn canonicalize_key(input: &str) -> String {
    input.chars().filter(|c| !c.is_whitespace() && *c != '-').collect()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StampPayload {
    /// The canonical key this stamp is bound to.
    #[serde(default)]
    key: String,
    #[serde(default)]
    name: String,
    /// Buyer email.
    #[serde(default)]
    email: String,
    #[serde(default)]
    subscription_type: String,
    /// Unix expiry; null = perpetual.
    #[serde(default)]
    expires: Option<i64>,
    /// Feature entitlements granted to this key (e.g. "feature_a"). Absent
    /// in older stamps → empty.
    #[serde(default)]
    entitlements: Vec<String>,
    /// The license never ends (Perpetual). Absent on subscriptions.
    #[serde(default)]
    perpetual: bool,
    /// Where the key was sold: `stripe`, `app_store`, `google_play`; absent
    /// on manual keys.
    #[serde(default)]
    origin: Option<String>,
}

/// Verify a `payload_b64.sig_b64` stamp signature against the embedded public
/// key. Returns the parsed payload (binding/expiry are checked by the caller).
fn verify_with(stamp: &str, pubkey_b64: &str) -> Option<StampPayload> {
    let cleaned: String = stamp.chars().filter(|c| !c.is_whitespace()).collect();
    let (payload_b64, sig_b64) = cleaned.split_once('.')?;
    let payload_bytes = B64.decode(payload_b64).ok()?;
    let sig_bytes = B64.decode(sig_b64).ok()?;
    let pk_bytes = B64.decode(pubkey_b64).ok()?;
    let pk_arr: &[u8; 32] = pk_bytes.as_slice().try_into().ok()?;
    let pk = VerifyingKey::from_bytes(pk_arr).ok()?;
    let sig = Signature::from_slice(&sig_bytes).ok()?;
    if pk.verify_strict(&payload_bytes, &sig).is_err() {
        log::warn!("license: stamp signature did not verify against embedded pubkey");
        return None;
    }
    serde_json::from_slice::<StampPayload>(&payload_bytes).ok()
}

/// Verify a stamp's signature AND that it is bound to `expected_key`. Expiry
/// is checked separately by the caller. Returns the payload on success.
fn verify_stamp(stamp: &str, expected_key: &str) -> Option<StampPayload> {
    let p = verify_with(stamp, LICENSE_PUBKEY_B64)?;
    if p.key != expected_key {
        log::warn!("license: stamp key does not match stored license_key — rejecting (replay guard)");
        return None;
    }
    Some(p)
}

#[derive(Debug, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum LicenseStatus {
    Licensed {
        name: String,
        email: String,
        key: String,
        expires: Option<i64>,
        subscription_type: String,
        entitlements: Vec<String>,
        /// Perpetual: the license never ends; `expires` is only the stamp's.
        perpetual: bool,
        /// Where the key was sold (`stripe`, `app_store`, `google_play`);
        /// None on manual keys.
        origin: Option<String>,
    },
    Trial {
        days_left: i64,
        /// Why the last key was revoked (`InstallRecord::revoked_reason`).
        #[serde(skip_serializing_if = "Option::is_none")]
        revoked_reason: Option<String>,
    },
    Expired {
        #[serde(skip_serializing_if = "Option::is_none")]
        revoked_reason: Option<String>,
    },
}

pub fn status_from(rec: &InstallRecord) -> LicenseStatus {
    status_at(rec, now_unix())
}

/// [`status_from`] evaluated at unix time `now`.
pub fn status_at(rec: &InstallRecord, now: i64) -> LicenseStatus {
    // A valid, bound, unexpired stamp wins.
    if let (Some(key), Some(stamp)) = (rec.license_key.as_ref(), rec.license_stamp.as_ref()) {
        if let Some(p) = verify_stamp(stamp, key) {
            if p.expires.map(|e| e > now).unwrap_or(true) {
                return LicenseStatus::Licensed {
                    name: p.name,
                    email: p.email,
                    key: key.clone(),
                    expires: p.expires,
                    subscription_type: p.subscription_type,
                    entitlements: p.entitlements,
                    perpetual: p.perpetual,
                    origin: p.origin,
                };
            }
        }
    }
    // Clock rolled back below the last launch → treat the trial as over.
    if now < rec.last_seen_unix {
        return LicenseStatus::Expired { revoked_reason: rec.revoked_reason.clone() };
    }
    let elapsed_days = (now - rec.trial_started_unix) / 86_400;
    let left = TRIAL_DAYS - elapsed_days;
    if left > 0 {
        LicenseStatus::Trial { days_left: left, revoked_reason: rec.revoked_reason.clone() }
    } else {
        LicenseStatus::Expired { revoked_reason: rec.revoked_reason.clone() }
    }
}

pub fn license_status_impl() -> Result<LicenseStatus> {
    Ok(status_from(&InstallRecord::load_or_create()?))
}

/// Whether paid features are allowed for this status read. Only a definitive
/// `Expired` denies; errors reading the status allow.
pub fn allows_paid_features(status: &Result<LicenseStatus>) -> bool {
    !matches!(status, Ok(LicenseStatus::Expired { .. }))
}

/// Whether paid features are allowed: a valid license OR an active trial.
/// Only a definitive `Expired` status denies; errors reading the status
/// allow.
pub fn features_enabled() -> bool {
    allows_paid_features(&license_status_impl())
}

/// Whether `status` is a subscription a Perpetual purchase can replace: an
/// active Stripe or Google Play subscription. The license server cancels and
/// refunds those itself when Perpetual is bought with `replaces_key`.
pub fn perpetual_upgrade_offered(status: &LicenseStatus) -> bool {
    matches!(
        status,
        LicenseStatus::Licensed { perpetual: false, origin: Some(o), .. }
            if o == "stripe" || o == "google_play"
    )
}

/// Whether the current license stamp carries the provider entitlement.
pub fn gateway_entitled() -> bool {
    matches!(
        license_status_impl(),
        Ok(LicenseStatus::Licensed { entitlements, .. })
            if entitlements.iter().any(|e| e == ENT_DAISY_CLOUD)
    )
}

/// Apply a `/api/license/refresh` (check-in) response to an InstallRecord.
/// Returns true if the record changed (caller persists). Pure — no I/O.
///
/// `403 revoked` drops the key and stamp; `403 inactive` and `403 expired`
/// drop the stamp. Every other failure keeps the existing stamp.
pub fn apply_checkin_response(rec: &mut InstallRecord, resp: Option<(u16, serde_json::Value)>) -> bool {
    apply_checkin_response_with(rec, resp, verify_stamp)
}

/// [`apply_checkin_response`] with an injected verification function.
fn apply_checkin_response_with(
    rec: &mut InstallRecord,
    resp: Option<(u16, serde_json::Value)>,
    verify: fn(&str, &str) -> Option<StampPayload>,
) -> bool {
    match resp {
        Some((s, body)) if (200..300).contains(&s) => {
            let key = match rec.license_key.clone() {
                Some(k) => k,
                None => return false,
            };
            match body.get("stamp").and_then(|t| t.as_str()) {
                Some(stamp) => {
                    let stamp = stamp.trim().to_string();
                    if verify(&stamp, &key).is_some() {
                        rec.license_stamp = Some(stamp);
                        return true;
                    }
                    log::warn!("license check-in: returned stamp failed verify/binding; keeping existing");
                }
                None => log::warn!("license check-in: HTTP {s} but no 'stamp' in body; keeping existing"),
            }
        }
        Some((403, body)) => {
            let err = body.get("error").and_then(|e| e.as_str()).unwrap_or("?");
            if err == "revoked" {
                // Hard drop; the trial clock is not touched. The reason (e.g.
                // `replaced_by_lifetime`) tells the license screen what to ask for.
                let reason = body.get("reason").and_then(|r| r.as_str()).map(str::to_string);
                log::warn!("license check-in: 403 revoked ({}) — clearing key + stamp", reason.as_deref().unwrap_or("no reason"));
                rec.license_key = None;
                rec.license_stamp = None;
                rec.revoked_reason = reason;
                return true;
            }
            if err == "inactive" || err == "expired" {
                // The subscription has ended: the license lapses now. The key
                // and seat receipt stay, so a renewal re-stamps on check-in.
                log::warn!("license check-in: 403 {err} — clearing stamp");
                return rec.license_stamp.take().is_some();
            }
            log::warn!("license check-in: 403 {err} — keeping current stamp; lapses at its expiry");
        }
        Some((s, _)) => log::warn!("license check-in: HTTP {s} — keeping current stamp"),
        None => log::info!("license check-in: server unreachable — keeping current stamp"),
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    // Covers the override branch.
    #[test]
    fn config_dir_honors_the_env_override() {
        // Serialized inside one test.
        let prior = std::env::var_os("DAISY_CONFIG_DIR");
        let td = std::env::temp_dir().join(format!("daisy-cfg-{}", std::process::id()));

        std::env::set_var("DAISY_CONFIG_DIR", &td);
        assert_eq!(config_dir().unwrap(), td);
        assert!(td.is_dir(), "the override dir is created");

        std::env::set_var("DAISY_CONFIG_DIR", "relative/config");
        let err = config_dir().expect_err("a relative override is rejected");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
        assert!(err.to_string().contains("DAISY_CONFIG_DIR"), "{err}");
        assert!(
            !std::path::Path::new("relative/config").exists(),
            "a rejected override creates nothing"
        );

        std::fs::remove_dir_all(&td).ok();
        match prior {
            Some(v) => std::env::set_var("DAISY_CONFIG_DIR", v),
            None => std::env::remove_var("DAISY_CONFIG_DIR"),
        }
    }

    fn signed_stamp(seed: &[u8; 32], key: &str, expires: Option<i64>) -> (String, String) {
        let sk = SigningKey::from_bytes(seed);
        let pk_b64 = B64.encode(sk.verifying_key().to_bytes());
        let payload = serde_json::json!({
            "key": key, "name": "Sam", "email": "sam@x.io",
            "subscription_type": "single_user", "expires": expires,
        });
        let pb = serde_json::to_vec(&payload).unwrap();
        let sig = sk.sign(&pb);
        (format!("{}.{}", B64.encode(&pb), B64.encode(sig.to_bytes())), pk_b64)
    }

    #[test]
    fn canonicalize_strips_dashes_and_whitespace_keeps_case() {
        assert_eq!(canonicalize_key(" aB1c-D2eF\n-GhIj-KLmn-OpQr "), "aB1cD2eFGhIjKLmnOpQr");
    }

    #[test]
    fn stamp_entitlements_parse_and_default_empty() {
        let sk = SigningKey::from_bytes(&[3u8; 32]);
        let pk_b64 = B64.encode(sk.verifying_key().to_bytes());
        // Older stamp without the field → empty.
        let (old_stamp, _) = signed_stamp(&[3u8; 32], "K", None);
        assert!(verify_with(&old_stamp, &pk_b64).unwrap().entitlements.is_empty());
        // Stamp carrying the field → parsed.
        let payload = serde_json::json!({
            "key": "K", "name": "", "email": "", "subscription_type": "single_user",
            "expires": null, "entitlements": ["daisy_cloud"],
        });
        let pb = serde_json::to_vec(&payload).unwrap();
        let sig = sk.sign(&pb);
        let stamp = format!("{}.{}", B64.encode(&pb), B64.encode(sig.to_bytes()));
        assert_eq!(verify_with(&stamp, &pk_b64).unwrap().entitlements, vec![ENT_DAISY_CLOUD]);
    }

    #[test]
    fn stamp_verifies_binding_and_tamper() {
        let seed = [7u8; 32];
        let (stamp, pk_b64) = signed_stamp(&seed, "MYKEY", None);
        // Right key binds.
        assert!(verify_with(&stamp, &pk_b64).is_some());
        // Wrong vendor key rejected.
        let other = B64.encode(SigningKey::from_bytes(&[9u8; 32]).verifying_key().to_bytes());
        assert!(verify_with(&stamp, &other).is_none());
    }

    // Stubs matching the real verify signature.
    fn verify_stub_ok(_stamp: &str, key: &str) -> Option<StampPayload> {
        Some(StampPayload { key: key.into(), name: "".into(), email: "".into(),
                            subscription_type: "single_user".into(), expires: Some(i64::MAX),
                            entitlements: vec![], perpetual: false, origin: None })
    }
    fn verify_stub_none(_: &str, _: &str) -> Option<StampPayload> { None }

    fn licensed_rec() -> InstallRecord {
        InstallRecord {
            install_id: "id".into(), key_hex: "deadbeef".into(),
            trial_started_unix: 0, last_seen_unix: 0,
            license_key: Some("MYKEY".into()),
            license_stamp: Some("OLD_STAMP".into()),
            activation_receipt: None, last_checkin_unix: 0,
            gateway_pubkey: None, revoked_reason: None,
        }
    }

    #[test]
    fn checkin_200_valid_stamp_stored() {
        let mut rec = licensed_rec();
        let resp = Some((200u16, serde_json::json!({ "ok": true, "stamp": "NEW_STAMP" })));
        assert!(apply_checkin_response_with(&mut rec, resp, verify_stub_ok));
        assert_eq!(rec.license_stamp.as_deref(), Some("NEW_STAMP"));
    }

    #[test]
    fn checkin_200_bad_stamp_keeps_existing() {
        let mut rec = licensed_rec();
        let resp = Some((200u16, serde_json::json!({ "stamp": "FORGED" })));
        assert!(!apply_checkin_response_with(&mut rec, resp, verify_stub_none));
        assert_eq!(rec.license_stamp.as_deref(), Some("OLD_STAMP"));
    }

    #[test]
    fn checkin_200_no_stamp_field_keeps_existing() {
        let mut rec = licensed_rec();
        let resp = Some((200u16, serde_json::json!({ "ok": true })));
        assert!(!apply_checkin_response_with(&mut rec, resp, verify_stub_ok));
        assert_eq!(rec.license_stamp.as_deref(), Some("OLD_STAMP"));
    }

    #[test]
    fn checkin_403_revoked_by_a_perpetual_purchase_drops_the_key_and_says_why() {
        let mut rec = licensed_rec();
        rec.trial_started_unix = now_unix() - 90 * 86_400;
        let resp = Some((403u16, serde_json::json!({ "error": "revoked", "reason": "replaced_by_lifetime" })));
        assert!(apply_checkin_response_with(&mut rec, resp, verify_stub_ok));
        assert!(rec.license_key.is_none() && rec.license_stamp.is_none());
        let status = serde_json::to_value(status_from(&rec)).unwrap();
        assert_eq!(status, serde_json::json!({ "state": "expired", "revoked_reason": "replaced_by_lifetime" }));
    }

    #[test]
    fn a_revocation_without_a_reason_reads_as_plain_expired() {
        let mut rec = licensed_rec();
        rec.revoked_reason = Some("replaced_by_lifetime".into());
        rec.trial_started_unix = now_unix() - 90 * 86_400;
        let resp = Some((403u16, serde_json::json!({ "error": "revoked" })));
        assert!(apply_checkin_response_with(&mut rec, resp, verify_stub_ok));
        assert_eq!(serde_json::to_value(status_from(&rec)).unwrap(), serde_json::json!({ "state": "expired" }));
    }

    #[test]
    fn checkin_403_revoked_hard_drops_but_not_trial_clock() {
        let mut rec = licensed_rec();
        rec.trial_started_unix = 12345;
        let resp = Some((403u16, serde_json::json!({ "error": "revoked" })));
        assert!(apply_checkin_response_with(&mut rec, resp, verify_stub_ok));
        assert!(rec.license_key.is_none());
        assert!(rec.license_stamp.is_none());
        assert_eq!(rec.trial_started_unix, 12345, "trial clock must NOT be reset on revoke");
    }

    #[test]
    fn checkin_403_inactive_and_expired_lapse_now_and_keep_the_key() {
        for err in ["inactive", "expired"] {
            let mut rec = licensed_rec();
            rec.activation_receipt = Some("RECEIPT".into());
            let resp = Some((403u16, serde_json::json!({ "error": err })));
            assert!(apply_checkin_response_with(&mut rec, resp, verify_stub_ok), "{err}");
            assert!(rec.license_stamp.is_none(), "{err}: the stamp is dropped");
            assert_eq!(rec.license_key.as_deref(), Some("MYKEY"), "{err}: the key stays for a renewal");
            assert_eq!(rec.activation_receipt.as_deref(), Some("RECEIPT"), "{err}: the seat receipt stays");
            // Without a stamp the record reads as trial or expired, never licensed.
            assert!(!matches!(status_at(&rec, now_unix()), LicenseStatus::Licensed { .. }), "{err}");
        }
    }

    #[test]
    fn checkin_403_inactive_lapses_an_app_store_key_like_any_other() {
        // The signer asks Apple at check-in, so its `inactive` is definitive
        // for every origin.
        let payload = B64.encode(serde_json::to_vec(&serde_json::json!({
            "key": "MYKEY", "subscription_type": "single_user", "origin": "app_store",
        })).unwrap());
        let mut rec = licensed_rec();
        rec.license_stamp = Some(format!("{payload}.SIG"));
        let resp = Some((403u16, serde_json::json!({ "error": "inactive" })));
        assert!(apply_checkin_response_with(&mut rec, resp, verify_stub_ok));
        assert!(rec.license_stamp.is_none());
        assert_eq!(rec.license_key.as_deref(), Some("MYKEY"));
    }

    #[test]
    fn checkin_403_inactive_without_a_stamp_changes_nothing() {
        let mut rec = licensed_rec();
        rec.license_stamp = None;
        let resp = Some((403u16, serde_json::json!({ "error": "inactive" })));
        assert!(!apply_checkin_response_with(&mut rec, resp, verify_stub_ok));
        assert_eq!(rec.license_key.as_deref(), Some("MYKEY"));
    }

    #[test]
    fn checkin_after_a_lapse_restamps_a_renewed_key() {
        let mut rec = licensed_rec();
        let lapse = Some((403u16, serde_json::json!({ "error": "inactive" })));
        assert!(apply_checkin_response_with(&mut rec, lapse, verify_stub_ok));
        let renewed = Some((200u16, serde_json::json!({ "stamp": "NEW_STAMP" })));
        assert!(apply_checkin_response_with(&mut rec, renewed, verify_stub_ok));
        assert_eq!(rec.license_stamp.as_deref(), Some("NEW_STAMP"));
    }

    #[test]
    fn checkin_offline_5xx_429_404_and_other_403s_keep_license() {
        for resp in [
            None,
            Some((500u16, serde_json::json!({}))),
            Some((503u16, serde_json::json!({ "error": "store_unavailable" }))),
            Some((429u16, serde_json::json!({ "error": "too_many_attempts" }))),
            Some((404u16, serde_json::json!({ "error": "unknown_key" }))),
            Some((403u16, serde_json::json!({ "error": "seat_limit" }))),
            Some((403u16, serde_json::json!({}))),
        ] {
            let mut rec = licensed_rec();
            assert!(!apply_checkin_response_with(&mut rec, resp.clone(), verify_stub_ok), "{resp:?}");
            assert_eq!(rec.license_key.as_deref(), Some("MYKEY"), "{resp:?}");
            assert_eq!(rec.license_stamp.as_deref(), Some("OLD_STAMP"), "{resp:?}");
        }
    }

    #[test]
    fn status_licensed_only_with_valid_bound_unexpired_stamp() {
        // No stamp → trial (fresh install, trial active).
        let mut rec = licensed_rec();
        rec.license_stamp = None;
        rec.trial_started_unix = now_unix();
        assert!(matches!(status_from(&rec), LicenseStatus::Trial { .. }));
    }

    #[test]
    fn perpetual_upgrade_is_offered_only_on_a_stripe_or_play_subscription() {
        let licensed = |perpetual: bool, origin: Option<&str>| LicenseStatus::Licensed {
            name: "".into(), email: "".into(), key: "K".into(), expires: Some(1),
            subscription_type: "single_user".into(), entitlements: vec![],
            perpetual, origin: origin.map(Into::into),
        };
        assert!(perpetual_upgrade_offered(&licensed(false, Some("stripe"))));
        assert!(perpetual_upgrade_offered(&licensed(false, Some("google_play"))));
        assert!(!perpetual_upgrade_offered(&licensed(true, Some("stripe"))), "already Perpetual");
        assert!(!perpetual_upgrade_offered(&licensed(false, Some("app_store"))), "Apple bills it");
        assert!(!perpetual_upgrade_offered(&licensed(false, None)), "a manual key");
        assert!(!perpetual_upgrade_offered(&LicenseStatus::Trial { days_left: 3, revoked_reason: None }));
    }

    #[test]
    fn paid_features_deny_only_on_expired() {
        assert!(!allows_paid_features(&Ok(LicenseStatus::Expired { revoked_reason: None })));
        assert!(allows_paid_features(&Ok(LicenseStatus::Trial { days_left: 1, revoked_reason: None })));
        assert!(allows_paid_features(&Ok(LicenseStatus::Licensed {
            name: "Sam".into(), email: "sam@x.io".into(), key: "K".into(),
            expires: None, subscription_type: "single_user".into(), entitlements: vec![],
            perpetual: false, origin: None,
        })));
        // Fail-open: a read error allows.
        assert!(allows_paid_features(&Err(LicenseError::Io("install.json unreadable".into()))));
    }
}
