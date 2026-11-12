// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! One-time pairing with a phone: the key the desktop shows, the exchange
//! the phone runs against it on `/pair/start`, the request it then posts to
//! `/pair`, the reply the desktop signs, and the keys both derive.
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use ed25519_dalek::Signer;
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use rand::{RngCore, TryRngCore};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

use super::identity::{PairedPhone, SyncIdentity};

/// How long a key stays live when nobody pairs with it.
pub const KEY_SECS: i64 = 600;
/// Sessions a key holds at once; a start past this replaces the oldest.
const MAX_SESSIONS: usize = 4;
/// HKDF salt for the pairing-key exchange.
const SALT_V2: &[u8] = b"daisy-pairing-v2";
/// HKDF salt for everything derived from the exchange's secret S: the `/pair`
/// MAC key, the payload key and the three BLE UUIDs.
const SALT: &[u8] = b"daisy-pairing-v1";
const CROCKFORD: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// Why a pairing request was refused. Each has the `error` string a phone
/// branches on and a status: 400 malformed or bad point, 403 bad key, bad MAC
/// or certificate mismatch, 409 no key or device id taken, 410 key expired,
/// 500 the desktop could not read its own identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairError {
    BadRequest,
    BadPoint,
    BadKey,
    BadMac,
    CertMismatch,
    Closed,
    Expired,
    Internal,
    /// A phone is already paired under that device id with another signing key.
    DeviceTaken,
}

impl PairError {
    pub fn reason(self) -> &'static str {
        match self {
            PairError::BadRequest => "malformed",
            PairError::BadPoint => "bad_point",
            PairError::BadKey => "bad_key",
            PairError::BadMac => "bad_mac",
            PairError::CertMismatch => "cert_mismatch",
            PairError::Closed => "closed",
            PairError::Expired => "expired",
            PairError::Internal => "internal",
            PairError::DeviceTaken => "device_id_taken",
        }
    }
}

/// The phone's half of the transcript, as `/pair/start` claimed it: its
/// device id, signing key and the SHA-256 of its TLS certificate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhoneClaim {
    pub device_id: [u8; 16],
    pub ed25519_pub: [u8; 32],
    pub cert_sha256: [u8; 32],
}

impl PhoneClaim {
    /// `A = device_id ‖ ed25519_pub ‖ SHA-256(tls_cert_der)`.
    pub fn bytes(&self) -> Vec<u8> {
        [&self.device_id[..], &self.ed25519_pub, &self.cert_sha256].concat()
    }
}

/// One started exchange. Its secret is zeroed when it is dropped.
#[derive(Zeroize, ZeroizeOnDrop)]
struct Session {
    #[zeroize(skip)]
    id_b64: String,
    secret: [u8; 32],
    #[zeroize(skip)]
    phone: PhoneClaim,
}

/// The key on the pairing screen, the host it was shown with, and the
/// sessions its holders have started. The key and every session secret are
/// zeroed when it is dropped.
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct PairingKey {
    pub key: [u8; 10],
    #[zeroize(skip)]
    pub host: String,
    #[zeroize(skip)]
    pub expires_unix: i64,
    sessions: Vec<Session>,
}

impl PairingKey {
    pub fn mint(host: String, now: i64) -> Self {
        let mut key = [0u8; 10];
        os_random(&mut key);
        Self { key, host, expires_unix: now + KEY_SECS, sessions: Vec::new() }
    }
    pub fn is_live(&self, now: i64) -> bool {
        now < self.expires_unix
    }
}

/// Fills `out` from the operating system's CSPRNG. Panics when the OS has no
/// randomness to give.
fn os_random(out: &mut [u8]) {
    rand::rngs::OsRng.unwrap_err().fill_bytes(out);
}

/// The key as 16 Crockford base32 characters, 5-bit groups MSB-first.
pub fn key_text(key: &[u8; 10]) -> String {
    let bits = key.iter().fold(0u128, |acc, b| (acc << 8) | *b as u128);
    (0..16).rev().map(|i| CROCKFORD[((bits >> (i * 5)) & 31) as usize] as char).collect()
}

/// The key as typed or scanned: case-insensitive, spaces and dashes dropped,
/// `O` read as `0` and `I`/`L` as `1`. `None` for anything else.
pub fn parse_key(text: &str) -> Option<[u8; 10]> {
    let mut bits = 0u128;
    let mut n = 0;
    for c in text.chars().filter(|c| *c != ' ' && *c != '-') {
        let c = match c.to_ascii_uppercase() {
            'O' => '0',
            'I' | 'L' => '1',
            c => c,
        };
        let v = CROCKFORD.iter().position(|a| *a as char == c)?;
        bits = (bits << 5) | v as u128;
        n += 1;
        if n > 16 {
            return None;
        }
    }
    (n == 16).then(|| {
        let mut key = [0u8; 10];
        for (i, b) in key.iter_mut().enumerate() {
            *b = (bits >> (8 * (9 - i))) as u8;
        }
        key
    })
}

/// The first 10 bits of `SHA-256("daisy-pair-tag" ‖ key)`, as 2 characters.
pub fn routing_tag(key: &[u8; 10]) -> String {
    let d = Sha256::new().chain_update(b"daisy-pair-tag").chain_update(key).finalize();
    let bits = ((d[0] as u16) << 2) | (d[1] as u16 >> 6);
    [CROCKFORD[(bits >> 5) as usize] as char, CROCKFORD[(bits & 31) as usize] as char].iter().collect()
}

/// The text the QR carries: `daisy-pair:2:<key>@<host>:<port>`.
pub fn qr_text(key: &[u8; 10], host: &str, port: u16) -> String {
    format!("daisy-pair:2:{}@{host}:{port}", key_text(key))
}

fn hmac(key: &[u8], parts: &[&[u8]]) -> [u8; 32] {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("any key length");
    for p in parts {
        mac.update(p);
    }
    mac.finalize().into_bytes().into()
}

/// `cP`: the phone's proof that it holds the key, over A and its fresh
/// exchange values.
pub fn phone_proof(key: &[u8; 10], a: &[u8], eph_pub: &[u8; 32], nonce: &[u8; 32]) -> [u8; 32] {
    let mut kp = Zeroizing::new([0u8; 32]);
    Hkdf::<Sha256>::new(Some(SALT_V2), key).expand(b"phone-start", &mut kp[..]).expect("valid length");
    hmac(&kp[..], &[b"daisy-pair-v2-start", a, eph_pub, nonce])
}

/// The most bytes of UTF-8 the desktop's name takes in B.
pub const NAME_MAX_BYTES: usize = 255;

/// The desktop's name as B and both replies carry it: at most
/// `NAME_MAX_BYTES`, cut at the last whole character.
pub fn claim_name(name: &str) -> &str {
    let mut end = name.len().min(NAME_MAX_BYTES);
    while !name.is_char_boundary(end) {
        end -= 1;
    }
    &name[..end]
}

/// Appends `bytes` after its length as a little-endian u16. Every caller's
/// input is bounded far below 65535 bytes.
fn len_prefixed(out: &mut Vec<u8>, bytes: &[u8]) {
    debug_assert!(bytes.len() <= u16::MAX as usize, "{} bytes do not fit a u16 length", bytes.len());
    out.extend_from_slice(&(bytes.len() as u16).to_le_bytes());
    out.extend_from_slice(bytes);
}

/// B: the desktop's identity as the phone will store it.
#[allow(clippy::too_many_arguments)]
pub fn desktop_claim(
    device_id: &[u8; 16],
    ed25519_pub: &[u8; 32],
    x25519_pub: &[u8; 32],
    tls_spki_sha256: &[u8; 32],
    name: &str,
    host: &str,
    port: u16,
) -> Vec<u8> {
    let mut b = [&device_id[..], ed25519_pub, x25519_pub, tls_spki_sha256].concat();
    len_prefixed(&mut b, name.as_bytes());
    len_prefixed(&mut b, host.as_bytes());
    b.extend_from_slice(&port.to_le_bytes());
    b
}

/// T: everything both sides saw, in one order.
pub fn transcript(a: &[u8], b: &[u8], eph_phone: &[u8; 32], eph_desktop: &[u8; 32], nonce_phone: &[u8; 32], nonce_desktop: &[u8; 32]) -> Vec<u8> {
    let mut t = b"daisy-pair-v2".to_vec();
    len_prefixed(&mut t, a);
    len_prefixed(&mut t, b);
    for part in [eph_phone, eph_desktop, nonce_phone, nonce_desktop] {
        t.extend_from_slice(part);
    }
    t
}

/// `(cD, S)` from the key, the exchange's shared secret Z and T.
pub fn confirm(key: &[u8; 10], z: &[u8; 32], t: &[u8]) -> ([u8; 32], [u8; 32]) {
    let ikm = Zeroizing::new([&key[..], z].concat());
    let hk = Hkdf::<Sha256>::new(Some(SALT_V2), &ikm);
    let th = Sha256::digest(t);
    let mut kd = Zeroizing::new([0u8; 32]);
    hk.expand(&[&b"desktop-confirm"[..], &th].concat(), &mut kd[..]).expect("valid length");
    let mut s = [0u8; 32];
    hk.expand(&[&b"secret"[..], &th].concat(), &mut s).expect("valid length");
    (hmac(&kd[..], &[t]), s)
}

fn b64_array<const N: usize>(s: &str) -> Result<[u8; N], PairError> {
    B64.decode(s).ok().and_then(|v| v.try_into().ok()).ok_or(PairError::BadRequest)
}

/// The 16 bytes of a 32-character lowercase hex device id.
fn device_id_bytes(hex: &str) -> Result<[u8; 16], PairError> {
    if hex.len() != 32 || !hex.bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)) {
        return Err(PairError::BadRequest);
    }
    let mut out = [0u8; 16];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).map_err(|_| PairError::BadRequest)?;
    }
    Ok(out)
}

#[derive(Deserialize)]
struct StartRequest {
    v: u32,
    device_id: String,
    ed25519_pub_b64: String,
    tls_cert_der_b64: String,
    eph_pub_b64: String,
    nonce_b64: String,
    #[serde(rename = "cP_b64")]
    cp_b64: String,
}

#[derive(Serialize)]
struct StartReply {
    v: u32,
    session_id: String,
    device_id: String,
    name: String,
    ed25519_pub_b64: String,
    x25519_pub_b64: String,
    tls_spki_sha256_b64: String,
    host: String,
    port: u16,
    eph_pub_b64: String,
    nonce_b64: String,
    #[serde(rename = "cD_b64")]
    cd_b64: String,
}

/// The desktop's fresh values for one `/pair/start`, zeroed when dropped.
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct StartRandom {
    pub eph_secret: [u8; 32],
    pub nonce: [u8; 32],
    pub session_id: [u8; 16],
}

impl StartRandom {
    pub fn fresh() -> Self {
        let mut r = Self { eph_secret: [0; 32], nonce: [0; 32], session_id: [0; 16] };
        os_random(&mut r.eph_secret);
        os_random(&mut r.nonce);
        os_random(&mut r.session_id);
        r
    }
}

/// `/pair/start`: checks the phone's proof of the key and answers with this
/// desktop's identity and `cD`, keeping a session for `/pair`. Nothing is
/// kept before `cP` verifies. `peer_cert` is the client certificate on this
/// connection.
#[allow(clippy::too_many_arguments)]
pub fn start(
    id: &SyncIdentity,
    pk: &mut PairingKey,
    now: i64,
    body: &[u8],
    peer_cert: Option<&[u8]>,
    name: &str,
    port: u16,
    fresh: StartRandom,
) -> Result<Vec<u8>, PairError> {
    let name = claim_name(name);
    if !pk.is_live(now) {
        return Err(PairError::Expired);
    }
    let req: StartRequest = serde_json::from_slice(body).map_err(|_| PairError::BadRequest)?;
    if req.v != 2 {
        return Err(PairError::BadRequest);
    }
    let phone_device = device_id_bytes(&req.device_id)?;
    let phone_pub: [u8; 32] = b64_array(&req.ed25519_pub_b64)?;
    let cert = B64.decode(&req.tls_cert_der_b64).map_err(|_| PairError::BadRequest)?;
    let eph_phone: [u8; 32] = b64_array(&req.eph_pub_b64)?;
    let nonce_phone: [u8; 32] = b64_array(&req.nonce_b64)?;
    let cp: [u8; 32] = b64_array(&req.cp_b64)?;
    if cert.is_empty() || peer_cert != Some(&cert[..]) {
        return Err(PairError::CertMismatch);
    }
    let phone = PhoneClaim { device_id: phone_device, ed25519_pub: phone_pub, cert_sha256: Sha256::digest(&cert).into() };
    let a = phone.bytes();
    if !bool::from(cp.ct_eq(&phone_proof(&pk.key, &a, &eph_phone, &nonce_phone))) {
        return Err(PairError::BadKey);
    }

    let eph = x25519_dalek::StaticSecret::from(fresh.eph_secret);
    let eph_desktop = x25519_dalek::PublicKey::from(&eph).to_bytes();
    let z = Zeroizing::new(eph.diffie_hellman(&x25519_dalek::PublicKey::from(eph_phone)).to_bytes());
    if *z == [0u8; 32] {
        return Err(PairError::BadPoint);
    }
    let sk = id.signing_key();
    let ed_pub = sk.verifying_key().to_bytes();
    let x_pub: [u8; 32] = b64_array(&id.x25519_pub_b64()).map_err(|_| PairError::Internal)?;
    let spki: [u8; 32] = b64_array(&id.tls_spki_sha256_b64()).map_err(|_| PairError::Internal)?;
    let desktop_device = device_id_bytes(&id.device_id).map_err(|_| PairError::Internal)?;
    let b = desktop_claim(&desktop_device, &ed_pub, &x_pub, &spki, name, &pk.host, port);
    let t = transcript(&a, &b, &eph_phone, &eph_desktop, &nonce_phone, &fresh.nonce);
    let (cd, mut secret) = confirm(&pk.key, &z, &t);

    let session_id = B64.encode(fresh.session_id);
    if pk.sessions.len() == MAX_SESSIONS {
        pk.sessions.remove(0);
    }
    pk.sessions.push(Session { id_b64: session_id.clone(), secret, phone });
    secret.zeroize();
    serde_json::to_vec(&StartReply {
        v: 2,
        session_id,
        device_id: id.device_id.clone(),
        name: name.to_string(),
        ed25519_pub_b64: B64.encode(ed_pub),
        x25519_pub_b64: B64.encode(x_pub),
        tls_spki_sha256_b64: B64.encode(spki),
        host: pk.host.clone(),
        port,
        eph_pub_b64: B64.encode(eph_desktop),
        nonce_b64: B64.encode(fresh.nonce),
        cd_b64: B64.encode(cd),
    })
    .map_err(|_| PairError::Internal)
}

/// The secret S of the session a `/pair` names, once its body and this
/// connection's client certificate are the phone that started it.
pub fn session_secret(pk: &PairingKey, now: i64, session_id: &str, body: &[u8], peer_cert: Option<&[u8]>) -> Result<[u8; 32], PairError> {
    if !pk.is_live(now) {
        return Err(PairError::Expired);
    }
    let session = pk.sessions.iter().find(|s| s.id_b64 == session_id).ok_or(PairError::BadRequest)?;
    let req: PairRequest = serde_json::from_slice(body).map_err(|_| PairError::BadRequest)?;
    let cert = B64.decode(&req.tls_cert_der_b64).map_err(|_| PairError::BadRequest)?;
    let claimed = PhoneClaim {
        device_id: device_id_bytes(&req.device_id)?,
        ed25519_pub: b64_array(&req.ed25519_pub_b64)?,
        cert_sha256: Sha256::digest(&cert).into(),
    };
    if claimed != session.phone || peer_cert != Some(&cert[..]) {
        return Err(PairError::CertMismatch);
    }
    Ok(session.secret)
}

/// Ends the session a failed `/pair` MAC named; the key and its other
/// sessions stay.
pub fn end_session(pk: &mut PairingKey, session_id: &str) {
    pk.sessions.retain(|s| s.id_b64 != session_id);
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

/// The MAC a phone sends in `X-Daisy-Pair-Mac`, keyed from the session's
/// secret S alone, so posting it proves the phone finished the exchange.
pub fn pair_mac(secret: &[u8; 32], body: &[u8]) -> [u8; 32] {
    let mut key = Zeroizing::new([0u8; 32]);
    Hkdf::<Sha256>::new(Some(SALT), secret)
        .expand(b"pair-mac", &mut key[..])
        .expect("32 bytes is a valid HKDF length");
    hmac(&key[..], &[body])
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

/// Verifies a `/pair` body under a session's secret S and returns the phone
/// to store plus `(reply body, its Ed25519 signature)`. The body is verified
/// and the reply signed as the exact bytes on the wire; neither is
/// re-serialized.
pub fn accept(
    id: &SyncIdentity,
    secret: &[u8; 32],
    now: i64,
    body: &[u8],
    mac_b64: &str,
    name: &str,
    port: u16,
) -> Result<(PairedPhone, Vec<u8>, String), PairError> {
    let name = claim_name(name);
    let got = B64.decode(mac_b64).map_err(|_| PairError::BadMac)?;
    if !bool::from(got.ct_eq(&pair_mac(secret, body)[..])) {
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
    let (mut key, service_uuid, wake_uuid, address_uuid) = derive(secret, &mac_pub, &phone_pub);
    let pairing_key_b64 = B64.encode(key);
    key.zeroize();
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
        platform: req.platform.trim().chars().take(32).collect(),
        ed25519_pub_b64: req.ed25519_pub_b64,
        tls_cert_der_b64: req.tls_cert_der_b64,
        pairing_key_b64,
        service_uuid,
        wake_uuid,
        address_uuid,
        paired_at_unix: now,
        display_name: None,
        unknown: Default::default(),
    };
    Ok((paired, reply_body, sig_b64))
}

/// Removes the phone. False when no phone has that id.
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

    const PHONE_ID: &str = "4a68f72f5a22110b2bfbe71b6588833c";
    const CERT: [u8; 91] = [7u8; 91];

    fn mac() -> SyncIdentity {
        SyncIdentity::mint().unwrap()
    }

    struct Phone {
        key: SigningKey,
        eph: x25519_dalek::StaticSecret,
        nonce: [u8; 32],
    }

    fn phone() -> Phone {
        let mut seed = [0u8; 32];
        rand::rng().fill_bytes(&mut seed);
        let mut nonce = [0u8; 32];
        rand::rng().fill_bytes(&mut nonce);
        let mut eph = [0u8; 32];
        rand::rng().fill_bytes(&mut eph);
        Phone { key: SigningKey::from_bytes(&seed), eph: x25519_dalek::StaticSecret::from(eph), nonce }
    }

    impl Phone {
        fn claim(&self) -> PhoneClaim {
            PhoneClaim {
                device_id: device_id_bytes(PHONE_ID).unwrap(),
                ed25519_pub: self.key.verifying_key().to_bytes(),
                cert_sha256: Sha256::digest(CERT).into(),
            }
        }
        fn eph_pub(&self) -> [u8; 32] {
            x25519_dalek::PublicKey::from(&self.eph).to_bytes()
        }
        /// The `/pair/start` body, proving `key`.
        fn start_body(&self, key: &[u8; 10]) -> Vec<u8> {
            self.start_body_with(key, self.eph_pub())
        }
        fn start_body_with(&self, key: &[u8; 10], eph_pub: [u8; 32]) -> Vec<u8> {
            serde_json::to_vec(&serde_json::json!({
                "v": 2,
                "device_id": PHONE_ID,
                "ed25519_pub_b64": B64.encode(self.key.verifying_key().to_bytes()),
                "tls_cert_der_b64": B64.encode(CERT),
                "eph_pub_b64": B64.encode(eph_pub),
                "nonce_b64": B64.encode(self.nonce),
                "cP_b64": B64.encode(phone_proof(key, &self.claim().bytes(), &eph_pub, &self.nonce)),
            }))
            .unwrap()
        }
        /// Checks the reply the way a phone does and returns `(session_id, S)`.
        fn finish(&self, key: &[u8; 10], reply: &[u8]) -> Result<(String, [u8; 32]), &'static str> {
            let r: serde_json::Value = serde_json::from_slice(reply).unwrap();
            let arr = |f: &str| -> [u8; 32] { B64.decode(r[f].as_str().unwrap()).unwrap().try_into().unwrap() };
            let b = desktop_claim(
                &device_id_bytes(r["device_id"].as_str().unwrap()).unwrap(),
                &arr("ed25519_pub_b64"),
                &arr("x25519_pub_b64"),
                &arr("tls_spki_sha256_b64"),
                r["name"].as_str().unwrap(),
                r["host"].as_str().unwrap(),
                r["port"].as_u64().unwrap() as u16,
            );
            let eph_d = arr("eph_pub_b64");
            let t = transcript(&self.claim().bytes(), &b, &self.eph_pub(), &eph_d, &self.nonce, &arr("nonce_b64"));
            let z = self.eph.diffie_hellman(&x25519_dalek::PublicKey::from(eph_d)).to_bytes();
            let (cd, s) = confirm(key, &z, &t);
            if !bool::from(cd.ct_eq(&arr("cD_b64"))) {
                return Err("cD");
            }
            Ok((r["session_id"].as_str().unwrap().to_string(), s))
        }
        /// The v1 `/pair` body and its MAC under S.
        fn pair_request(&self, secret: &[u8; 32]) -> (Vec<u8>, String) {
            let body = serde_json::to_vec(&serde_json::json!({
                "v": 1,
                "device_id": PHONE_ID,
                "name": "Rowan's phone",
                "platform": "ios",
                "ed25519_pub_b64": B64.encode(self.key.verifying_key().to_bytes()),
                "tls_cert_der_b64": B64.encode(CERT),
            }))
            .unwrap();
            let mac = B64.encode(pair_mac(secret, &body));
            (body, mac)
        }
    }

    fn key_at(now: i64) -> PairingKey {
        PairingKey::mint("studio-mac.local".into(), now)
    }

    fn do_start(id: &SyncIdentity, pk: &mut PairingKey, body: &[u8]) -> Result<Vec<u8>, PairError> {
        start(id, pk, 1010, body, Some(&CERT), "Studio Mac", 7845, StartRandom::fresh())
    }

    #[test]
    fn key_text_round_trips_and_parses_what_people_type() {
        let key = [0x39, 0x45, 0x3b, 0xab, 0x1a, 0x23, 0xd0, 0x6b, 0xb5, 0xd9];
        let text = key_text(&key);
        assert_eq!(text.len(), 16);
        assert_eq!(parse_key(&text), Some(key));
        let typed = format!("{}-{} {}-{}", &text[..4], &text[4..8], &text[8..12], &text[12..]).to_lowercase();
        assert_eq!(parse_key(&typed), Some(key));
        assert_eq!(parse_key("0000000000000000"), Some([0; 10]));
        assert_eq!(parse_key("oOoOiIlL00000000"), parse_key("0000111100000000"));
        assert_eq!(parse_key("ZZZZZZZZZZZZZZZZ"), Some([0xff; 10]));
        assert_eq!(parse_key("U000000000000000"), None);
        assert_eq!(parse_key("000000000000000"), None, "15 characters");
        assert_eq!(parse_key("00000000000000000"), None, "17 characters");
        assert_eq!(parse_key("00000000_0000000"), None);
    }

    #[test]
    fn the_qr_carries_the_canonical_key_and_where_to_find_it() {
        let key = [0u8; 10];
        assert_eq!(qr_text(&key, "studio-mac.local", 7845), "daisy-pair:2:0000000000000000@studio-mac.local:7845");
    }

    #[test]
    fn the_tag_is_two_characters_of_the_keys_hash() {
        let tag = routing_tag(&[1u8; 10]);
        assert_eq!(tag.len(), 2);
        let d = Sha256::new().chain_update(b"daisy-pair-tag").chain_update([1u8; 10]).finalize();
        let first10 = ((d[0] as usize) << 2) | (d[1] as usize >> 6);
        assert_eq!(tag.as_bytes(), [CROCKFORD[first10 >> 5], CROCKFORD[first10 & 31]]);
    }

    #[test]
    fn a_key_holder_pairs_and_both_sides_derive_the_same_keys() {
        let id = mac();
        let mut pk = key_at(1000);
        let key = pk.key;
        let p = phone();
        let reply = do_start(&id, &mut pk, &p.start_body(&key)).unwrap();
        let (session, s) = p.finish(&key, &reply).unwrap();
        let r: serde_json::Value = serde_json::from_slice(&reply).unwrap();
        assert_eq!(r["host"], "studio-mac.local");
        assert_eq!(r["tls_spki_sha256_b64"], id.tls_spki_sha256_b64());

        let (body, mac_b64) = p.pair_request(&s);
        let secret = session_secret(&pk, 1020, &session, &body, Some(&CERT)).unwrap();
        assert_eq!(secret, s);
        let (paired, reply, sig_b64) = accept(&id, &secret, 1020, &body, &mac_b64, "Studio Mac", 7845).unwrap();
        let sig = ed25519_dalek::Signature::from_bytes(&B64.decode(&sig_b64).unwrap().try_into().unwrap());
        id.signing_key().verifying_key().verify(&reply, &sig).unwrap();
        let (k, service, wake, addr) = derive(&s, &id.signing_key().verifying_key().to_bytes(), &p.key.verifying_key().to_bytes());
        assert_eq!(paired.pairing_key_b64, B64.encode(k));
        assert_eq!((&paired.service_uuid, &paired.wake_uuid, &paired.address_uuid), (&service, &wake, &addr));
    }

    #[test]
    fn a_wrong_key_is_refused_and_leaves_nothing_behind() {
        let id = mac();
        let mut pk = key_at(1000);
        let key = pk.key;
        let p = phone();
        let mut wrong = key;
        wrong[9] ^= 1;
        for _ in 0..10 {
            assert_eq!(do_start(&id, &mut pk, &p.start_body(&wrong)).unwrap_err(), PairError::BadKey);
        }
        assert!(pk.sessions.is_empty());
        assert!(do_start(&id, &mut pk, &p.start_body(&key)).is_ok(), "the key is still live");
    }

    #[test]
    fn a_body_certificate_other_than_the_connections_is_refused() {
        let id = mac();
        let mut pk = key_at(1000);
        let key = pk.key;
        let body = phone().start_body(&key);
        let other = [8u8; 91];
        assert_eq!(start(&id, &mut pk, 1010, &body, Some(&other), "n", 7845, StartRandom::fresh()).unwrap_err(), PairError::CertMismatch);
        assert_eq!(start(&id, &mut pk, 1010, &body, None, "n", 7845, StartRandom::fresh()).unwrap_err(), PairError::CertMismatch);
    }

    #[test]
    fn low_order_points_are_refused() {
        let id = mac();
        let mut pk = key_at(1000);
        let key = pk.key;
        let p = phone();
        let mut one = [0u8; 32];
        one[0] = 1;
        for point in [[0u8; 32], one] {
            assert_eq!(do_start(&id, &mut pk, &p.start_body_with(&key, point)).unwrap_err(), PairError::BadPoint);
        }
        assert!(pk.sessions.is_empty());
    }

    #[test]
    fn an_expired_key_is_refused_on_both_routes() {
        let id = mac();
        let mut pk = key_at(1000);
        let key = pk.key;
        let p = phone();
        let reply = do_start(&id, &mut pk, &p.start_body(&key)).unwrap();
        let (session, s) = p.finish(&key, &reply).unwrap();
        let body = p.start_body(&key);
        assert_eq!(start(&id, &mut pk, 1600, &body, Some(&CERT), "n", 7845, StartRandom::fresh()).unwrap_err(), PairError::Expired);
        let (pair_body, _) = p.pair_request(&s);
        assert_eq!(session_secret(&pk, 1600, &session, &pair_body, Some(&CERT)).unwrap_err(), PairError::Expired);
        assert!(session_secret(&pk, 1599, &session, &pair_body, Some(&CERT)).is_ok());
    }

    #[test]
    fn pair_must_come_from_the_phone_that_started_the_session() {
        let id = mac();
        let mut pk = key_at(1000);
        let key = pk.key;
        let p = phone();
        let (session, s) = p.finish(&key, &do_start(&id, &mut pk, &p.start_body(&key)).unwrap()).unwrap();
        let (body, _) = p.pair_request(&s);
        assert_eq!(session_secret(&pk, 1010, "AAAAAAAAAAAAAAAAAAAAAA==", &body, Some(&CERT)).unwrap_err(), PairError::BadRequest);
        assert_eq!(session_secret(&pk, 1010, &session, &body, Some(&[8u8; 91])).unwrap_err(), PairError::CertMismatch);
        let (other_body, _) = phone().pair_request(&s);
        assert_eq!(session_secret(&pk, 1010, &session, &other_body, Some(&CERT)).unwrap_err(), PairError::CertMismatch);
    }

    #[test]
    fn a_bad_mac_ends_its_session_only() {
        let id = mac();
        let mut pk = key_at(1000);
        let key = pk.key;
        let p = phone();
        let (first, _) = p.finish(&key, &do_start(&id, &mut pk, &p.start_body(&key)).unwrap()).unwrap();
        let (second, s2) = p.finish(&key, &do_start(&id, &mut pk, &p.start_body(&key)).unwrap()).unwrap();
        let (body, _) = p.pair_request(&s2);
        let wrong = B64.encode(pair_mac(&[9u8; 32], &body));
        let secret = session_secret(&pk, 1010, &first, &body, Some(&CERT)).unwrap();
        assert_eq!(accept(&id, &secret, 1010, &body, &wrong, "n", 7845).unwrap_err(), PairError::BadMac);
        end_session(&mut pk, &first);
        assert_eq!(session_secret(&pk, 1010, &first, &body, Some(&CERT)).unwrap_err(), PairError::BadRequest);
        assert_eq!(session_secret(&pk, 1010, &second, &body, Some(&CERT)).unwrap(), s2);
    }

    #[test]
    fn a_fifth_start_replaces_the_oldest_session() {
        let id = mac();
        let mut pk = key_at(1000);
        let key = pk.key;
        let p = phone();
        let ids: Vec<String> =
            (0..5).map(|_| p.finish(&key, &do_start(&id, &mut pk, &p.start_body(&key)).unwrap()).unwrap().0).collect();
        assert_eq!(pk.sessions.len(), 4);
        assert!(pk.sessions.iter().all(|s| s.id_b64 != ids[0]));
        assert_eq!(pk.sessions.last().unwrap().id_b64, ids[4]);
    }

    #[test]
    fn a_malformed_start_is_refused() {
        let id = mac();
        let mut pk = key_at(1000);
        let key = pk.key;
        let p = phone();
        let good: serde_json::Value = serde_json::from_slice(&p.start_body(&key)).unwrap();
        for (field, value) in [
            ("v", serde_json::json!(1)),
            ("device_id", serde_json::json!("phone-1")),
            ("device_id", serde_json::json!(PHONE_ID.to_uppercase())),
            ("eph_pub_b64", serde_json::json!("AAAA")),
            ("nonce_b64", serde_json::json!("not base64")),
        ] {
            let mut bad = good.clone();
            bad[field] = value;
            assert_eq!(do_start(&id, &mut pk, &serde_json::to_vec(&bad).unwrap()).unwrap_err(), PairError::BadRequest, "{field}");
        }
    }

    #[test]
    fn an_overlong_name_and_platform_are_kept_short() {
        let id = mac();
        let p = phone();
        let body = serde_json::to_vec(&serde_json::json!({
            "v": 1,
            "device_id": PHONE_ID,
            "name": "n".repeat(10_000),
            "platform": "p".repeat(10_000),
            "ed25519_pub_b64": B64.encode(p.key.verifying_key().to_bytes()),
            "tls_cert_der_b64": B64.encode(CERT),
        }))
        .unwrap();
        let s = [3u8; 32];
        let mac_b64 = B64.encode(pair_mac(&s, &body));
        let (paired, _, _) = accept(&id, &s, 1010, &body, &mac_b64, "n", 7845).unwrap();
        assert_eq!(paired.name.chars().count(), 80);
        assert_eq!(paired.platform.chars().count(), 32);
    }

    #[test]
    fn a_long_desktop_name_is_cut_to_255_bytes_and_both_sides_still_agree() {
        let long = format!("a{}", "日".repeat(100));
        assert_eq!(long.len(), 301);
        let id = mac();
        let mut pk = key_at(1000);
        let key = pk.key;
        let p = phone();
        let reply = start(&id, &mut pk, 1010, &p.start_body(&key), Some(&CERT), &long, 7845, StartRandom::fresh()).unwrap();
        let r: serde_json::Value = serde_json::from_slice(&reply).unwrap();
        let sent = r["name"].as_str().unwrap();
        assert!(sent.len() <= 255, "{} bytes", sent.len());
        assert_eq!(sent.len(), 253, "cut at the last whole character");
        assert!(long.starts_with(sent));
        let (session, s) = p.finish(&key, &reply).expect("B and T rebuilt from the reply verify cD");

        let (body, mac_b64) = p.pair_request(&s);
        let secret = session_secret(&pk, 1020, &session, &body, Some(&CERT)).unwrap();
        let (_, pair_reply, _) = accept(&id, &secret, 1020, &body, &mac_b64, &long, 7845).unwrap();
        let pr: serde_json::Value = serde_json::from_slice(&pair_reply).unwrap();
        assert_eq!(pr["name"].as_str().unwrap(), sent, "/pair names the desktop as /pair/start did");
    }

    #[test]
    fn the_three_uuids_differ() {
        let (_, s, w, a) = derive(&[1u8; 32], &[2u8; 32], &[3u8; 32]);
        assert!(s != w && w != a && s != a);
    }

    #[test]
    fn an_unpaired_phone_can_pair_again() {
        let mut id = mac();
        let p = phone();
        let mut pk = key_at(1000);
        let key = pk.key;
        let (_, s) = p.finish(&key, &do_start(&id, &mut pk, &p.start_body(&key)).unwrap()).unwrap();
        let (body, mac_b64) = p.pair_request(&s);
        id.paired.push(accept(&id, &s, 1010, &body, &mac_b64, "n", 7845).unwrap().0);
        assert!(unpair(&mut id, PHONE_ID));
        assert!(id.paired.is_empty());
        assert!(!unpair(&mut id, PHONE_ID), "unknown id changes nothing");
    }
}
