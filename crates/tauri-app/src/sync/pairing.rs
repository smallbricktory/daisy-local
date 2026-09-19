// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! One-time pairing with a phone: the code the desktop shows, the request the
//! phone posts to `/pair`, the reply the desktop signs, and the keys both
//! derive. Wire shapes and derivations are the contract in smbr-daisy-ios
//! `docs/sync/protocol.md` sections 1-3; the phone is built against that file
//! and every byte here has to match it.
use base64::engine::general_purpose::{STANDARD as B64, URL_SAFE_NO_PAD as B64URL};
use base64::Engine;
use ed25519_dalek::Signer;
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use subtle::ConstantTimeEq;

use super::identity::{PairedPhone, SyncIdentity};
use crate::error::AppError;

const WINDOW_SECS: i64 = 90;
/// HKDF salt for every pairing derivation.
const SALT: &[u8] = b"daisy-pairing-v1";

/// Why a `/pair` request was refused. The status each one answers with is
/// the protocol's: 400 malformed, 403 bad MAC, 409 screen closed, 410 secret
/// expired or used, 500 the desktop could not read its own identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairError {
    BadRequest,
    BadMac,
    Closed,
    Expired,
    Internal,
    /// A phone is already paired under that device id with another signing
    /// key. 409, with the reason in the body: the id decides whose tombstones
    /// this desktop honours, so the pairing on record stands until the owner
    /// unpairs it.
    DeviceTaken,
}

pub struct PairingWindow {
    pub secret: [u8; 32],
    pub expires_unix: i64,
}

impl PairingWindow {
    pub fn open(now: i64) -> Self {
        let mut secret = [0u8; 32];
        rand::rng().fill_bytes(&mut secret);
        Self { secret, expires_unix: now + WINDOW_SECS }
    }
    pub fn is_open(&self, now: i64) -> bool {
        now < self.expires_unix
    }
}

/// The JSON inside the pairing code.
#[derive(Debug, Clone, Serialize)]
struct PairCode {
    pub v: u32,
    pub device_id: String,
    pub name: String,
    pub ed25519_pub_b64: String,
    pub tls_spki_sha256_b64: String,
    pub secret_b64: String,
    pub host: String,
    pub port: u16,
}

/// The text the QR carries: `daisy-pair:1:<base64url of the JSON, no padding>`.
pub fn pair_code_text(id: &SyncIdentity, w: &PairingWindow, name: &str, host: String, port: u16) -> Result<String, AppError> {
    let code = PairCode {
        v: 1,
        device_id: id.device_id.clone(),
        name: name.to_string(),
        ed25519_pub_b64: id.ed25519_pub_b64(),
        tls_spki_sha256_b64: id.tls_spki_sha256_b64(),
        secret_b64: B64.encode(w.secret),
        host,
        port,
    };
    Ok(format!("daisy-pair:1:{}", B64URL.encode(serde_json::to_vec(&code)?)))
}

#[derive(Debug, Clone, Deserialize)]
struct PairRequest {
    v: u32,
    device_id: String,
    name: String,
    platform: String,
    ed25519_pub_b64: String,
    tls_cert_der_b64: String,
}

#[derive(Debug, Clone, Serialize)]
struct PairReply {
    v: u32,
    device_id: String,
    name: String,
    x25519_pub_b64: String,
    phone_ed25519_pub_b64: String,
    port: u16,
}

/// The MAC a phone sends in `X-Daisy-Pair-Mac`: keyed with the code's secret
/// alone, so posting it proves the phone read the code.
pub fn pair_mac(secret: &[u8; 32], body: &[u8]) -> [u8; 32] {
    let mut key = [0u8; 32];
    Hkdf::<Sha256>::new(Some(SALT), secret)
        .expand(b"pair-mac", &mut key)
        .expect("32 bytes is a valid HKDF length");
    let mut mac = Hmac::<Sha256>::new_from_slice(&key).expect("any key length");
    mac.update(body);
    mac.finalize().into_bytes().into()
}

/// `(pairing_key, service_uuid, wake_uuid, address_uuid)` for one pairing.
/// Both sides derive these independently; none of them crosses the wire.
pub(crate) fn derive(secret: &[u8; 32], mac_pub: &[u8], phone_pub: &[u8]) -> ([u8; 32], String, String, String) {
    let hk = Hkdf::<Sha256>::new(Some(SALT), secret);
    let info = |label: &[u8]| [label, mac_pub, phone_pub].concat();
    let mut key = [0u8; 32];
    hk.expand(&info(b"payload-key"), &mut key).expect("valid length");
    let uuid = |label: &[u8]| {
        let mut b = [0u8; 16];
        hk.expand(&info(label), &mut b).expect("valid length");
        uuid::Builder::from_random_bytes(b).into_uuid().hyphenated().to_string().to_uppercase()
    };
    (key, uuid(b"service-uuid"), uuid(b"char-wake"), uuid(b"char-address"))
}

/// Verifies a `/pair` body against the open window and returns the phone to
/// store plus `(reply body, its Ed25519 signature)`. The body is verified and
/// the reply signed as the exact bytes on the wire; neither is re-serialized.
pub fn accept(
    id: &SyncIdentity,
    w: &PairingWindow,
    now: i64,
    body: &[u8],
    mac_b64: &str,
    name: &str,
    port: u16,
) -> Result<(PairedPhone, Vec<u8>, String), PairError> {
    if !w.is_open(now) {
        return Err(PairError::Expired);
    }
    let got = B64.decode(mac_b64).map_err(|_| PairError::BadMac)?;
    if !bool::from(got.ct_eq(&pair_mac(&w.secret, body)[..])) {
        return Err(PairError::BadMac);
    }
    let req: PairRequest = serde_json::from_slice(body).map_err(|_| PairError::BadRequest)?;
    if req.v != 1 || req.device_id.is_empty() || req.device_id.len() > 128 {
        return Err(PairError::BadRequest);
    }
    let phone_pub = B64.decode(&req.ed25519_pub_b64).map_err(|_| PairError::BadRequest)?;
    let phone_pub: [u8; 32] = phone_pub.try_into().map_err(|_| PairError::BadRequest)?;
    ed25519_dalek::VerifyingKey::from_bytes(&phone_pub).map_err(|_| PairError::BadRequest)?;
    if B64.decode(&req.tls_cert_der_b64).map_err(|_| PairError::BadRequest)?.is_empty() {
        return Err(PairError::BadRequest);
    }

    let sk = id.signing_key();
    let mac_pub = sk.verifying_key().to_bytes();
    let (key, service_uuid, wake_uuid, address_uuid) = derive(&w.secret, &mac_pub, &phone_pub);
    let reply = PairReply {
        v: 1,
        device_id: id.device_id.clone(),
        name: name.to_string(),
        x25519_pub_b64: id.x25519_pub_b64(),
        phone_ed25519_pub_b64: req.ed25519_pub_b64.clone(),
        port,
    };
    let reply_body = serde_json::to_vec(&reply).map_err(|_| PairError::BadRequest)?;
    let sig_b64 = B64.encode(sk.sign(&reply_body).to_bytes());
    let paired = PairedPhone {
        device_id: req.device_id,
        name: req.name.trim().chars().take(80).collect(),
        platform: req.platform,
        ed25519_pub_b64: req.ed25519_pub_b64,
        tls_cert_der_b64: req.tls_cert_der_b64,
        pairing_key_b64: B64.encode(key),
        service_uuid,
        wake_uuid,
        address_uuid,
        paired_at_unix: now,
        display_name: None,
    };
    Ok((paired, reply_body, sig_b64))
}

/// Removes the phone. False when no phone has that id. Its keys are not
/// remembered: an unpaired phone is refused on every route by not being in
/// `paired`, and pairing again needs a code the owner deliberately showed.
pub fn unpair(id: &mut SyncIdentity, device_id: &str) -> bool {
    let Some(pos) = id.paired.iter().position(|p| p.device_id == device_id) else {
        return false;
    };
    id.paired.remove(pos);
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{SigningKey, Verifier};

    fn mac() -> SyncIdentity {
        SyncIdentity::mint().unwrap()
    }

    struct Phone {
        key: SigningKey,
        id: String,
    }

    fn phone() -> Phone {
        let mut seed = [0u8; 32];
        rand::rng().fill_bytes(&mut seed);
        Phone { key: SigningKey::from_bytes(&seed), id: "phone-1".into() }
    }

    /// The `/pair` body a phone posts, and its MAC under the code's secret.
    fn request(p: &Phone, secret: &[u8; 32]) -> (Vec<u8>, String) {
        let body = serde_json::to_vec(&serde_json::json!({
            "v": 1,
            "device_id": p.id,
            "name": "Rowan's phone",
            "platform": "ios",
            "ed25519_pub_b64": B64.encode(p.key.verifying_key().to_bytes()),
            "tls_cert_der_b64": B64.encode([7u8; 91]),
        }))
        .unwrap();
        let mac = B64.encode(pair_mac(secret, &body));
        (body, mac)
    }

    #[test]
    fn window_closes_after_ninety_seconds() {
        let w = PairingWindow::open(1000);
        assert!(w.is_open(1000) && w.is_open(1089));
        assert!(!w.is_open(1090));
        assert_eq!(w.expires_unix, 1090);
    }

    #[test]
    fn the_code_is_the_prefixed_base64url_of_what_the_phone_pins() {
        let id = mac();
        let w = PairingWindow::open(1000);
        let text = pair_code_text(&id, &w, "studio-mac", "studio-mac.local".into(), 7845).unwrap();
        let b64 = text.strip_prefix("daisy-pair:1:").expect("prefix");
        assert!(!b64.contains('='), "no padding");
        let code: serde_json::Value = serde_json::from_slice(&B64URL.decode(b64).unwrap()).unwrap();
        assert_eq!(code["v"], 1);
        assert_eq!(code["device_id"], id.device_id);
        assert_eq!(code["name"], "studio-mac");
        assert_eq!(code["ed25519_pub_b64"], id.ed25519_pub_b64());
        assert_eq!(code["tls_spki_sha256_b64"], id.tls_spki_sha256_b64());
        assert_eq!(code["host"], "studio-mac.local");
        assert_eq!(code["port"], 7845);
        assert_eq!(B64.decode(code["secret_b64"].as_str().unwrap()).unwrap(), w.secret);
        assert_eq!(code.as_object().unwrap().len(), 8, "no field the phone does not expect");
    }

    #[test]
    fn a_correct_request_is_accepted_and_both_sides_derive_the_same_keys() {
        let id = mac();
        let w = PairingWindow::open(1000);
        let p = phone();
        let (body, mac_b64) = request(&p, &w.secret);
        let (paired, reply, sig_b64) = accept(&id, &w, 1010, &body, &mac_b64, "studio-mac", 7845).unwrap();
        assert_eq!(paired.device_id, "phone-1");
        assert_eq!(paired.platform, "ios");
        assert_eq!(paired.paired_at_unix, 1010);

        // The phone verifies the reply bytes under the code's Ed25519 key.
        let sig = ed25519_dalek::Signature::from_bytes(&B64.decode(&sig_b64).unwrap().try_into().unwrap());
        id.signing_key().verifying_key().verify(&reply, &sig).unwrap();
        let reply: serde_json::Value = serde_json::from_slice(&reply).unwrap();
        assert_eq!(reply["v"], 1);
        assert_eq!(reply["device_id"], id.device_id);
        assert_eq!(reply["x25519_pub_b64"], id.x25519_pub_b64());
        assert_eq!(reply["phone_ed25519_pub_b64"], B64.encode(p.key.verifying_key().to_bytes()));
        assert_eq!(reply["port"], 7845);

        // Derivations are a pure function of (secret, both pubs).
        let (k, service, wake, addr) = derive(&w.secret, &id.signing_key().verifying_key().to_bytes(), &p.key.verifying_key().to_bytes());
        assert_eq!(paired.pairing_key_b64, B64.encode(k));
        assert_eq!((&paired.service_uuid, &paired.wake_uuid, &paired.address_uuid), (&service, &wake, &addr));
        for u in [service, wake, addr] {
            assert_eq!(u.len(), 36);
            assert_eq!(&u[14..15], "4", "version nibble");
            assert!(matches!(&u[19..20], "8" | "9" | "A" | "B"), "variant bits: {u}");
        }
    }

    #[test]
    fn the_three_uuids_differ() {
        let (_, s, w, a) = derive(&[1u8; 32], &[2u8; 32], &[3u8; 32]);
        assert!(s != w && w != a && s != a);
    }

    #[test]
    fn a_mac_under_another_secret_is_refused() {
        let id = mac();
        let w = PairingWindow::open(1000);
        let (body, _) = request(&phone(), &w.secret);
        let wrong = B64.encode(pair_mac(&[9u8; 32], &body));
        assert_eq!(accept(&id, &w, 1010, &body, &wrong, "studio-mac", 7845).unwrap_err(), PairError::BadMac);
    }

    #[test]
    fn a_body_edited_after_the_mac_is_refused() {
        let id = mac();
        let w = PairingWindow::open(1000);
        let (body, mac_b64) = request(&phone(), &w.secret);
        let tampered = String::from_utf8(body).unwrap().replace("Rowan", "Mallo").into_bytes();
        assert_eq!(accept(&id, &w, 1010, &tampered, &mac_b64, "studio-mac", 7845).unwrap_err(), PairError::BadMac);
    }

    #[test]
    fn expired_window_is_refused() {
        let id = mac();
        let w = PairingWindow::open(1000);
        let (body, mac_b64) = request(&phone(), &w.secret);
        assert_eq!(accept(&id, &w, 1091, &body, &mac_b64, "studio-mac", 7845).unwrap_err(), PairError::Expired);
    }

    #[test]
    fn an_unpaired_phone_can_pair_again() {
        let mut id = mac();
        let w = PairingWindow::open(1000);
        let p = phone();
        let (body, mac_b64) = request(&p, &w.secret);
        let (paired, _, _) = accept(&id, &w, 1010, &body, &mac_b64, "studio-mac", 7845).unwrap();
        id.paired.push(paired);
        assert!(unpair(&mut id, "phone-1"));
        assert!(id.paired.is_empty());

        let w2 = PairingWindow::open(2000);
        let (body, mac_b64) = request(&p, &w2.secret);
        assert!(accept(&id, &w2, 2001, &body, &mac_b64, "studio-mac", 7845).is_ok(), "the owner showed a fresh code");
        assert!(!unpair(&mut id, "phone-1"), "unknown id changes nothing");
    }

    #[test]
    fn protocol_version_other_than_one_is_refused() {
        let id = mac();
        let w = PairingWindow::open(1000);
        let body = serde_json::to_vec(&serde_json::json!({
            "v": 2, "device_id": "phone-1", "name": "n", "platform": "ios",
            "ed25519_pub_b64": B64.encode([0u8; 32]), "tls_cert_der_b64": B64.encode([7u8; 91]),
        }))
        .unwrap();
        let mac_b64 = B64.encode(pair_mac(&w.secret, &body));
        assert_eq!(accept(&id, &w, 1010, &body, &mac_b64, "studio-mac", 7845).unwrap_err(), PairError::BadRequest);
    }
}
