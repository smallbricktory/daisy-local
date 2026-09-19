// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Vault lifecycle commands: init / unlock / lock / status / set_provider /
//! list_providers.

use crate::error::{AppError, Result};
use crate::state::{AppState, DecryptedKeys, ProviderConfig, ProviderId, VaultState};
use serde::Serialize;
use std::path::PathBuf;
use vault::{decrypt, encrypt, Envelope};

pub fn vault_path(app: &AppState) -> PathBuf {
    app.profile.root().join("keys.vault.json")
}

#[derive(Debug, Serialize)]
pub struct VaultStatus {
    pub vault_exists: bool,
    pub unlocked: bool,
}

pub fn vault_status_impl(app: &AppState, vs: &VaultState) -> Result<VaultStatus> {
    Ok(VaultStatus {
        vault_exists: vault_path(app).is_file(),
        unlocked: vs.is_unlocked(),
    })
}

/// First-run only: create an empty vault encrypted with the chosen passphrase.
pub fn init_vault_impl(app: &AppState, vs: &VaultState, passphrase: &str) -> Result<()> {
    let path = vault_path(app);
    if path.is_file() {
        return Err(AppError::Config("vault already exists".into()));
    }
    let plaintext = serde_json::to_vec(&DecryptedKeys::default())?;
    let env = encrypt(&plaintext, passphrase)
        .map_err(|e| AppError::Config(format!("vault: {e}")))?;
    let bytes = serde_json::to_vec_pretty(&env)?;
    syncsafe::write_atomic(&path, &bytes)?;
    *vs.keys.lock().unwrap() = Some(DecryptedKeys::default());
    *vs.passphrase.lock().unwrap() = Some(zeroize::Zeroizing::new(passphrase.to_string()));
    Ok(())
}

pub fn unlock_vault_impl(app: &AppState, vs: &VaultState, passphrase: &str) -> Result<()> {
    let path = vault_path(app);
    if !path.is_file() {
        return Err(AppError::Config(
            "vault does not exist; create one in Welcome".into(),
        ));
    }
    let bytes = syncsafe::read(&path)?;
    let env: Envelope = serde_json::from_slice(&bytes)?;
    let plaintext = decrypt(&env, passphrase)
        .map_err(|e| AppError::Config(format!("vault: {e}")))?;
    let keys: DecryptedKeys = serde_json::from_slice(&plaintext)
        .map_err(|e| AppError::Config(format!("vault payload: {e}")))?;
    *vs.keys.lock().unwrap() = Some(keys);
    *vs.passphrase.lock().unwrap() = Some(zeroize::Zeroizing::new(passphrase.to_string()));
    // One-shot data migrations (see crate::migrations). Best-effort and
    // marker-gated; never fails the unlock.
    crate::migrations::run_on_unlock(app, vs);
    Ok(())
}

/// Re-encrypt the vault under a new passphrase. Requires the old passphrase
/// even when the vault is already unlocked. Updates the in-memory stash.
pub fn change_vault_passphrase_impl(
    app: &AppState,
    vs: &VaultState,
    old: &str,
    new: &str,
) -> Result<()> {
    if new.is_empty() {
        return Err(AppError::Config("new passphrase must not be empty".into()));
    }
    let path = vault_path(app);
    if !path.is_file() {
        return Err(AppError::Config(
            "vault does not exist; create one in Welcome".into(),
        ));
    }
    // Verify the old passphrase decrypts the envelope.
    let bytes = syncsafe::read(&path)?;
    let env: Envelope = serde_json::from_slice(&bytes)?;
    let _plain = decrypt(&env, old)
        .map_err(|_| AppError::Config("wrong current passphrase".into()))?;
    // Re-encrypt the in-memory keys under the new passphrase; falls back to
    // the just-decrypted plaintext when the vault is not unlocked.
    let plaintext: Vec<u8> = {
        let keys_guard = vs.keys.lock().unwrap();
        match keys_guard.as_ref() {
            Some(keys) => serde_json::to_vec(keys)?,
            None => _plain.to_vec(),
        }
    };
    let new_env = encrypt(&plaintext, new)
        .map_err(|e| AppError::Config(format!("vault: {e}")))?;
    let new_bytes = serde_json::to_vec_pretty(&new_env)?;
    let identity = crate::sync::identity::resealed(app, old, new)?;
    syncsafe::write_atomic(&path, &new_bytes)?;
    if let Some(bytes) = identity {
        crate::sync::identity::write_sealed(app, &bytes)?;
    }
    *vs.passphrase.lock().unwrap() = Some(zeroize::Zeroizing::new(new.to_string()));
    Ok(())
}

/// Switch the vault between passphrase-mode and machine-mode in place,
/// preserving the full payload. The vault must be unlocked; the in-memory
/// payload is re-encrypted under the target key, then the kind sidecar is
/// flipped atomically.
///
/// `new_passphrase`: `Some(p)` → switch to passphrase-mode under `p`;
///                   `None`    → switch to machine-mode (key from machine id).
/// Returns the resulting vault kind ("passphrase" | "machine").
pub fn switch_vault_mode_impl(
    app: &AppState,
    vs: &VaultState,
    new_passphrase: Option<&str>,
) -> Result<String> {
    let plaintext: Vec<u8> = {
        let g = vs.keys.lock().unwrap();
        let keys = g.as_ref().ok_or(AppError::VaultLocked)?;
        serde_json::to_vec(keys)?
    };
    let current = vs.passphrase.lock().unwrap().clone().ok_or(AppError::VaultLocked)?;
    let (key, kind): (String, &str) = match new_passphrase {
        Some(p) => {
            if p.trim().is_empty() {
                return Err(AppError::Config("new passphrase must not be empty".into()));
            }
            (p.to_string(), "passphrase")
        }
        None => {
            let pass = crate::machine_id::machine_passphrase();
            if pass.is_empty() {
                return Err(AppError::Config(
                    "machine ID unavailable; cannot switch to machine mode".into(),
                ));
            }
            (pass, "machine")
        }
    };
    let env = encrypt(&plaintext, &key).map_err(|e| AppError::Config(format!("vault: {e}")))?;
    let path = vault_path(app);
    let bytes = serde_json::to_vec_pretty(&env)?;
    let identity = crate::sync::identity::resealed(app, current.as_str(), &key)?;
    syncsafe::write_atomic(&path, &bytes)?;
    if let Some(bytes) = identity {
        crate::sync::identity::write_sealed(app, &bytes)?;
    }
    write_vault_kind(app, kind)?;
    *vs.passphrase.lock().unwrap() = Some(zeroize::Zeroizing::new(key));
    Ok(kind.to_string())
}

/// Sidecar marker file that records whether the vault is in passphrase-mode
/// or machine-mode. Absent = passphrase mode. Holds the single line `machine`
/// or `passphrase`.
fn vault_kind_path(app: &AppState) -> PathBuf {
    app.profile.root().join("keys.vault.kind")
}

/// Machine mode: names the computer whose key opens the vault.
fn vault_owner_path(app: &AppState) -> PathBuf {
    app.profile.root().join("keys.vault.owner")
}

/// Records the vault kind. Machine mode writes this computer's name to the
/// owner sidecar first; passphrase mode removes it.
fn write_vault_kind(app: &AppState, kind: &str) -> Result<()> {
    if kind == "machine" {
        write_vault_owner(app, &crate::hardware::machine_name())?;
    }
    syncsafe::write_atomic(vault_kind_path(app), format!("{kind}\n"))?;
    if kind != "machine" {
        remove_vault_owner(app)?;
    }
    Ok(())
}

fn write_vault_owner(app: &AppState, name: &str) -> Result<()> {
    syncsafe::write_atomic(vault_owner_path(app), format!("{name}\n"))?;
    Ok(())
}

fn remove_vault_owner(app: &AppState) -> Result<()> {
    let op = vault_owner_path(app);
    if op.is_file() {
        syncsafe::remove_file(&op)
            .map_err(|e| AppError::Config(format!("remove vault owner: {e}")))?;
    }
    Ok(())
}

/// The kind marker's non-empty lines, trimmed.
fn read_vault_kind_lines(app: &AppState) -> Vec<String> {
    syncsafe::read_to_string(vault_kind_path(app))
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
}

/// "passphrase" (default) or "machine". Reads the marker's first line.
pub fn read_vault_kind(app: &AppState) -> String {
    read_vault_kind_lines(app)
        .into_iter()
        .next()
        .unwrap_or_else(|| "passphrase".to_string())
}

/// The computer recorded as the machine-mode vault's owner: the owner
/// sidecar, else a second line in the kind marker.
fn read_vault_owner(app: &AppState) -> Option<String> {
    syncsafe::read_to_string(vault_owner_path(app))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| read_vault_kind_lines(app).into_iter().nth(1))
}

/// How the profile's vault is opened, for the unlock screen.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct VaultTrust {
    /// "passphrase" or "machine".
    pub kind: String,
    /// Machine mode: the computer whose key opens the vault. `None` in
    /// passphrase mode and for machine-mode vaults that predate the record.
    pub trusted_on: Option<String>,
    /// This computer's name.
    pub this_machine: String,
}

pub fn read_vault_trust(app: &AppState) -> VaultTrust {
    let kind = read_vault_kind(app);
    let trusted_on = if kind == "machine" { read_vault_owner(app) } else { None };
    VaultTrust { kind, trusted_on, this_machine: crate::hardware::machine_name() }
}

/// Initialize the vault without prompting for a passphrase. The encryption
/// key is derived from the machine ID.
pub fn init_vault_machine_mode_impl(app: &AppState, vs: &VaultState) -> Result<()> {
    let pass = crate::machine_id::machine_passphrase();
    if pass.is_empty() {
        return Err(AppError::Config(
            "machine ID unavailable; cannot init machine-mode vault".into(),
        ));
    }
    init_vault_impl(app, vs, &pass)?;
    write_vault_kind(app, "machine")
}

/// Auto-unlock a machine-mode vault at startup. No-op when the sidecar
/// says passphrase mode (or is missing). On failure the vault stays locked.
/// On success the owner sidecar is brought up to this computer's current
/// name and the kind marker to its single line.
pub fn unlock_if_machine_mode_impl(app: &AppState, vs: &VaultState) -> Result<bool> {
    if read_vault_kind(app) != "machine" {
        return Ok(false);
    }
    let pass = crate::machine_id::machine_passphrase();
    if pass.is_empty() {
        return Ok(false);
    }
    unlock_vault_impl(app, vs, &pass)?;
    refresh_vault_owner(app);
    Ok(true)
}

fn refresh_vault_owner(app: &AppState) {
    let here = crate::hardware::machine_name();
    let owner_current = syncsafe::read_to_string(vault_owner_path(app))
        .map(|s| s.trim() == here)
        .unwrap_or(false);
    if !owner_current {
        if let Err(e) = write_vault_owner(app, &here) {
            log::warn!("vault: couldn't record owner name: {e:?}");
        }
    }
    if read_vault_kind_lines(app).len() > 1 {
        if let Err(e) = syncsafe::write_atomic(vault_kind_path(app), "machine\n") {
            log::warn!("vault: couldn't rewrite kind marker: {e:?}");
        }
    }
}

pub fn lock_vault_impl(app: &AppState, vs: &VaultState) -> Result<()> {
    *vs.keys.lock().unwrap() = None;
    *vs.passphrase.lock().unwrap() = None;
    *app.sync_window.lock().unwrap() = None;
    crate::sync::identity::forget_cached();
    Ok(())
}

/// Re-encrypt the in-memory DecryptedKeys to disk using the stashed passphrase.
pub fn re_encrypt_keys(app: &AppState, vs: &VaultState) -> Result<()> {
    let keys_guard = vs.keys.lock().unwrap();
    let keys = keys_guard
        .as_ref()
        .ok_or(AppError::VaultLocked)?;
    let pass_guard = vs.passphrase.lock().unwrap();
    let pass = pass_guard
        .as_ref()
        .ok_or_else(|| AppError::Config("vault passphrase not available".into()))?;
    let plaintext = serde_json::to_vec(keys)?;
    let env = encrypt(&plaintext, pass.as_str())
        .map_err(|e| AppError::Config(format!("vault: {e}")))?;
    let path = vault_path(app);
    let bytes = serde_json::to_vec_pretty(&env)?;
    syncsafe::write_atomic(&path, &bytes)?;
    Ok(())
}

/// Enable Daisy Cloud: ensure the per-install Ed25519 keypair exists in the
/// vault (generated + persisted on first use), then register its public key
/// with the license server. The vault must be unlocked. Idempotent.
pub fn register_gateway_impl(app: &AppState, vs: &VaultState) -> Result<()> {
    let pubkey = {
        let mut guard = vs.keys.lock().unwrap();
        let keys = guard.as_mut().ok_or(AppError::VaultLocked)?;
        let (seed, _generated) = crate::state::get_or_create_install_seed(keys);
        crate::state::pubkey_b64_from_seed(&seed)
            .ok_or_else(|| AppError::Config("couldn't derive the gateway key".into()))?
    };
    // Persist any newly-generated seed before registering its public key.
    re_encrypt_keys(app, vs)?;
    crate::commands::license::register_gateway_install_impl(&pubkey)
}

/// Derive this install's gateway public key from the vault seed (generating +
/// persisting the seed on first use), sent with `/api/activate`. Returns
/// `None` when the vault is locked.
pub fn derive_install_pubkey(app: &AppState, vs: &VaultState) -> Option<String> {
    let pubkey = {
        let mut guard = vs.keys.lock().unwrap();
        let keys = guard.as_mut()?; // vault locked → None
        let (seed, _generated) = crate::state::get_or_create_install_seed(keys);
        crate::state::pubkey_b64_from_seed(&seed)?
    };
    // Persist any newly-generated seed (best-effort).
    if let Err(e) = re_encrypt_keys(app, vs) {
        log::warn!("activate: couldn't persist install seed: {e:?}");
    }
    Some(pubkey)
}

/// Replace one provider's config and persist. The vault must already be
/// unlocked; the rewrite re-encrypts with the passphrase stashed at unlock
/// time.
pub fn set_provider_impl(
    app: &AppState,
    vs: &VaultState,
    provider: ProviderId,
    config: ProviderConfig,
) -> Result<()> {
    {
        let mut guard = vs.keys.lock().unwrap();
        let current = guard
            .as_mut()
            .ok_or(AppError::VaultLocked)?;
        // Merge with any existing entry: a `None` field means "leave
        // unchanged"; an explicit empty-string api_key or a zero
        // context_tokens means "clear".
        let prev = current.providers.get(&provider);
        let prev_key = prev.and_then(|c| c.api_key.clone());
        let prev_model = prev.and_then(|c| c.model.clone());
        let prev_base = prev.and_then(|c| c.base_url.clone());
        let prev_ctx = prev.and_then(|c| c.context_tokens);
        let merged = ProviderConfig {
            api_key: match config.api_key {
                None => prev_key,
                Some(ref s) if s.is_empty() => None,
                Some(s) => Some(s),
            },
            model: config.model.or(prev_model),
            base_url: config.base_url.or(prev_base),
            context_tokens: match config.context_tokens {
                None => prev_ctx,
                Some(0) => None,
                Some(n) => Some(n),
            },
        };
        current.providers.insert(provider, merged);
    }
    re_encrypt_keys(app, vs)
}

#[derive(Debug, Serialize)]
pub struct ProviderListEntry {
    pub name: ProviderId,
    pub has_key: bool,
    pub model: Option<String>,
    pub base_url: Option<String>,
    pub context_tokens: Option<u32>,
}

pub fn list_providers_impl(vs: &VaultState) -> Result<Vec<ProviderListEntry>> {
    let guard = vs.keys.lock().unwrap();
    let keys = guard
        .as_ref()
        .ok_or(AppError::VaultLocked)?;
    let mut out: Vec<ProviderListEntry> = keys
        .providers
        .iter()
        .map(|(name, cfg)| ProviderListEntry {
            name: *name,
            has_key: cfg.api_key.is_some(),
            model: cfg.model.clone(),
            base_url: cfg.base_url.clone(),
            context_tokens: cfg.context_tokens,
        })
        .collect();
    // Daisy Cloud has no vault entry; synthesize its list entry — only for
    // licenses whose stamp carries the daisy_cloud entitlement.
    if crate::commands::license::gateway_entitled()
        && !out.iter().any(|e| e.name == ProviderId::DaisyGateway)
    {
        out.push(ProviderListEntry {
            name: ProviderId::DaisyGateway,
            has_key: false,
            model: None,
            base_url: None,
            context_tokens: None,
        });
    }
    Ok(out)
}

/// Destroy the vault file, the sync identity sealed under its key, and the
/// unlocked state. `tags.json` and sessions are not touched.
pub fn reset_vault_impl(app: &AppState, vs: &VaultState) -> Result<()> {
    let path = vault_path(app);
    if path.is_file() {
        syncsafe::remove_file(&path)
            .map_err(|e| AppError::Config(format!("remove vault: {e}")))?;
    }
    crate::sync::identity::remove(app)?;
    let kp = vault_kind_path(app);
    if kp.is_file() {
        syncsafe::remove_file(&kp)
            .map_err(|e| AppError::Config(format!("remove vault kind: {e}")))?;
    }
    remove_vault_owner(app)?;
    *vs.keys.lock().unwrap() = None;
    *vs.passphrase.lock().unwrap() = None;
    *app.sync_window.lock().unwrap() = None;
    // Drop the v3-migration marker.
    let m = app.profile.root().join(".migrated_v3");
    let _ = syncsafe::remove_file(&m);
    Ok(())
}
