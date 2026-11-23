// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! The Bluetooth LE peripheral a phone wakes on: one service per pairing,
//! advertised with nothing else, carrying a wake notify, a writable challenge
//! and a readable address. Section 9a of the phone sync protocol.
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

/// How a [`Responder`] took a challenge write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Written {
    /// Kept against the central; its next address read answers it.
    Kept,
    /// The value is not a challenge's length; nothing was kept.
    WrongLength,
    /// The reply could not be sealed; nothing was kept.
    Failed,
    /// Writes arrive faster than the service answers, or every slot is in
    /// use; nothing was kept.
    Limited,
}

/// Answers the centrals of one pairing's service. Called on the radio's own
/// queue; an answer never calls back into the radio.
pub trait Responder: Send + Sync {
    /// A central, by its identifier, wrote `value` to the challenge
    /// characteristic.
    fn challenge(&self, central: &str, value: &[u8]) -> Written;
    /// The bytes a central's address read returns, or `None` when that
    /// central holds no challenge.
    fn address(&self, central: &str) -> Option<Vec<u8>>;
}

/// What the radio is doing. Only `PoweredOn` can be advertised on; the rest
/// are reasons a phone will not be woken, and the user can act on two of
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

    /// Publishes one pairing's service. UUIDs are the pairing's derived ones;
    /// `responder` answers the service's challenge writes and address reads
    /// with bytes this crate never opens.
    pub fn add_service(
        &self,
        service: &str,
        wake: &str,
        address: &str,
        challenge: &str,
        responder: std::sync::Arc<dyn Responder>,
    ) -> Result<(), Error> {
        #[cfg(target_os = "macos")]
        {
            self.inner.add_service(service, wake, address, challenge, responder)
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (service, wake, address, challenge, responder);
            Err(Error::Unsupported)
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

    /// True while the radio is putting the services on the air.
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
