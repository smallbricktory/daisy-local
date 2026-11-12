// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! The process-wide keyring as a suite's tests share it. A test that reads
//! or writes sealed session files holds [`unlocked`]; a test that locks the
//! vault (and with it the keyring) holds [`exclusive`].
#![allow(dead_code)]
use std::sync::{OnceLock, RwLock, RwLockReadGuard, RwLockWriteGuard};

static RING: RwLock<()> = RwLock::new(());

/// The keyring unlocked (with one content key per test process when no test
/// left another open), held so no exclusive test locks it.
pub fn unlocked() -> RwLockReadGuard<'static, ()> {
    static SECRET: OnceLock<[u8; 32]> = OnceLock::new();
    let guard = RING.read().unwrap_or_else(|e| e.into_inner());
    if !sealfile::keyring::is_unlocked() {
        let bytes = *SECRET.get_or_init(|| *sealfile::ContentSecret::generate().0.to_bytes());
        sealfile::keyring::unlock(sealfile::ContentSecret::from_bytes(bytes));
    }
    guard
}

/// Sole use of the keyring, unlocked with a fresh content key.
pub fn exclusive() -> RwLockWriteGuard<'static, ()> {
    let guard = RING.write().unwrap_or_else(|e| e.into_inner());
    sealfile::keyring::unlock(sealfile::ContentSecret::generate().0);
    guard
}
