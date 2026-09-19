// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! What one machine can prove about the peripheral: the radio powers on, a
//! pairing's service is published without complaint, and the advertisement is
//! running. A wake sent with nobody subscribed reports that nobody heard it.
//!
//! What one machine cannot prove: that the advertisement is discoverable. A
//! Mac does not see its own advertisements. Discovery, the address read and a
//! delivered notify need a second device: `scripts/ble-scan.sh` is that half,
//! run on another Mac or the phone.
//!
//! Ignored by default; it needs a radio that is powered on:
//!
//! ```sh
//! cargo test -p ble --test macos_peripheral_probe -- --ignored --nocapture
//! ```
#![cfg(target_os = "macos")]

/// The uuids of one pairing, distinct per run so nothing stale can pass.
fn uuids() -> (String, String, String) {
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_micros() as u64;
    (
        format!("D4A87EF9-7B7A-4AD3-806E-{:012X}", stamp & 0xFFFF_FFFF_FFFF),
        "0A23320A-898F-4334-8D10-93B7F079AEE2".to_string(),
        "C5057A9F-691F-4D19-8510-395B1430A39E".to_string(),
    )
}

fn powered_on(p: &ble::Peripheral) -> bool {
    for _ in 0..50 {
        if p.state() == ble::State::PoweredOn {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    false
}

#[test]
#[ignore = "needs a powered-on Bluetooth radio; takes a few seconds"]
fn a_pairings_service_goes_on_the_air() {
    let peripheral = ble::Peripheral::start().expect("the radio starts");
    assert!(powered_on(&peripheral), "no powered-on radio to test with: {:?}", peripheral.state());

    let (service, wake, address) = uuids();
    peripheral.add_service(&service, &wake, &address, b"sealed-address-payload").expect("published");
    // Advertising restarts on the CoreBluetooth queue.
    for _ in 0..50 {
        if peripheral.is_advertising() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert!(peripheral.is_advertising(), "the service is being advertised");

    // A second pairing joins the same advertisement.
    let (service2, _, _) = uuids();
    peripheral.add_service(&service2, &wake, &address, b"another-phone").expect("two phones, two services");
    assert!(peripheral.is_advertising());

    // The address a phone would read is replaceable while the radio runs.
    assert!(peripheral.set_address(&service, b"moved-to-another-network"));
    assert!(!peripheral.set_address("D4A87EF9-0000-4000-8000-000000000000", b"nobody"), "an unknown service");

    // Nobody is listening, which the subscriber count is the signal for. The
    // radio still accepts the wake: measured, `updateValue` answers true with
    // no subscribed central, so its answer says queued, never heard.
    assert_eq!(peripheral.subscribers(&service), 0);
    assert!(peripheral.notify(&service, b"sealed-wake-payload"));

    drop(peripheral);
}

#[test]
#[ignore = "needs a powered-on Bluetooth radio"]
fn a_uuid_that_is_not_a_uuid_is_refused() {
    let peripheral = ble::Peripheral::start().expect("the radio starts");
    assert!(powered_on(&peripheral), "no powered-on radio to test with");
    let err = peripheral.add_service("not-a-uuid", "also-not", "nor-this", b"x").unwrap_err();
    assert!(matches!(err, ble::Error::Radio(_)), "{err:?}");
    assert!(!peripheral.is_advertising(), "nothing was published, so nothing is on the air");
}
