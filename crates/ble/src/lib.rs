// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! The Bluetooth LE peripheral a phone wakes on: one service per pairing,
//! advertised with nothing else, carrying a notify and a readable address.
//! Section 9 of the phone sync protocol.
//!
//! macOS only. A desktop without a peripheral radio answers
//! [`Error::Unsupported`] and the phone reaches it over the LAN whenever its
//! app is open.

#[cfg(target_os = "macos")]
mod macos;

/// Why the peripheral is not running.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// This platform has no peripheral role in Daisy.
    Unsupported,
    /// CoreBluetooth refused: the message is the framework's.
    Radio(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Unsupported => write!(f, "bluetooth peripheral not supported on this platform"),
            Error::Radio(m) => write!(f, "bluetooth: {m}"),
        }
    }
}

impl std::error::Error for Error {}

/// What the radio is doing. Only `PoweredOn` can be advertised on; the rest
/// are reasons a phone will not be woken, and the owner can act on two of
/// them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Unknown,
    Resetting,
    Unsupported,
    Unauthorized,
    PoweredOff,
    PoweredOn,
}

impl State {
    /// The CoreBluetooth raw value, named. Kept on every platform so the
    /// mapping is testable where there is no radio.
    pub fn from_raw(raw: i32) -> Self {
        match raw {
            1 => State::Resetting,
            2 => State::Unsupported,
            3 => State::Unauthorized,
            4 => State::PoweredOff,
            5 => State::PoweredOn,
            _ => State::Unknown,
        }
    }
}

/// The peripheral. Advertising stops when this is dropped.
pub struct Peripheral {
    #[cfg(target_os = "macos")]
    inner: macos::Handle,
}

impl Peripheral {
    /// This build has a peripheral radio at all (macOS CoreBluetooth).
    pub fn supported() -> bool {
        cfg!(target_os = "macos")
    }

    /// Starts the radio. Services are added as pairings are known; the radio
    /// powers on asynchronously and anything added before then is published
    /// when it does.
    pub fn start() -> Result<Self, Error> {
        #[cfg(target_os = "macos")]
        {
            Ok(Self { inner: macos::Handle::start()? })
        }
        #[cfg(not(target_os = "macos"))]
        {
            Err(Error::Unsupported)
        }
    }

    /// Publishes one pairing's service and the address a phone reads after it
    /// connects. UUIDs are the pairing's derived ones; `address` is the
    /// sealed payload, which this crate never opens.
    pub fn add_service(&self, service: &str, wake: &str, address: &str, sealed_address: &[u8]) -> Result<(), Error> {
        #[cfg(target_os = "macos")]
        {
            self.inner.add_service(service, wake, address, sealed_address)
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (service, wake, address, sealed_address);
            Err(Error::Unsupported)
        }
    }

    /// Replaces what a read of that pairing's address characteristic returns,
    /// which is how a phone learns this machine moved.
    pub fn set_address(&self, service: &str, sealed_address: &[u8]) -> bool {
        #[cfg(target_os = "macos")]
        {
            self.inner.set_address(service, sealed_address)
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (service, sealed_address);
            false
        }
    }

    /// Wakes the phone paired on that service. True says the radio accepted
    /// the payload, never that a phone heard it: a wake sent with nobody
    /// subscribed is accepted and goes nowhere. False means the transmit
    /// queue is full and the caller should try again.
    pub fn notify(&self, service: &str, sealed_wake: &[u8]) -> bool {
        #[cfg(target_os = "macos")]
        {
            self.inner.notify(service, sealed_wake)
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (service, sealed_wake);
            false
        }
    }

    /// How many centrals are subscribed to that pairing's wake right now,
    /// which is the only local signal that a phone is listening.
    pub fn subscribers(&self, service: &str) -> i32 {
        #[cfg(target_os = "macos")]
        {
            self.inner.subscribers(service)
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = service;
            0
        }
    }

    /// True while the radio is putting the services on the air. Whether any
    /// phone can see them is a question only a second device answers.
    pub fn is_advertising(&self) -> bool {
        #[cfg(target_os = "macos")]
        {
            self.inner.is_advertising()
        }
        #[cfg(not(target_os = "macos"))]
        {
            false
        }
    }

    pub fn state(&self) -> State {
        #[cfg(target_os = "macos")]
        {
            State::from_raw(self.inner.state())
        }
        #[cfg(not(target_os = "macos"))]
        {
            State::Unsupported
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_core_bluetooth_state_has_a_name() {
        assert_eq!(State::from_raw(5), State::PoweredOn);
        assert_eq!(State::from_raw(4), State::PoweredOff);
        assert_eq!(State::from_raw(3), State::Unauthorized);
        assert_eq!(State::from_raw(2), State::Unsupported);
        assert_eq!(State::from_raw(1), State::Resetting);
        assert_eq!(State::from_raw(0), State::Unknown);
        assert_eq!(State::from_raw(99), State::Unknown);
    }

    #[test]
    fn an_error_says_which_half_failed() {
        assert!(Error::Unsupported.to_string().contains("not supported"));
        assert_eq!(Error::Radio("off".into()).to_string(), "bluetooth: off");
    }
}
