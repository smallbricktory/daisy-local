// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Test vectors for the phone sync protocol, shared with smbr-daisy-ios.
//! `tests/fixtures/sync-vectors-v1.json` is byte-identical to the copy under
//! `ios/DaisyKit/Tests/DaisyKitTests/Fixtures/` there, and both sides assert
//! against it: every key, MAC, signature, UUID and body that section 1-4, 8
//! and 9 of `docs/sync/protocol.md` define, from one fixed secret and two
//! fixed key pairs.
//!
//! `write_vectors` (ignored) rebuilds the file from the primitives named in
//! protocol.md, spelled here without the app's own derivation code, so the
//! app is checked against the contract and never against itself. TLS key
//! material is minted once and carried over between regenerations.
//!
//! ```sh
//! cargo test -p tauri-app --test sync_vectors -- --ignored write_vectors
//! ```
use aes_gcm::aead::Aead;
use aes_gcm::{Aes256Gcm, KeyInit as GcmKeyInit, Nonce};
use base64::engine::general_purpose::{STANDARD as B64, URL_SAFE_NO_PAD as B64URL};
use base64::Engine;
use ed25519_dalek::{Signer, SigningKey, Verifier, VerifyingKey};
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri_app_core::sync::identity::{SyncIdentity, SCHEMA};
use tauri_app_core::sync::pairing::{accept, pair_code_text, pair_mac, PairingWindow};
use tauri_app_core::sync::signing;
use tauri_app_core::sync::wake;

const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/sync-vectors-v1.json");
const SALT: &[u8] = b"daisy-pairing-v1";
const NOW: i64 = 1_787_300_000;
const PORT: u16 = 7845;

#[derive(Serialize, Deserialize)]
struct Vectors {
    #[serde(rename = "_note")]
    note: String,
    secret_b64: String,
    desktop: Desktop,
    phone: Phone,
    derived: Derived,
    pairing_code: PairingCode,
    pair_request: PairRequest,
    pair_reply: PairReply,
    signed_requests: Vec<SignedRequest>,
    signed_reply: SignedReply,
    tombstone: Tombstone,
    wake_payload: Payload,
    address_payload: Payload,
    envelope_v2: EnvelopeSample,
}

#[derive(Serialize, Deserialize)]
struct Desktop {
    device_id: String,
    name: String,
    ed25519_seed_b64: String,
    ed25519_pub_b64: String,
    x25519_secret_b64: String,
    x25519_pub_b64: String,
    tls_key_der_b64: String,
    tls_cert_der_b64: String,
    tls_spki_sha256_b64: String,
}

#[derive(Serialize, Deserialize)]
struct Phone {
    device_id: String,
    name: String,
    vault_key_b64: String,
    ed25519_seed_b64: String,
    ed25519_pub_b64: String,
    tls_key_der_b64: String,
    tls_cert_der_b64: String,
    tls_spki_sha256_b64: String,
}

#[derive(Serialize, Deserialize)]
struct Derived {
    pair_mac_key_b64: String,
    pairing_key_b64: String,
    service_uuid: String,
    wake_uuid: String,
    address_uuid: String,
}

#[derive(Serialize, Deserialize)]
struct PairingCode {
    host: String,
    port: u16,
    json: String,
    text: String,
}

#[derive(Serialize, Deserialize)]
struct PairRequest {
    body: String,
    mac_b64: String,
}

#[derive(Serialize, Deserialize)]
struct PairReply {
    body: String,
    signature_b64: String,
}

#[derive(Serialize, Deserialize)]
struct SignedRequest {
    method: String,
    path: String,
    nonce_b64: String,
    body: String,
    body_sha256: String,
    signing_string: String,
    signature_b64: String,
}

#[derive(Serialize, Deserialize)]
struct SignedReply {
    status: u16,
    path: String,
    nonce_b64: String,
    body: String,
    signing_string: String,
    signature_b64: String,
}

#[derive(Serialize, Deserialize)]
struct Tombstone {
    session: String,
    seq: u64,
    origin: String,
    signing_string: String,
    sig_b64: String,
    body: String,
}

/// One file the phone sealed while locked: readable by the phone's vault key
/// and by the desktop's X25519 key, and by nothing else.
#[derive(Serialize, Deserialize)]
struct EnvelopeSample {
    plaintext: String,
    data_key_b64: String,
    ephemeral_secret_b64: String,
    envelope: String,
}

#[derive(Serialize, Deserialize)]
struct Payload {
    plaintext: String,
    nonce_b64: String,
    wire_b64: String,
}

fn vectors() -> Vectors {
    serde_json::from_str(include_str!("fixtures/sync-vectors-v1.json")).expect("fixture parses")
}

fn b64(s: &str) -> Vec<u8> {
    B64.decode(s).expect("base64")
}

fn arr<const N: usize>(s: &str) -> [u8; N] {
    b64(s).try_into().expect("length")
}

fn identity(v: &Vectors) -> SyncIdentity {
    SyncIdentity {
        schema_version: SCHEMA,
        device_id: v.desktop.device_id.clone(),
        ed25519_seed_b64: v.desktop.ed25519_seed_b64.clone(),
        x25519_secret_b64: v.desktop.x25519_secret_b64.clone(),
        tls_key_der_b64: v.desktop.tls_key_der_b64.clone(),
        tls_cert_der_b64: v.desktop.tls_cert_der_b64.clone(),
        paired: Vec::new(),
    }
}

fn window(v: &Vectors) -> PairingWindow {
    PairingWindow { secret: arr(&v.secret_b64), expires_unix: NOW + 60 }
}

#[test]
fn identity_keys_derive_from_the_fixture_seeds() {
    let v = vectors();
    let id = identity(&v);
    assert_eq!(id.ed25519_pub_b64(), v.desktop.ed25519_pub_b64);
    assert_eq!(id.x25519_pub_b64(), v.desktop.x25519_pub_b64);
    assert_eq!(id.tls_spki_sha256_b64(), v.desktop.tls_spki_sha256_b64);
}

#[test]
fn the_pairing_code_is_the_fixture_text_byte_for_byte() {
    let v = vectors();
    let text = pair_code_text(&identity(&v), &window(&v), &v.desktop.name, v.pairing_code.host.clone(), v.pairing_code.port).unwrap();
    assert_eq!(text, v.pairing_code.text);
    let json = B64URL.decode(text.strip_prefix("daisy-pair:1:").unwrap()).unwrap();
    assert_eq!(json, v.pairing_code.json.as_bytes());
}

#[test]
fn the_pair_mac_matches() {
    let v = vectors();
    assert_eq!(B64.encode(pair_mac(&arr(&v.secret_b64), v.pair_request.body.as_bytes())), v.pair_request.mac_b64);
}

#[test]
fn accepting_the_fixture_request_derives_the_fixture_keys_and_signs_the_fixture_reply() {
    let v = vectors();
    let (paired, reply, sig_b64) = accept(
        &identity(&v),
        &window(&v),
        NOW,
        v.pair_request.body.as_bytes(),
        &v.pair_request.mac_b64,
        &v.desktop.name,
        PORT,
    )
    .expect("the fixture request is well-formed under the fixture secret");
    assert_eq!(paired.device_id, v.phone.device_id);
    assert_eq!(paired.name, v.phone.name);
    assert_eq!(paired.ed25519_pub_b64, v.phone.ed25519_pub_b64);
    assert_eq!(paired.tls_cert_der_b64, v.phone.tls_cert_der_b64);
    assert_eq!(paired.pairing_key_b64, v.derived.pairing_key_b64);
    assert_eq!(paired.service_uuid, v.derived.service_uuid);
    assert_eq!(paired.wake_uuid, v.derived.wake_uuid);
    assert_eq!(paired.address_uuid, v.derived.address_uuid);
    assert_eq!(reply, v.pair_reply.body.as_bytes(), "reply bytes");
    assert_eq!(sig_b64, v.pair_reply.signature_b64, "Ed25519 is deterministic: same bytes, same signature");
}

#[test]
fn every_fixture_signature_verifies_under_its_public_key() {
    let v = vectors();
    let mac_pub = VerifyingKey::from_bytes(&arr(&v.desktop.ed25519_pub_b64)).unwrap();
    let phone_pub = VerifyingKey::from_bytes(&arr(&v.phone.ed25519_pub_b64)).unwrap();
    let sig = |s: &str| ed25519_dalek::Signature::from_bytes(&arr(s));
    mac_pub.verify(v.pair_reply.body.as_bytes(), &sig(&v.pair_reply.signature_b64)).unwrap();
    for r in &v.signed_requests {
        assert_eq!(r.body_sha256, signing::body_hash(r.body.as_bytes()));
        let rebuilt = signing::signing_string(&r.method, &r.path, &v.phone.device_id, &r.nonce_b64, r.body.as_bytes());
        assert_eq!(rebuilt, r.signing_string);
        assert!(
            signing::verify_request(&v.phone.ed25519_pub_b64, &r.signature_b64, &rebuilt),
            "the desktop accepts what the phone signed"
        );
    }
    let r = &v.signed_reply;
    mac_pub.verify(r.signing_string.as_bytes(), &sig(&r.signature_b64)).unwrap();
    assert_eq!(r.signing_string, signing::reply_signing_string(r.status, &r.path, &r.nonce_b64, r.body.as_bytes()));
    let t = &v.tombstone;
    phone_pub.verify(t.signing_string.as_bytes(), &sig(&t.sig_b64)).unwrap();
    assert_eq!(t.signing_string, format!("tombstone\n{}\n{}", t.session, t.seq));
}

#[test]
fn the_gatt_payloads_open_under_the_pairing_key() {
    let v = vectors();
    let key: [u8; 32] = arr(&v.derived.pairing_key_b64);
    for p in [&v.wake_payload, &v.address_payload] {
        let wire = b64(&p.wire_b64);
        assert_eq!(&wire[..12], &b64(&p.nonce_b64)[..]);
        assert_eq!(wake::open(&key, &wire).expect("the pairing key opens it"), p.plaintext.as_bytes());
    }
}

/// The payloads this desktop puts on the air are the shape the fixture
/// records, which is the shape the phone decodes. Only the random nonce and
/// the wake's own nonce differ from the stored bytes.
#[test]
fn the_payloads_this_desktop_seals_match_the_fixture_shape() {
    let v = vectors();
    let key: [u8; 32] = arr(&v.derived.pairing_key_b64);

    let mine = wake::wake_payload(&key).unwrap();
    let opened: serde_json::Value = serde_json::from_slice(&wake::open(&key, &mine).unwrap()).unwrap();
    let stored: serde_json::Value = serde_json::from_slice(v.wake_payload.plaintext.as_bytes()).unwrap();
    assert_eq!(opened["t"], stored["t"]);
    assert_eq!(
        B64.decode(opened["n"].as_str().unwrap()).unwrap().len(),
        B64.decode(stored["n"].as_str().unwrap()).unwrap().len()
    );

    let stored: serde_json::Value = serde_json::from_slice(v.address_payload.plaintext.as_bytes()).unwrap();
    let mine = wake::address_payload(&key, stored["host"].as_str().unwrap(), stored["port"].as_u64().unwrap() as u16)
        .unwrap();
    assert_eq!(wake::open(&key, &mine).unwrap(), v.address_payload.plaintext.as_bytes());
}

#[test]
fn a_file_the_phone_sealed_opens_with_this_machines_x25519_key() {
    let v = vectors();
    let secret: [u8; 32] = arr(&v.desktop.x25519_secret_b64);
    let opened = vault::envelope_v2::open_v2(v.envelope_v2.envelope.as_bytes(), &secret, &v.desktop.device_id)
        .expect("the desktop is a recipient");
    assert_eq!(&*opened, v.envelope_v2.plaintext.as_bytes());
    assert!(vault::envelope_v2::is_v2(v.envelope_v2.envelope.as_bytes()));

    // Another machine's key opens nothing, and neither does another id.
    assert!(vault::envelope_v2::open_v2(v.envelope_v2.envelope.as_bytes(), &[3u8; 32], &v.desktop.device_id).is_err());
    assert!(vault::envelope_v2::open_v2(v.envelope_v2.envelope.as_bytes(), &secret, &v.phone.device_id).is_err());
}

// ---- generator ----------------------------------------------------------

fn sha256_label(bytes: &[u8]) -> String {
    format!("sha256:{}", hex(&Sha256::digest(bytes)))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A fixed, recognisable input: SHA-256 of a label under the fixture's name.
fn fixed(label: &str) -> [u8; 32] {
    Sha256::digest(format!("daisy-sync-vectors-v1:{label}")).into()
}

fn hkdf(secret: &[u8], info: &[u8], len: usize) -> Vec<u8> {
    let mut out = vec![0u8; len];
    Hkdf::<Sha256>::new(Some(SALT), secret).expand(info, &mut out).unwrap();
    out
}

/// Section 2's `uuid(b)`: version-4 bits, then the standard uppercase text.
fn uuid(b: &[u8]) -> String {
    let mut b: [u8; 16] = b.try_into().unwrap();
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h = hex(&b).to_uppercase();
    format!("{}-{}-{}-{}-{}", &h[..8], &h[8..12], &h[12..16], &h[16..20], &h[20..])
}

/// A JSON object with its fields in the order protocol.md lists them.
fn obj(fields: &[(&str, serde_json::Value)]) -> String {
    let inner: Vec<String> = fields.iter().map(|(k, v)| format!("{}:{}", serde_json::to_string(k).unwrap(), v)).collect();
    format!("{{{}}}", inner.join(","))
}

fn s(v: &str) -> serde_json::Value {
    serde_json::Value::String(v.to_string())
}

fn seal(key: &[u8], nonce: &[u8; 12], plaintext: &str) -> Payload {
    let cipher = <Aes256Gcm as GcmKeyInit>::new_from_slice(key).unwrap();
    let boxed = cipher.encrypt(Nonce::from_slice(nonce), plaintext.as_bytes()).unwrap();
    Payload {
        plaintext: plaintext.to_string(),
        nonce_b64: B64.encode(nonce),
        wire_b64: B64.encode([&nonce[..], &boxed].concat()),
    }
}

/// `(key PKCS#8 DER, certificate DER, SPKI SHA-256)`, kept from the existing
/// fixture when there is one so the file stays stable across regenerations.
fn tls(existing: Option<(&str, &str)>, cn: &str) -> (String, String, String) {
    let (key_der, cert_der) = match existing {
        Some((k, c)) => (b64(k), b64(c)),
        None => {
            let key = rcgen::KeyPair::generate().unwrap();
            let mut params = rcgen::CertificateParams::new(vec![cn.to_string()]).unwrap();
            params.not_after = rcgen::date_time_ymd(2099, 12, 31);
            let cert = params.self_signed(&key).unwrap();
            (key.serialize_der(), cert.der().to_vec())
        }
    };
    let pair = rcgen::KeyPair::try_from(key_der.as_slice()).unwrap();
    (B64.encode(&key_der), B64.encode(&cert_der), B64.encode(Sha256::digest(pair.public_key_der())))
}

#[test]
#[ignore = "rewrites tests/fixtures/sync-vectors-v1.json; run by hand"]
fn write_vectors() {
    let existing: Option<Vectors> = std::fs::read_to_string(FIXTURE).ok().and_then(|s| serde_json::from_str(&s).ok());

    let secret = fixed("secret");
    let mac_seed = fixed("desktop-ed25519-seed");
    let mac_key = SigningKey::from_bytes(&mac_seed);
    let mac_pub = mac_key.verifying_key().to_bytes();
    let x_secret = x25519_dalek::StaticSecret::from(fixed("desktop-x25519-secret"));
    let x_pub = x25519_dalek::PublicKey::from(&x_secret).to_bytes();
    let phone_seed = fixed("phone-ed25519-seed");
    let phone_key = SigningKey::from_bytes(&phone_seed);
    let phone_pub = phone_key.verifying_key().to_bytes();
    let mac_id = hex(&fixed("desktop-device-id")[..16]);
    let phone_id = hex(&fixed("phone-device-id")[..16]);
    let mac_name = "Studio Mac";
    let phone_name = "Rowan's phone";

    let (mac_tls_key, mac_tls_cert, mac_spki) = tls(
        existing.as_ref().map(|e| (e.desktop.tls_key_der_b64.as_str(), e.desktop.tls_cert_der_b64.as_str())),
        &format!("daisy-{mac_id}"),
    );
    let (phone_tls_key, phone_tls_cert, phone_spki) = tls(
        existing.as_ref().map(|e| (e.phone.tls_key_der_b64.as_str(), e.phone.tls_cert_der_b64.as_str())),
        &format!("daisy-{phone_id}"),
    );

    // Section 2.
    let both = [&mac_pub[..], &phone_pub[..]].concat();
    let info = |label: &[u8]| [label, &both].concat();
    let pair_mac_key = hkdf(&secret, b"pair-mac", 32);
    let pairing_key = hkdf(&secret, &info(b"payload-key"), 32);
    let derived = Derived {
        pair_mac_key_b64: B64.encode(&pair_mac_key),
        pairing_key_b64: B64.encode(&pairing_key),
        service_uuid: uuid(&hkdf(&secret, &info(b"service-uuid"), 16)),
        wake_uuid: uuid(&hkdf(&secret, &info(b"char-wake"), 16)),
        address_uuid: uuid(&hkdf(&secret, &info(b"char-address"), 16)),
    };

    // Section 1.
    let host = "studio-mac.local";
    let code_json = obj(&[
        ("v", 1.into()),
        ("device_id", s(&mac_id)),
        ("name", s(mac_name)),
        ("ed25519_pub_b64", s(&B64.encode(mac_pub))),
        ("tls_spki_sha256_b64", s(&mac_spki)),
        ("secret_b64", s(&B64.encode(secret))),
        ("host", s(host)),
        ("port", PORT.into()),
    ]);
    let pairing_code = PairingCode {
        host: host.to_string(),
        port: PORT,
        text: format!("daisy-pair:1:{}", B64URL.encode(&code_json)),
        json: code_json,
    };

    // Section 3.
    let request_body = obj(&[
        ("v", 1.into()),
        ("device_id", s(&phone_id)),
        ("name", s(phone_name)),
        ("platform", s("ios")),
        ("ed25519_pub_b64", s(&B64.encode(phone_pub))),
        ("tls_cert_der_b64", s(&phone_tls_cert)),
    ]);
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(&pair_mac_key).unwrap();
    mac.update(request_body.as_bytes());
    let pair_request = PairRequest { mac_b64: B64.encode(mac.finalize().into_bytes()), body: request_body };
    let reply_body = obj(&[
        ("v", 1.into()),
        ("device_id", s(&mac_id)),
        ("name", s(mac_name)),
        ("x25519_pub_b64", s(&B64.encode(x_pub))),
        ("phone_ed25519_pub_b64", s(&B64.encode(phone_pub))),
        ("port", PORT.into()),
    ]);
    let pair_reply = PairReply {
        signature_b64: B64.encode(mac_key.sign(reply_body.as_bytes()).to_bytes()),
        body: reply_body,
    };

    // Section 4: one request with a body, one with none; one signed reply.
    let session = "daisy-1787249858";
    let artifact_id = sha256_label(b"summary.json bytes");
    let signed_request = |method: &str, path: &str, nonce_label: &str, body: String| {
        let nonce_b64 = B64.encode(&fixed(nonce_label)[..16]);
        let body_sha256 = sha256_label(body.as_bytes());
        let signing_string = [method, path, &phone_id, &nonce_b64, &body_sha256].join("\n");
        SignedRequest {
            signature_b64: B64.encode(phone_key.sign(signing_string.as_bytes()).to_bytes()),
            method: method.to_string(),
            path: path.to_string(),
            nonce_b64,
            body,
            body_sha256,
            signing_string,
        }
    };
    let hello_body = obj(&[
        ("v", 1.into()),
        ("device_id", s(&phone_id)),
        (
            "sessions",
            serde_json::json!([{
                "id": session,
                "inputs": {
                    "manifest": sha256_label(b"manifest.json bytes"),
                    "transcript": sha256_label(b"transcript.json bytes"),
                },
                "artifacts": [artifact_id],
            }]),
        ),
        ("tombstones", serde_json::json!([])),
    ]);
    let signed_requests = vec![
        signed_request("POST", "/hello", "hello-nonce", hello_body),
        signed_request("GET", &format!("/artifact/{artifact_id}"), "artifact-nonce", String::new()),
    ];
    let reply_body = obj(&[
        ("v", 1.into()),
        ("wants", serde_json::json!([{ "session": session, "file": "manifest.json" }])),
        ("offers", serde_json::json!([])),
        ("tombstones", serde_json::json!([])),
    ]);
    let nonce_b64 = signed_requests[0].nonce_b64.clone();
    let signing_string = ["200", "/hello", &nonce_b64, &sha256_label(reply_body.as_bytes())].join("\n");
    let signed_reply = SignedReply {
        status: 200,
        path: "/hello".into(),
        nonce_b64,
        body: reply_body,
        signature_b64: B64.encode(mac_key.sign(signing_string.as_bytes()).to_bytes()),
        signing_string,
    };

    // Section 8.
    let seq = 3u64;
    let signing_string = format!("tombstone\n{session}\n{seq}");
    let sig_b64 = B64.encode(phone_key.sign(signing_string.as_bytes()).to_bytes());
    let tombstone = Tombstone {
        body: obj(&[
            ("v", 1.into()),
            ("session", s(session)),
            ("seq", seq.into()),
            ("origin", s(&phone_id)),
            ("sig_b64", s(&sig_b64)),
        ]),
        session: session.to_string(),
        seq,
        origin: phone_id.clone(),
        signing_string,
        sig_b64,
    };

    // Section 9.
    let wake_n = B64.encode(&fixed("wake-n")[..16]);
    let wake_payload = seal(&pairing_key, &fixed("wake-nonce")[..12].try_into().unwrap(), &obj(&[("t", s("wake")), ("n", s(&wake_n))]));
    let address_payload = seal(
        &pairing_key,
        &fixed("address-nonce")[..12].try_into().unwrap(),
        &obj(&[("t", s("addr")), ("host", s("192.168.1.20")), ("port", PORT.into())]),
    );

    // Section 10: a random data key seals the file, wrapped to the phone's
    // own vault key and to this desktop's X25519 key.
    let data_key = fixed("envelope-data-key");
    let phone_vault_key = fixed("phone-vault-key");
    let envelope_plaintext = "{\"schema_version\":4,\"language\":\"fr-CA\"}";
    let ephemeral_secret = fixed("envelope-ephemeral-secret");
    let ephemeral = x25519_dalek::StaticSecret::from(ephemeral_secret);
    let ephemeral_pub = x25519_dalek::PublicKey::from(&ephemeral).to_bytes();
    let shared = ephemeral.diffie_hellman(&x25519_dalek::PublicKey::from(x_pub));
    let mut wrap_key = [0u8; 32];
    Hkdf::<Sha256>::new(Some(b"daisy-envelope-v2"), shared.as_bytes())
        .expand(&[b"x25519-wrap".as_slice(), &ephemeral_pub, &x_pub].concat(), &mut wrap_key)
        .unwrap();
    let seal = |key: &[u8; 32], nonce_label: &str, plaintext: &[u8]| {
        let nonce: [u8; 12] = fixed(nonce_label)[..12].try_into().unwrap();
        let cipher = <Aes256Gcm as GcmKeyInit>::new_from_slice(key).unwrap();
        let boxed = cipher.encrypt(Nonce::from_slice(&nonce), plaintext).unwrap();
        (B64.encode(nonce), B64.encode(boxed))
    };
    let (body_nonce, body_ct) = seal(&data_key, "envelope-body-nonce", envelope_plaintext.as_bytes());
    let (device_nonce, device_wrapped) = seal(&phone_vault_key, "envelope-device-nonce", &data_key);
    let (peer_nonce, peer_wrapped) = seal(&wrap_key, "envelope-peer-nonce", &data_key);
    let envelope = obj(&[
        ("schema_version", 2.into()),
        ("cipher", s("aes-256-gcm")),
        ("nonce_b64", s(&body_nonce)),
        ("ciphertext_b64", s(&body_ct)),
        (
            "recipients",
            serde_json::json!([
                { "kind": "device_key", "id": phone_id, "nonce_b64": device_nonce, "wrapped_b64": device_wrapped },
                {
                    "kind": "x25519",
                    "id": mac_id,
                    "ephemeral_pub_b64": B64.encode(ephemeral_pub),
                    "nonce_b64": peer_nonce,
                    "wrapped_b64": peer_wrapped,
                },
            ]),
        ),
    ]);

    let v = Vectors {
        note: "Shared test vectors for docs/sync/protocol.md v1 (smbr-daisy-ios). Fixed inputs are SHA-256 of \
               'daisy-sync-vectors-v1:<label>'; every other value follows from them. Identical copies: \
               smbr-daisy-app crates/tauri-app/tests/fixtures/sync-vectors-v1.json and smbr-daisy-ios \
               ios/DaisyKit/Tests/DaisyKitTests/Fixtures/sync-vectors-v1.json. Regenerate with \
               `cargo test -p tauri-app --test sync_vectors -- --ignored write_vectors`."
            .into(),
        secret_b64: B64.encode(secret),
        desktop: Desktop {
            device_id: mac_id,
            name: mac_name.into(),
            ed25519_seed_b64: B64.encode(mac_seed),
            ed25519_pub_b64: B64.encode(mac_pub),
            x25519_secret_b64: B64.encode(x_secret.to_bytes()),
            x25519_pub_b64: B64.encode(x_pub),
            tls_key_der_b64: mac_tls_key,
            tls_cert_der_b64: mac_tls_cert,
            tls_spki_sha256_b64: mac_spki,
        },
        phone: Phone {
            device_id: phone_id,
            name: phone_name.into(),
            vault_key_b64: B64.encode(phone_vault_key),
            ed25519_seed_b64: B64.encode(phone_seed),
            ed25519_pub_b64: B64.encode(phone_pub),
            tls_key_der_b64: phone_tls_key,
            tls_cert_der_b64: phone_tls_cert,
            tls_spki_sha256_b64: phone_spki,
        },
        derived,
        pairing_code,
        pair_request,
        pair_reply,
        signed_requests,
        signed_reply,
        tombstone,
        wake_payload,
        address_payload,
        envelope_v2: EnvelopeSample {
            plaintext: envelope_plaintext.to_string(),
            data_key_b64: B64.encode(data_key),
            ephemeral_secret_b64: B64.encode(ephemeral_secret),
            envelope,
        },
    };
    let mut out = serde_json::to_string_pretty(&v).unwrap();
    out.push('\n');
    std::fs::write(FIXTURE, out).unwrap();
}
