//! Trial + licensing — opaque-key model.
//!
//! A license is an opaque key (`XXXX-XXXX-XXXX-XXXX-XXXX`); it carries no
//! data. On check-in the server returns a short-lived Ed25519-signed
//! validity stamp `{ key, name, email, subscription_type, expires }`. The
//! client caches the stamp and, offline, treats the license as valid while
//! `now < stamp.expires`, the signature verifies, and the stamp's `key`
//! matches the stored key. The stamp TTL is enforced entirely by the client.
//!
//! On revocation the server stops issuing fresh stamps (or returns
//! `403 revoked` on check-in); the cached stamp lapses at its expiry.

use crate::error::{AppError, Result};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use ed25519_dalek::{Signature, VerifyingKey};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Trial length in days.
pub const TRIAL_DAYS: i64 = 30;

const ACTIVATE_URL: &str = "https://daisy.smbr.app/api/activate";
const DEACTIVATE_URL: &str = "https://daisy.smbr.app/api/deactivate";
/// Check-in (heartbeat) endpoint — returns a fresh signed validity stamp.
const REFRESH_URL: &str = "https://daisy.smbr.app/api/license/refresh";
/// Minimum spacing between check-in attempts.
const CHECKIN_THROTTLE_SECS: i64 = 86_400;

/// POST a JSON body, returning (HTTP status, parsed body) or None on a
/// network/transport error. 8s timeout.
fn post_json(url: &str, body: serde_json::Value) -> Option<(u16, serde_json::Value)> {
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(8))
        .build()
        .ok()?;
    let resp = client.post(url).json(&body).send().ok()?;
    let status = resp.status().as_u16();
    let json = resp.json::<serde_json::Value>().unwrap_or(serde_json::Value::Null);
    Some((status, json))
}

/// Vendor public key (base64 of 32 raw bytes) that stamp signatures are
/// verified against.
const LICENSE_PUBKEY_B64: &str = "B0VAcPcVxjlEZHeeK1FsccGMo927fu9L5SPm0LQ1d9Q=";

use crate::now_unix;

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
}

impl InstallRecord {
    fn path() -> Result<PathBuf> {
        let pd = directories::ProjectDirs::from("ai", "daisy", "Daisy")
            .ok_or_else(|| AppError::Config("no config dir".into()))?;
        let dir = pd.config_dir().to_path_buf();
        syncsafe::create_dir_all(&dir).map_err(|e| AppError::Io(e.to_string()))?;
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
                }
            }
        };
        rec.last_seen_unix = now_unix();
        rec.save()?;
        Ok(rec)
    }

    fn save(&self) -> Result<()> {
        let p = Self::path()?;
        let tmp = p.with_extension("json.tmp");
        syncsafe::write(&tmp, serde_json::to_vec_pretty(self).map_err(|e| AppError::Config(e.to_string()))?)
            .map_err(|e| AppError::Io(e.to_string()))?;
        syncsafe::rename(&tmp, &p).map_err(|e| AppError::Io(e.to_string()))?;
        Ok(())
    }
}

fn hex_encode(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Canonical form of an opaque key: strips dashes and all whitespace;
/// preserves case (the alphabet is case-sensitive base62).
fn canonicalize_key(input: &str) -> String {
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
    /// Feature entitlements granted to this key (e.g. "daisy_cloud"). Absent
    /// in older stamps → empty.
    #[serde(default)]
    entitlements: Vec<String>,
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
    },
    Trial { days_left: i64 },
    Expired,
}

fn status_from(rec: &InstallRecord) -> LicenseStatus {
    // A valid, bound, unexpired stamp wins.
    if let (Some(key), Some(stamp)) = (rec.license_key.as_ref(), rec.license_stamp.as_ref()) {
        if let Some(p) = verify_stamp(stamp, key) {
            if p.expires.map(|e| e > now_unix()).unwrap_or(true) {
                return LicenseStatus::Licensed {
                    name: p.name,
                    email: p.email,
                    key: key.clone(),
                    expires: p.expires,
                    subscription_type: p.subscription_type,
                    entitlements: p.entitlements,
                };
            }
        }
    }
    let now = now_unix();
    // Clock rolled back below the last launch → treat the trial as over.
    if now < rec.last_seen_unix {
        return LicenseStatus::Expired;
    }
    let elapsed_days = (now - rec.trial_started_unix) / 86_400;
    let left = TRIAL_DAYS - elapsed_days;
    if left > 0 {
        LicenseStatus::Trial { days_left: left }
    } else {
        LicenseStatus::Expired
    }
}

pub fn license_status_impl() -> Result<LicenseStatus> {
    Ok(status_from(&InstallRecord::load_or_create()?))
}

/// Whether paid features are allowed: a valid license OR an active trial.
/// Only a definitive `Expired` status denies; errors reading the status
/// allow.
pub fn features_enabled() -> bool {
    !matches!(license_status_impl(), Ok(LicenseStatus::Expired))
}

/// Entitlement string that unlocks the Daisy Cloud gateway provider.
pub const ENT_DAISY_CLOUD: &str = "daisy_cloud";

/// Whether the current license stamp carries the Daisy Cloud entitlement.
pub fn gateway_entitled() -> bool {
    matches!(
        license_status_impl(),
        Ok(LicenseStatus::Licensed { entitlements, .. })
            if entitlements.iter().any(|e| e == ENT_DAISY_CLOUD)
    )
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
    post_json(ACTIVATE_URL, activate_body(key, install_id, install_pubkey))
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
                "inactive" => "This subscription is inactive — renew it to reactivate.".into(),
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

/// `install_pubkey` is this install's base64 Ed25519 public key (its private
/// half signs gateway requests); it is registered with the seat on activate.
/// `None` when the vault is locked.
pub fn activate_license_impl(key: String, install_pubkey: Option<String>) -> Result<LicenseStatus> {
    let canonical = canonicalize_key(&key);
    if canonical.is_empty() {
        return Err(AppError::Config("Enter your license key.".into()));
    }
    let mut rec = InstallRecord::load_or_create()?;
    // Claim the seat and register the install's gateway pubkey. A successful
    // server response is required to license this device.
    let resp = post_activate(&canonical, &rec.install_id, install_pubkey.as_deref());
    if let Some(e) = activate_error(&resp) {
        return Err(e);
    }
    let receipt = resp
        .and_then(|(_, body)| body.get("receipt").and_then(|r| r.as_str()).map(String::from));
    rec.license_key = Some(canonical.clone());
    rec.activation_receipt = receipt;
    // The key/seat is persisted before the first check-in.
    rec.save()?;
    // Check in for the first stamp; `last_checkin_unix` is set only when a
    // stamp is stored.
    let resp = post_json(REFRESH_URL, serde_json::json!({ "key": canonical }));
    if apply_checkin_response(&mut rec, resp, verify_stamp) {
        rec.last_checkin_unix = now_unix();
        rec.save()?;
    }
    Ok(status_from(&rec))
}

/// Register (or refresh) this install's Daisy Cloud gateway public key by
/// re-activating the same install_id with `install_pubkey`. Requires an
/// active license key.
pub fn register_gateway_install_impl(pubkey_b64: &str) -> Result<()> {
    let rec = InstallRecord::load_or_create()?;
    let key = rec.license_key.clone().ok_or_else(|| {
        AppError::Config("Activate your license before enabling Daisy Cloud.".into())
    })?;
    let resp = post_activate(&key, &rec.install_id, Some(pubkey_b64));
    match activate_error(&resp) {
        None => Ok(()),
        Some(_) if matches!(&resp, Some((403, _))) => Err(AppError::Config(
            "Seat limit reached — deactivate Daisy on another device first.".into(),
        )),
        Some(e) => Err(e),
    }
}

/// Release this device's seat (server-side, authenticated by the receipt) and
/// remove the license locally, reverting to trial/expired. The network call
/// is best-effort.
pub fn deactivate_license_impl() -> Result<LicenseStatus> {
    let mut rec = InstallRecord::load_or_create()?;
    if let Some(key) = rec.license_key.clone() {
        let _ = post_json(
            DEACTIVATE_URL,
            serde_json::json!({
                "key": key,
                "install_id": rec.install_id,
                "receipt": rec.activation_receipt,
            }),
        );
    }
    rec.license_key = None;
    rec.license_stamp = None;
    rec.activation_receipt = None;
    rec.save()?;
    Ok(status_from(&rec))
}

/// Apply a `/api/license/refresh` (check-in) response to an InstallRecord.
/// Returns true if the record changed (caller persists). Pure — no I/O.
///
/// Only `403 revoked` hard-drops the license; every other failure keeps the
/// existing stamp. `verify` is an injected verification function.
fn apply_checkin_response(
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
                // Hard drop; the trial clock is not touched.
                log::warn!("license check-in: 403 revoked — clearing key + stamp");
                rec.license_key = None;
                rec.license_stamp = None;
                return true;
            }
            log::warn!("license check-in: 403 {err} — keeping current stamp; lapses at its expiry");
        }
        Some((s, _)) => log::warn!("license check-in: HTTP {s} — keeping current stamp"),
        None => log::info!("license check-in: server unreachable — keeping current stamp"),
    }
    false
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
    let resp = post_json(REFRESH_URL, serde_json::json!({ "key": key }));
    if apply_checkin_response(&mut rec, resp, verify_stamp) {
        rec.save()?;
    }
    Ok(status_from(&rec))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

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
    fn activate_body_includes_pubkey_only_when_present() {
        let bare = activate_body("KEY", "INST", None);
        assert_eq!(bare["key"], "KEY");
        assert!(bare.get("install_pubkey").is_none());
        let reg = activate_body("KEY", "INST", Some("PUB"));
        assert_eq!(reg["install_pubkey"], "PUB");
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
                            entitlements: vec![] })
    }
    fn verify_stub_none(_: &str, _: &str) -> Option<StampPayload> { None }

    fn licensed_rec() -> InstallRecord {
        InstallRecord {
            install_id: "id".into(), key_hex: "deadbeef".into(),
            trial_started_unix: 0, last_seen_unix: 0,
            license_key: Some("MYKEY".into()),
            license_stamp: Some("OLD_STAMP".into()),
            activation_receipt: None, last_checkin_unix: 0,
        }
    }

    #[test]
    fn checkin_200_valid_stamp_stored() {
        let mut rec = licensed_rec();
        let resp = Some((200u16, serde_json::json!({ "ok": true, "stamp": "NEW_STAMP" })));
        assert!(apply_checkin_response(&mut rec, resp, verify_stub_ok));
        assert_eq!(rec.license_stamp.as_deref(), Some("NEW_STAMP"));
    }

    #[test]
    fn checkin_200_bad_stamp_keeps_existing() {
        let mut rec = licensed_rec();
        let resp = Some((200u16, serde_json::json!({ "stamp": "FORGED" })));
        assert!(!apply_checkin_response(&mut rec, resp, verify_stub_none));
        assert_eq!(rec.license_stamp.as_deref(), Some("OLD_STAMP"));
    }

    #[test]
    fn checkin_200_no_stamp_field_keeps_existing() {
        let mut rec = licensed_rec();
        let resp = Some((200u16, serde_json::json!({ "ok": true })));
        assert!(!apply_checkin_response(&mut rec, resp, verify_stub_ok));
        assert_eq!(rec.license_stamp.as_deref(), Some("OLD_STAMP"));
    }

    #[test]
    fn checkin_403_revoked_hard_drops_but_not_trial_clock() {
        let mut rec = licensed_rec();
        rec.trial_started_unix = 12345;
        let resp = Some((403u16, serde_json::json!({ "error": "revoked" })));
        assert!(apply_checkin_response(&mut rec, resp, verify_stub_ok));
        assert!(rec.license_key.is_none());
        assert!(rec.license_stamp.is_none());
        assert_eq!(rec.trial_started_unix, 12345, "trial clock must NOT be reset on revoke");
    }

    #[test]
    fn checkin_403_inactive_keeps_license() {
        let mut rec = licensed_rec();
        let resp = Some((403u16, serde_json::json!({ "error": "inactive" })));
        assert!(!apply_checkin_response(&mut rec, resp, verify_stub_ok));
        assert_eq!(rec.license_key.as_deref(), Some("MYKEY"));
        assert_eq!(rec.license_stamp.as_deref(), Some("OLD_STAMP"));
    }

    #[test]
    fn checkin_404_and_5xx_and_network_keep_license() {
        for resp in [
            Some((404u16, serde_json::json!({ "error": "unknown_key" }))),
            Some((500u16, serde_json::json!({}))),
            None,
        ] {
            let mut rec = licensed_rec();
            assert!(!apply_checkin_response(&mut rec, resp, verify_stub_ok));
            assert_eq!(rec.license_key.as_deref(), Some("MYKEY"));
            assert_eq!(rec.license_stamp.as_deref(), Some("OLD_STAMP"));
        }
    }

    #[test]
    fn activate_error_matrix() {
        assert!(activate_error(&Some((200, serde_json::json!({ "receipt": "r" })))).is_none());
        assert!(activate_error(&Some((400, serde_json::json!({ "error": "invalid_key" })))).is_some());
        assert!(activate_error(&Some((404, serde_json::json!({ "error": "unknown_key" })))).is_some());
        assert!(activate_error(&Some((403, serde_json::json!({ "error": "revoked" })))).is_some());
        assert!(activate_error(&Some((403, serde_json::json!({ "error": "seat_limit" })))).is_some());
        assert!(activate_error(&Some((429, serde_json::json!({ "error": "rate_limited" })))).is_some());
        assert!(activate_error(&None).is_some());
    }

    #[test]
    fn status_licensed_only_with_valid_bound_unexpired_stamp() {
        // No stamp → trial (fresh install, trial active).
        let mut rec = licensed_rec();
        rec.license_stamp = None;
        rec.trial_started_unix = now_unix();
        assert!(matches!(status_from(&rec), LicenseStatus::Trial { .. }));
    }
}
