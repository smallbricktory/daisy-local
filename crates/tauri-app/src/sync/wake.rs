// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! What the desktop puts on its Bluetooth characteristics: a wake notify and an address reply, each sealed under the pairing key.

use aes_gcm::aead::{Aead, KeyInit, Payload as Sealed};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use hkdf::Hkdf;
use rand::RngCore;
use sha2::Sha256;

/// Sixteen bytes a phone writes to the challenge characteristic.
pub type Challenge = [u8; 16];

/// A wake's own sixteen random bytes, `n` on the wire.
pub type WakeNonce = [u8; 16];

/// Associated data of a wake notify.
pub const WAKE_AAD: &[u8] = b"daisy-gatt-2/wake";

/// Associated data of an address reply, before the challenge it answers.
pub const ADDRESS_LABEL: &[u8] = b"daisy-gatt-2/addr";

/// A payload's fields in wire order.
#[derive(serde::Serialize)]
struct Payload<'a> {
    t: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    n: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    host: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    port: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    w: Option<String>,
}

/// The challenge characteristic's uuid for one pairing:
/// `uuid(HKDF(ikm=pairing_key, salt="daisy-gatt-2", info="char-challenge", 16))`.
pub fn challenge_uuid(pairing_key: &[u8; 32]) -> String {
    let mut b = [0u8; 16];
    Hkdf::<Sha256>::new(Some(b"daisy-gatt-2"), pairing_key)
        .expand(b"char-challenge", &mut b)
        .expect("16 bytes is a valid HKDF length");
    uuid::Builder::from_random_bytes(b).into_uuid().hyphenated().to_string().to_uppercase()
}

/// A wake notify with fresh random `n`: that `n` and the sealed bytes.
pub fn wake_payload(pairing_key: &[u8; 32]) -> Option<(WakeNonce, Vec<u8>)> {
    let mut n = [0u8; 16];
    rand::rng().fill_bytes(&mut n);
    let payload = Payload { t: "wake", n: Some(B64.encode(n)), host: None, port: None, w: None };
    Some((n, seal(pairing_key, WAKE_AAD, &payload)?))
}

/// The address reply to one challenge: where this machine is on the LAN, and
/// `w`, the pending wake's nonce, when one is pending.
pub fn address_payload(
    pairing_key: &[u8; 32],
    challenge: &Challenge,
    host: &str,
    port: u16,
    w: Option<&WakeNonce>,
) -> Option<Vec<u8>> {
    let payload = Payload { t: "addr", n: None, host: Some(host), port: Some(port), w: w.map(|n| B64.encode(n)) };
    seal(pairing_key, &address_aad(challenge), &payload)
}

/// `daisy-gatt-2/addr || challenge`.
pub fn address_aad(challenge: &Challenge) -> Vec<u8> {
    [ADDRESS_LABEL, &challenge[..]].concat()
}

/// AES-256-GCM under the pairing key with a random nonce, wire bytes
/// `nonce || ciphertext || tag`.
fn seal(pairing_key: &[u8; 32], aad: &[u8], payload: &Payload) -> Option<Vec<u8>> {
    let plaintext = serde_json::to_vec(payload).ok()?;
    let mut nonce = [0u8; 12];
    rand::rng().fill_bytes(&mut nonce);
    let cipher = Aes256Gcm::new_from_slice(pairing_key).ok()?;
    let boxed = cipher.encrypt(Nonce::from_slice(&nonce), Sealed { msg: &plaintext, aad }).ok()?;
    Some([&nonce[..], &boxed].concat())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    const KEY: [u8; 32] = [4u8; 32];
    const CHALLENGE: Challenge = [7u8; 16];

    /// What a phone does with the wire: the plaintext, or `None` when it does
    /// not open under that key and associated data.
    pub(crate) fn open(key: &[u8; 32], aad: &[u8], wire: &[u8]) -> Option<Vec<u8>> {
        if wire.len() <= 12 + 16 {
            return None;
        }
        let cipher = Aes256Gcm::new_from_slice(key).ok()?;
        cipher.decrypt(Nonce::from_slice(&wire[..12]), Sealed { msg: &wire[12..], aad }).ok()
    }

    fn json(bytes: &[u8]) -> serde_json::Value {
        serde_json::from_slice(bytes).unwrap()
    }

    #[test]
    fn a_wake_carries_the_nonce_it_returns() {
        let (n, wire) = wake_payload(&KEY).unwrap();
        let value = json(&open(&KEY, WAKE_AAD, &wire).unwrap());
        assert_eq!(value["t"], "wake");
        assert_eq!(value["n"], B64.encode(n));
        assert_eq!(value.as_object().unwrap().len(), 2);
    }

    #[test]
    fn two_wakes_never_carry_the_same_bytes() {
        let (a, wa) = wake_payload(&KEY).unwrap();
        let (b, wb) = wake_payload(&KEY).unwrap();
        assert_ne!(a, b);
        assert_ne!(wa, wb);
    }

    #[test]
    fn an_address_opens_only_under_its_own_challenge() {
        let wire = address_payload(&KEY, &CHALLENGE, "192.168.1.20", 7845, None).unwrap();
        let value = json(&open(&KEY, &address_aad(&CHALLENGE), &wire).unwrap());
        assert_eq!(value["t"], "addr");
        assert_eq!(value["host"], "192.168.1.20");
        assert_eq!(value["port"], 7845);
        assert!(value.get("w").is_none(), "no wake pending, no w");

        assert!(open(&KEY, &address_aad(&[8u8; 16]), &wire).is_none(), "another challenge");
        assert!(open(&KEY, b"", &wire).is_none(), "no associated data");
        assert!(open(&KEY, WAKE_AAD, &wire).is_none(), "the wake label");
        assert!(open(&[9u8; 32], &address_aad(&CHALLENGE), &wire).is_none(), "another pairing's key");
    }

    #[test]
    fn a_pending_wake_rides_in_the_address_as_w() {
        let n = [5u8; 16];
        let wire = address_payload(&KEY, &CHALLENGE, "10.0.0.2", 7845, Some(&n)).unwrap();
        let plain = open(&KEY, &address_aad(&CHALLENGE), &wire).unwrap();
        assert_eq!(
            String::from_utf8(plain).unwrap(),
            format!(r#"{{"t":"addr","host":"10.0.0.2","port":7845,"w":"{}"}}"#, B64.encode(n))
        );
    }

    #[test]
    fn a_wake_does_not_open_as_an_address() {
        let (_, wire) = wake_payload(&KEY).unwrap();
        assert!(open(&KEY, b"", &wire).is_none());
        assert!(open(&KEY, &address_aad(&CHALLENGE), &wire).is_none());
        assert!(open(&KEY, ADDRESS_LABEL, &wire).is_none());
    }

    #[test]
    fn the_wire_is_nonce_then_ciphertext_and_tag() {
        let wire = address_payload(&KEY, &CHALLENGE, "10.0.0.2", 7845, None).unwrap();
        let plain = open(&KEY, &address_aad(&CHALLENGE), &wire).unwrap();
        assert_eq!(wire.len(), 12 + plain.len() + 16);
    }

    #[test]
    fn the_labels_are_seventeen_ascii_bytes() {
        assert_eq!(WAKE_AAD.len(), 17);
        assert_eq!(ADDRESS_LABEL.len(), 17);
        assert_eq!(address_aad(&CHALLENGE).len(), 33);
    }

    #[test]
    fn the_challenge_uuid_is_a_version_4_uuid_of_the_pairing_key() {
        let a = challenge_uuid(&KEY);
        assert_eq!(a, challenge_uuid(&KEY));
        assert_ne!(a, challenge_uuid(&[9u8; 32]));
        assert_eq!(a.len(), 36);
        assert_eq!(&a[14..15], "4");
        assert_eq!(a, a.to_uppercase());
    }

    #[test]
    fn a_payload_stays_inside_one_bluetooth_attribute() {
        // An attribute value is at most 512 bytes.
        assert!(wake_payload(&KEY).unwrap().1.len() <= 512);
        let longest = "a".repeat(253);
        assert!(address_payload(&KEY, &CHALLENGE, &longest, 65535, Some(&[0xFF; 16])).unwrap().len() <= 512);
    }
}
