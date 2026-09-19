// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! What the desktop puts on its two Bluetooth characteristics: a wake notify
//! and the address a woken phone connects back to. Both are sealed under the
//! pairing key, so a listener nearby learns neither that a meeting ended nor
//! where this machine is. Section 9 of the sync protocol.

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use rand::RngCore;

/// A payload's fields in the order section 9 writes them; a JSON object
/// built any other way sorts them and no longer matches the protocol text.
#[derive(serde::Serialize)]
struct Payload<'a> {
    t: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    n: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    host: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    port: Option<u16>,
}

/// The notify a phone wakes on: a fresh nonce, so two wakes never look alike
/// on the air.
pub fn wake_payload(pairing_key: &[u8; 32]) -> Option<Vec<u8>> {
    let mut n = [0u8; 16];
    rand::rng().fill_bytes(&mut n);
    seal(pairing_key, &Payload { t: "wake", n: Some(B64.encode(n)), host: None, port: None })
}

/// The address characteristic a phone reads after every connect: where this
/// machine is on the LAN right now.
pub fn address_payload(pairing_key: &[u8; 32], host: &str, port: u16) -> Option<Vec<u8>> {
    seal(pairing_key, &Payload { t: "addr", n: None, host: Some(host), port: Some(port) })
}

/// AES-256-GCM under the pairing key, wire bytes `nonce || ciphertext || tag`.
fn seal(pairing_key: &[u8; 32], payload: &Payload) -> Option<Vec<u8>> {
    let plaintext = serde_json::to_vec(payload).ok()?;
    let mut nonce = [0u8; 12];
    rand::rng().fill_bytes(&mut nonce);
    let cipher = Aes256Gcm::new_from_slice(pairing_key).ok()?;
    let boxed = cipher.encrypt(Nonce::from_slice(&nonce), plaintext.as_slice()).ok()?;
    Some([&nonce[..], &boxed].concat())
}

/// The plaintext of a payload, or `None` for one this pairing key does not
/// open. A payload that fails to authenticate is ignored, never guessed at.
pub fn open(pairing_key: &[u8; 32], wire: &[u8]) -> Option<Vec<u8>> {
    if wire.len() <= 12 + 16 {
        return None;
    }
    let cipher = Aes256Gcm::new_from_slice(pairing_key).ok()?;
    cipher.decrypt(Nonce::from_slice(&wire[..12]), &wire[12..]).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: [u8; 32] = [4u8; 32];

    #[test]
    fn a_wake_payload_is_a_nonce_the_phone_can_read() {
        let wire = wake_payload(&KEY).unwrap();
        let plain = open(&KEY, &wire).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&plain).unwrap();
        assert_eq!(value["t"], "wake");
        let n = base64::Engine::decode(&B64, value["n"].as_str().unwrap()).unwrap();
        assert_eq!(n.len(), 16, "sixteen random bytes, per section 9");
    }

    #[test]
    fn two_wakes_never_carry_the_same_bytes() {
        assert_ne!(wake_payload(&KEY).unwrap(), wake_payload(&KEY).unwrap());
    }

    #[test]
    fn an_address_payload_names_where_to_connect() {
        let wire = address_payload(&KEY, "192.168.1.20", 7845).unwrap();
        let plain = open(&KEY, &wire).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&plain).unwrap();
        assert_eq!(value["t"], "addr");
        assert_eq!(value["host"], "192.168.1.20");
        assert_eq!(value["port"], 7845);
    }

    #[test]
    fn the_wire_is_nonce_then_ciphertext_and_tag() {
        let wire = address_payload(&KEY, "10.0.0.2", 7845).unwrap();
        let plain = open(&KEY, &wire).unwrap();
        assert_eq!(wire.len(), 12 + plain.len() + 16);
    }

    #[test]
    fn another_pairings_key_reads_nothing() {
        let wire = wake_payload(&KEY).unwrap();
        assert!(open(&[9u8; 32], &wire).is_none());
    }

    #[test]
    fn a_payload_stays_inside_one_bluetooth_notification() {
        // A central is guaranteed 512 bytes per notify.
        assert!(wake_payload(&KEY).unwrap().len() <= 512);
        assert!(address_payload(&KEY, "fe80::1c2d:3e4f:5a6b:7c8d", 7845).unwrap().len() <= 512);
    }
}
