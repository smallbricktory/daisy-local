// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! This machine's sync identity: signing key, key-agreement key, TLS key and
//! certificate, device id and the paired phones. Lives in
//! `sync.<machine>.vault.json` beside `keys.vault.json`, sealed with the
//! same passphrase; the file name keeps it out of every other machine's
//! view of a shared profile.
use std::path::{Path, PathBuf};

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use vault::{decrypt, encrypt, Envelope};

use crate::error::{AppError, Result};
use crate::state::{AppState, VaultState};

pub const SCHEMA: u32 = 2;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PairedPhone {
    pub device_id: String,
    pub name: String,
    pub platform: String,
    pub ed25519_pub_b64: String,
    pub tls_cert_der_b64: String,
    pub pairing_key_b64: String,
    pub service_uuid: String,
    pub wake_uuid: String,
    pub address_uuid: String,
    pub paired_at_unix: i64,
    /// The name this desktop shows for the phone, set by a rename here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
}

impl PairedPhone {
    /// The rename when there is one, else the name the phone paired with.
    pub fn shown_name(&self) -> &str {
        self.display_name.as_deref().unwrap_or(&self.name)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SyncIdentity {
    pub schema_version: u32,
    pub device_id: String,
    pub ed25519_seed_b64: String,
    pub x25519_secret_b64: String,
    pub tls_key_der_b64: String,
    pub tls_cert_der_b64: String,
    #[serde(default)]
    pub paired: Vec<PairedPhone>,
}

impl SyncIdentity {
    pub(crate) fn mint() -> Result<Self> {
        let mut rng = rand::rng();
        let mut device = [0u8; 16];
        rng.fill_bytes(&mut device);
        let mut seed = [0u8; 32];
        rng.fill_bytes(&mut seed);
        let mut x_bytes = [0u8; 32];
        rng.fill_bytes(&mut x_bytes);
        let x = x25519_dalek::StaticSecret::from(x_bytes);
        let key = rcgen::KeyPair::generate()
            .map_err(|e| AppError::Config(format!("tls key: {e}")))?;
        let mut params = rcgen::CertificateParams::new(vec![format!("daisy-{}", hex(&device))])
            .map_err(|e| AppError::Config(format!("tls cert: {e}")))?;
        params.not_after = rcgen::date_time_ymd(2099, 12, 31);
        let cert = params
            .self_signed(&key)
            .map_err(|e| AppError::Config(format!("tls cert: {e}")))?;
        Ok(Self {
            schema_version: SCHEMA,
            device_id: hex(&device),
            ed25519_seed_b64: B64.encode(seed),
            x25519_secret_b64: B64.encode(x.to_bytes()),
            tls_key_der_b64: B64.encode(key.serialize_der()),
            tls_cert_der_b64: B64.encode(cert.der()),
            paired: Vec::new(),
        })
    }

    pub fn signing_key(&self) -> ed25519_dalek::SigningKey {
        let bytes = B64.decode(&self.ed25519_seed_b64).expect("seed written by mint");
        ed25519_dalek::SigningKey::from_bytes(&bytes.try_into().expect("32-byte seed"))
    }

    pub fn ed25519_pub_b64(&self) -> String {
        B64.encode(self.signing_key().verifying_key().to_bytes())
    }

    /// The envelope key that opens what a locked phone sealed to this machine.
    pub fn machine_key(&self) -> crate::sync::import::MachineKey {
        let bytes = B64.decode(&self.x25519_secret_b64).expect("secret written by mint");
        crate::sync::import::MachineKey {
            x25519_secret: bytes.try_into().expect("32-byte secret"),
            device_id: self.device_id.clone(),
        }
    }

    /// The key a phone wraps a file's data key to (envelope v2, recipient
    /// kind `x25519`). Handed over in the pairing reply.
    pub fn x25519_pub_b64(&self) -> String {
        let bytes = B64.decode(&self.x25519_secret_b64).expect("secret written by mint");
        let secret = x25519_dalek::StaticSecret::from(<[u8; 32]>::try_from(bytes).expect("32-byte secret"));
        B64.encode(x25519_dalek::PublicKey::from(&secret).to_bytes())
    }

    /// SHA-256 of the certificate's SubjectPublicKeyInfo: what a phone pins.
    pub fn tls_spki_sha256_b64(&self) -> String {
        let key = B64.decode(&self.tls_key_der_b64).expect("key written by mint");
        let pair = rcgen::KeyPair::try_from(key.as_slice()).expect("PKCS#8 written by mint");
        B64.encode(Sha256::digest(pair.public_key_der()))
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn identity_path(app: &AppState) -> PathBuf {
    let name = crate::hardware::sanitized_machine_name();
    app.profile.root().join(format!("sync.{name}.vault.json"))
}

fn passphrase(vs: &VaultState) -> Result<zeroize::Zeroizing<String>> {
    if !vs.is_unlocked() {
        return Err(AppError::VaultLocked);
    }
    vs.passphrase
        .lock()
        .unwrap()
        .clone()
        .ok_or_else(|| AppError::Config("vault passphrase not available".into()))
}

/// Serializes the read-or-mint sequence process-wide. Two callers reaching a
/// machine with no identity yet would otherwise mint one each, and the one
/// that wrote last would be the only one a phone could reach.
static MINT_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The identity as last read or written, so a burst of phone requests does
/// not run Argon2id once each: opening the file costs hundreds of
/// milliseconds and a phone gives the whole exchange about ten seconds.
/// Dropped when the vault locks.
static CACHE: std::sync::Mutex<Option<(PathBuf, SyncIdentity)>> = std::sync::Mutex::new(None);

fn cached(path: &Path) -> Option<SyncIdentity> {
    let guard = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    guard.as_ref().filter(|(p, _)| p == path).map(|(_, id)| id.clone())
}

fn remember(path: &Path, id: &SyncIdentity) {
    *CACHE.lock().unwrap_or_else(|e| e.into_inner()) = Some((path.to_path_buf(), id.clone()));
}

/// Drops the cached identity: called when the vault locks, is re-keyed or is
/// reset, so nothing serves a phone from an identity the vault no longer has.
pub fn forget_cached() {
    *CACHE.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

pub fn load_or_create(app: &AppState, vs: &VaultState) -> Result<SyncIdentity> {
    let _guard = MINT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let pass = passphrase(vs)?;
    let path = identity_path(app);
    if let Some(id) = cached(&path) {
        return Ok(id);
    }
    if path.is_file() {
        let env: Envelope = serde_json::from_slice(&syncsafe::read(&path)?)?;
        let plain = decrypt(&env, pass.as_str())
            .map_err(|e| AppError::Config(format!("sync identity: {e}")))?;
        let id: SyncIdentity = serde_json::from_slice(&plain)
            .map_err(|e| AppError::Config(format!("sync identity payload: {e}")))?;
        if id.schema_version == SCHEMA {
            remember(&path, &id);
            return Ok(id);
        }
        // Schema 1 paired under a pairing protocol the phone never spoke;
        // those pairings cannot be repaired, only redone.
        log::warn!("sync identity schema {} replaced; every phone re-pairs", id.schema_version);
    }
    let id = SyncIdentity::mint()?;
    write(app, &pass, &id)?;
    Ok(id)
}

/// The identity file re-sealed under `new_pass`, ready for `write_sealed`;
/// `None` when this machine has no identity yet. A vault re-key computes this
/// before it writes `keys.vault.json`, so a failure here leaves both files
/// under the old key.
pub fn resealed(app: &AppState, old_pass: &str, new_pass: &str) -> Result<Option<Vec<u8>>> {
    let path = identity_path(app);
    if !path.is_file() {
        return Ok(None);
    }
    let env: Envelope = serde_json::from_slice(&syncsafe::read(&path)?)?;
    let plain = decrypt(&env, old_pass).map_err(|e| AppError::Config(format!("sync identity: {e}")))?;
    let env = encrypt(&plain, new_pass).map_err(|e| AppError::Config(format!("sync identity: {e}")))?;
    Ok(Some(serde_json::to_vec_pretty(&env)?))
}

pub fn write_sealed(app: &AppState, bytes: &[u8]) -> Result<()> {
    forget_cached();
    syncsafe::write_atomic(identity_path(app), bytes)?;
    Ok(())
}

/// Removes the identity file; the next `load_or_create` mints a fresh one and
/// every phone has to pair again.
pub fn remove(app: &AppState) -> Result<()> {
    forget_cached();
    let path = identity_path(app);
    if path.is_file() {
        syncsafe::remove_file(&path)?;
    }
    Ok(())
}

pub fn save(app: &AppState, vs: &VaultState, id: &SyncIdentity) -> Result<()> {
    let _guard = MINT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let pass = passphrase(vs)?;
    write(app, &pass, id)
}

/// Seals and writes the identity. The caller holds `MINT_LOCK`.
fn write(app: &AppState, pass: &str, id: &SyncIdentity) -> Result<()> {
    let env = encrypt(&serde_json::to_vec(id)?, pass)
        .map_err(|e| AppError::Config(format!("sync identity: {e}")))?;
    let path = identity_path(app);
    syncsafe::write_atomic(&path, &serde_json::to_vec_pretty(&env)?)?;
    remember(&path, id);
    Ok(())
}
