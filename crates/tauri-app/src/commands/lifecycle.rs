// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Vault lifecycle commands: init / unlock / lock / status / set_provider /
//! list_providers.
//!
//! Every vault transition ends in exactly one of [`vault_opened`] (init,
//! unlock, the startup auto-unlock) or [`vault_closed`] (lock, reset). The
//! keys and the passphrase are set and cleared here and nowhere else in the
//! app; closing the vault also closes the pairing window.

use crate::error::{AppError, Result};
use crate::state::{AppState, DecryptedKeys, ProviderConfig, ProviderId, VaultState};
use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use sealfile::{keyring, keyset, recovery, ContentSecret};
use serde::Serialize;
use std::path::PathBuf;
use vault::{decrypt, encrypt, Envelope};
use zeroize::Zeroizing;

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
/// Creates the vault under a passphrase a person chose. A passphrase with
/// the machine-key prefix is refused.
pub fn init_vault_impl(app: &AppState, vs: &VaultState, passphrase: &str) -> Result<()> {
    refuse_reserved(passphrase)?;
    create_vault(app, vs, passphrase)
}

/// The message for a chosen passphrase that starts like a machine key.
pub const RESERVED_PASSPHRASE: &str = "A passphrase can't begin with \"daisy.v1.machine:\". Choose a different one.";

/// Refuses a passphrase a person chose that starts with the prefix machine keys carry.
fn refuse_reserved(passphrase: &str) -> Result<()> {
    if passphrase.starts_with(crate::machine_id::MACHINE_KEY_PREFIX) {
        return Err(AppError::Config(RESERVED_PASSPHRASE.into()));
    }
    Ok(())
}

/// Creates the vault, empty, under `key` and holds it open.
fn create_vault(app: &AppState, vs: &VaultState, key: &str) -> Result<()> {
    let path = vault_path(app);
    if path.is_file() {
        return Err(AppError::Config("vault already exists".into()));
    }
    let plaintext = serde_json::to_vec(&DecryptedKeys::default())?;
    let env = encrypt(&plaintext, key)
        .map_err(|e| AppError::Config(format!("vault: {e}")))?;
    let bytes = serde_json::to_vec_pretty(&env)?;
    syncsafe::write_atomic(&path, &bytes)?;
    *vs.keys.lock().unwrap() = Some(DecryptedKeys::default());
    *vs.passphrase.lock().unwrap() = Some(zeroize::Zeroizing::new(key.to_string()));
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
    refuse_reserved(new)?;
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
    let content = content_files_resealed(app, old, new)?;
    stage_resealed(&content)?;
    syncsafe::write_atomic(&path, &new_bytes)?;
    if let Some(bytes) = identity {
        crate::sync::identity::write_sealed(app, &bytes)?;
    }
    commit_resealed(&content)?;
    *vs.passphrase.lock().unwrap() = Some(zeroize::Zeroizing::new(new.to_string()));
    Ok(())
}

/// Switch the vault between passphrase-mode and machine-mode in place,
/// preserving the full payload. The vault must be unlocked; the in-memory
/// payload is re-encrypted under the target key, then the kind sidecar is
/// flipped atomically.
///
/// `current_passphrase`: on a vault open with a passphrase, that passphrase;
///                       anything else is refused before the vault is
///                       touched. A vault open with a machine key needs none.
/// `new_passphrase`: `Some(p)` → switch to passphrase-mode under `p`;
///                   `None`    → switch to machine-mode (key from machine id).
/// Returns the resulting vault kind ("passphrase" | "machine").
pub fn switch_vault_mode_impl(
    app: &AppState,
    vs: &VaultState,
    current_passphrase: Option<&str>,
    new_passphrase: Option<&str>,
) -> Result<String> {
    let plaintext: Vec<u8> = {
        let g = vs.keys.lock().unwrap();
        let keys = g.as_ref().ok_or(AppError::VaultLocked)?;
        serde_json::to_vec(keys)?
    };
    if !opened_with_machine_key(vs) && !is_current_passphrase(vs, current_passphrase.unwrap_or_default()) {
        return Err(AppError::Config("wrong current passphrase".into()));
    }
    let current = vs.passphrase.lock().unwrap().clone().ok_or(AppError::VaultLocked)?;
    let (key, kind): (String, &str) = match new_passphrase {
        Some(p) => {
            if p.trim().is_empty() {
                return Err(AppError::Config("new passphrase must not be empty".into()));
            }
            refuse_reserved(p)?;
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
    let content = content_files_resealed(app, current.as_str(), &key)?;
    stage_resealed(&content)?;
    syncsafe::write_atomic(&path, &bytes)?;
    if let Some(bytes) = identity {
        crate::sync::identity::write_sealed(app, &bytes)?;
    }
    commit_resealed(&content)?;
    write_vault_kind(app, kind)?;
    *vs.passphrase.lock().unwrap() = Some(zeroize::Zeroizing::new(key));
    Ok(kind.to_string())
}

/// Whether the open vault opened with a machine key. False while locked.
fn opened_with_machine_key(vs: &VaultState) -> bool {
    vs.passphrase.lock().unwrap().as_ref().is_some_and(|k| k.starts_with(crate::machine_id::MACHINE_KEY_PREFIX))
}

/// Whether `given` is the passphrase the open vault holds, compared in
/// constant time. False while locked and for an empty `given`.
fn is_current_passphrase(vs: &VaultState, given: &str) -> bool {
    use subtle::ConstantTimeEq;
    !given.is_empty() && vs.passphrase.lock().unwrap().as_ref().is_some_and(|k| bool::from(k.as_bytes().ct_eq(given.as_bytes())))
}

/// The vault kind the app acts on: from the key in memory while the vault
/// is open, from the kind sidecar while it is locked.
pub fn vault_kind_impl(app: &AppState, vs: &VaultState) -> String {
    if !vs.is_unlocked() {
        return read_vault_kind(app);
    }
    if opened_with_machine_key(vs) { "machine" } else { "passphrase" }.to_string()
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
    /// Machine mode: the computer whose key opens the vault. `None` in passphrase mode and for machine-mode vaults with no record.
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
    create_vault(app, vs, &pass)?;
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

/// Forgets everything the open vault gave this process: the keys, the
/// passphrase, an open pairing window, the cached sync identity, the content
/// secret, every session key that is not pinned, and (Linux) the decrypted
/// playback file. The keys go before the playback file: deleting it waits
/// for no copy in progress, and a copy that ends after the lock deletes its
/// own file.
pub fn lock_vault_impl(app: &AppState, vs: &VaultState) -> Result<()> {
    *vs.keys.lock().unwrap() = None;
    *vs.passphrase.lock().unwrap() = None;
    *app.sync_window.lock().unwrap() = None;
    crate::sync::identity::forget_cached();
    crate::media_scheme::forget_built_audio();
    keyring::lock();
    #[cfg(target_os = "linux")]
    crate::playback_file::clear();
    Ok(())
}

/// What follows the vault opening, in order: the content key (minted on a
/// profile that has none, with `encryption:words_ready` carrying the new
/// recovery words), the one-shot migrations (the background seal reports on
/// `encryption:progress`), then the listeners that read
/// their secrets from it (MCP, phone sync), the launch maintenance that
/// decrypts provider credentials, the workflow queue, and the status every
/// AI gate reads.
pub fn vault_opened<R: tauri::Runtime>(handle: &tauri::AppHandle<R>) {
    use tauri::{Emitter, Manager};
    {
        let app = handle.state::<AppState>();
        let vs = handle.state::<VaultState>();
        match open_content_key(&app, &vs) {
            Ok(ContentKeyOpen::Minted(words)) => {
                if let Err(e) = handle.emit("encryption:words_ready", words) {
                    log::warn!("content key: couldn't announce the recovery words: {e:?}");
                }
            }
            Ok(ContentKeyOpen::Opened) => {}
            Ok(ContentKeyOpen::Missing) => log::warn!("content key: none on this profile yet"),
            Ok(ContentKeyOpen::Unopenable) => log::warn!("content key: does not open with the vault's key"),
            Err(e) => log::warn!("content key: {e:?}"),
        }
        let emitter = handle.clone();
        crate::migrations::run_on_unlock(&app, &vs, move |progress| {
            if let Err(e) = emitter.emit("encryption:progress", progress) {
                log::warn!("seal: couldn't announce progress: {e:?}");
            }
        });
    }
    if let Err(e) = crate::mcp::server::apply_mcp_state(handle) {
        log::warn!("mcp: after the vault opened: {e:?}");
    }
    crate::sync::server::apply_sync_state(handle);
    crate::commands::finalize::spawn_post_unlock_maintenance(handle.clone());
    if let Some(wf) = handle.try_state::<crate::commands::workflow_engine::WorkflowEngineHandle>() {
        wf.0.notify_one();
    }
    crate::builtin_model::emit(handle);
}

/// What follows the vault closing: its secrets are forgotten, the built-in
/// model stops (a request in flight ends), a recording's live names lose
/// the saved voices, and the listeners that hold its secrets go down.
pub fn vault_closed<R: tauri::Runtime>(handle: &tauri::AppHandle<R>) {
    use tauri::Manager;
    if let Some(picks) = handle.try_state::<crate::commands::files::DialogPicks>() {
        picks.clear_all();
    }
    {
        let app = handle.state::<AppState>();
        let vs = handle.state::<VaultState>();
        let _ = lock_vault_impl(&app, &vs);
        crate::builtin_model::stop(&app);
    }
    crate::live_names::forget_voices();
    crate::sync::server::apply_sync_state(handle);
    if let Err(e) = crate::mcp::server::apply_mcp_state(handle) {
        log::warn!("mcp: after the vault closed: {e:?}");
    }
    crate::builtin_model::emit(handle);
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

/// Ensure the per-install Ed25519 keypair exists in the vault (generated and
/// persisted on first use), then register its public key with the license
/// server. The vault must be unlocked. Idempotent.
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

/// Derive this install's public key from the vault seed (generating and
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
    // A provider with no vault entry gets a synthesized list entry, only for
    // licenses whose stamp carries the matching entitlement.
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
/// unlocked state. `tags.json`, sessions and the content key files are not
/// touched; after a reset only the recovery words reopen the content key.
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
    lock_vault_impl(app, vs)
}

// ── Content key set ────────────────────────────────────────────────────
//
// The profile holds a set of content key pairs (`sealfile::keyset`), each
// key's four files named by its fingerprint. Beside them, per machine:
// `content.<machine>.active` names the fingerprint of the key this machine
// wraps new session keys to, and `content.<machine>.words.pending` names it
// until the person confirms seeing that key's recovery words. The
// unfingerprinted `content.words.pending` counts as pending as well.
//
// Every key whose vault file opens with the vault's key is held open. The
// active key is the open key of lowest fingerprint; the seal step moves
// every session wrapped to another open key onto it, and that key's files
// stay in the profile.

/// What Settings and the restore screen show for any wrong recovery entry.
pub const RECOVERY_INCORRECT: &str = "The recovery key you entered is incorrect.";

/// The words-pending marker earlier builds wrote, for every machine.
const LEGACY_WORDS_PENDING: &str = "content.words.pending";

/// The profile file that names the fingerprint of the content key this
/// machine wraps new session keys to.
pub fn active_file_name() -> String {
    format!("content.{}.active", crate::hardware::sanitized_machine_name())
}

fn active_path(app: &AppState) -> PathBuf {
    app.profile.root().join(active_file_name())
}

fn words_pending_path(app: &AppState) -> PathBuf {
    app.profile.root().join(format!("content.{}.words.pending", crate::hardware::sanitized_machine_name()))
}

/// What opening the content keys found.
pub enum ContentKeyOpen {
    /// At least one content key opened with the vault's key.
    Opened,
    /// The profile had no content key files: a key pair and recovery words
    /// were made. The words are shown once.
    Minted(Vec<String>),
    /// No key opened, and one of the profile's keys has not fully synced in
    /// (its vault file is not there yet). Nothing was written.
    Missing,
    /// No key opened: every key's vault file is sealed under another
    /// passphrase or holds another key. The recovery words reopen a key, or
    /// a new key starts ([`content_key_start_new_impl`]).
    Unopenable,
}

/// Serializes opening, minting and restoring content keys process-wide.
static CONTENT_MINT_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The bytes of a file sealed with the vault's key, as `keys.vault.json` is.
fn seal_with_passphrase(plain: &[u8], pass: &str) -> Result<Vec<u8>> {
    let env = encrypt(plain, pass).map_err(|e| AppError::Config(format!("vault: {e}")))?;
    Ok(serde_json::to_vec_pretty(&env)?)
}

fn open_with_passphrase(path: &std::path::Path, pass: &str) -> Option<Zeroizing<Vec<u8>>> {
    let env: Envelope = serde_json::from_slice(&syncsafe::read(path).ok()?).ok()?;
    decrypt(&env, pass).ok()
}

/// The content secret in the key's vault file, when it opens with `pass`
/// and is that key ([`keyset::KeyFiles::is_key_of`]). When it does not and
/// the copy a passphrase change staged (`<name>.next`) does, the staged
/// copy is moved into place. Either way a staged words file that opens
/// with `pass` is moved into place.
fn open_content_secret(key: &keyset::KeyFiles, pass: &str) -> Option<ContentSecret> {
    let open = |path: &std::path::Path| -> Option<ContentSecret> {
        let plain = open_with_passphrase(path, pass)?;
        let bytes: [u8; 32] = plain.as_slice().try_into().ok()?;
        let secret = ContentSecret::from_bytes(bytes);
        key.is_key_of(&secret).then_some(secret)
    };
    let (vault, words) = (key.vault_path(), key.words_path());
    let secret = match open(&vault) {
        Some(secret) => secret,
        None => {
            let secret = open(&next_path(&vault))?;
            if let Err(e) = syncsafe::rename(next_path(&vault), &vault) {
                log::warn!("content key: staged content key not moved into place: {e}");
            }
            secret
        }
    };
    if next_path(&words).is_file() && open_with_passphrase(&next_path(&words), pass).is_some() {
        if let Err(e) = syncsafe::rename(next_path(&words), &words) {
            log::warn!("content key: staged recovery words not moved into place: {e}");
        }
    }
    Some(secret)
}

fn vault_key(vs: &VaultState) -> Result<Zeroizing<String>> {
    if !vs.is_unlocked() {
        return Err(AppError::VaultLocked);
    }
    vs.passphrase
        .lock()
        .unwrap()
        .clone()
        .ok_or_else(|| AppError::Config("vault passphrase not available".into()))
}

/// A key of the set, by its fingerprint, or `content` for unfingerprinted
/// files with no public key.
fn key_id(key: &keyset::KeyFiles) -> String {
    key.fingerprint().unwrap_or_else(|| "content".into())
}

/// A key's vault file as it is on disk: the key's id ([`key_id`]) with the
/// file's length and modification time. `None` while the file is not
/// there.
fn vault_version(key: &keyset::KeyFiles) -> Option<String> {
    let meta = std::fs::metadata(key.vault_path()).ok()?;
    let at = meta.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_nanos());
    Some(format!("{}:{}:{at}", key_id(key), meta.len()))
}

/// Per profile, the versions of the vault files ([`vault_version`]) tried
/// so far.
static TRIED: std::sync::Mutex<Option<std::collections::HashMap<PathBuf, std::collections::HashSet<String>>>> =
    std::sync::Mutex::new(None);

fn note_tried(app: &AppState, versions: impl IntoIterator<Item = String>) {
    let mut guard = TRIED.lock().unwrap_or_else(|e| e.into_inner());
    guard.get_or_insert_with(Default::default).entry(app.profile.root().to_path_buf()).or_default().extend(versions);
}

/// True when the key's vault file is there as it was when it was last
/// tried.
fn was_tried(app: &AppState, key: &keyset::KeyFiles) -> bool {
    let Some(version) = vault_version(key) else { return false };
    let guard = TRIED.lock().unwrap_or_else(|e| e.into_inner());
    guard.as_ref().and_then(|m| m.get(app.profile.root())).is_some_and(|t| t.contains(&version))
}

/// True when a key's vault file is in the profile as it was not the last
/// time the content keys were opened: it has synced in, or been replaced,
/// since.
fn untried_vault(app: &AppState) -> bool {
    keyset::list(app.profile.root()).iter().any(|k| k.vault_path().is_file() && !was_tried(app, k))
}

/// True when the key of the profile `public` names, or any key of the
/// profile with `public` `None`, is not held and may still open without
/// the recovery words: its files or its vault file have not synced in, or
/// its vault file has not been tried as it is now.
pub fn waiting_for_key(app: &AppState, public: Option<&sealfile::ContentPublic>) -> bool {
    let held: Vec<String> = keyring::open_publics().iter().map(keyset::fingerprint).collect();
    let set = keyset::list(app.profile.root());
    let pending = |k: &keyset::KeyFiles| {
        !k.fingerprint().is_some_and(|fp| held.contains(&fp)) && (!k.vault_path().is_file() || !was_tried(app, k))
    };
    match public {
        Some(p) => {
            let fp = keyset::fingerprint(p);
            let of = |k: &&keyset::KeyFiles| k.fingerprint().as_deref() == Some(fp.as_str());
            let unnamed_tried = set.iter().any(|k| k.fingerprint().is_none() && !pending(k));
            !held.contains(&fp) && set.iter().filter(of).all(pending) && (set.iter().any(|k| of(&k)) || !unnamed_tried)
        }
        None => set.iter().any(pending),
    }
}

/// Opens every content key of the profile whose vault file opens with the
/// vault's key and hands it to the keyring; on a profile with no content
/// key files, makes a key pair and recovery words first. The open key of
/// lowest fingerprint becomes the key new session keys are wrapped to
/// ([`note_active`]). The keyring reads this profile's content public key
/// while it holds none. The secrets opened replace those held before;
/// pinned session keys are kept.
pub fn open_content_key(app: &AppState, vs: &VaultState) -> Result<ContentKeyOpen> {
    open_content_key_with(app, vs, true).map(|(opened, _)| opened)
}

/// [`open_content_key`] with `fresh`. Without it, only keys not held yet
/// are opened, beside those held, and a profile with no key files is
/// [`ContentKeyOpen::Missing`]. Also returns whether this call opened a key
/// that was not open before.
fn open_content_key_with(app: &AppState, vs: &VaultState, fresh: bool) -> Result<(ContentKeyOpen, bool)> {
    let _guard = CONTENT_MINT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let pass = vault_key(vs)?;
    let root = app.profile.root();
    keyring::set_profile_root(root, Some(&active_file_name()));
    let set = keyset::list(root);
    let held: Vec<String> = if fresh { Vec::new() } else { keyring::open_publics().iter().map(keyset::fingerprint).collect() };
    let opened: Vec<ContentSecret> = set
        .iter()
        .filter(|key| !key.fingerprint().is_some_and(|fp| held.contains(&fp)))
        .filter_map(|key| {
            let secret = open_content_secret(key, &pass)?;
            settle_opened(key, &secret);
            Some(secret)
        })
        .collect();
    note_tried(app, set.iter().filter_map(vault_version));
    let opened_now = if fresh {
        let any = !opened.is_empty();
        keyring::hold_secrets(opened);
        any
    } else {
        opened.into_iter().fold(false, |any, secret| keyring::add_secret(secret) | any)
    };
    crate::commands::library::invalidate_library_cache();
    if keyring::is_unlocked() {
        note_active(app, true)?;
        return Ok((ContentKeyOpen::Opened, opened_now));
    }
    keyring::forget_content_public();
    if set.is_empty() {
        if !fresh {
            return Ok((ContentKeyOpen::Missing, false));
        }
        return Ok((ContentKeyOpen::Minted(mint_content_key(app, &pass)?), true));
    }
    let waiting = set.iter().any(|k| !k.vault_path().is_file());
    Ok((if waiting { ContentKeyOpen::Missing } else { ContentKeyOpen::Unopenable }, false))
}

/// Brings the files of a key that opened by passphrase up to date: its
/// archived marker is removed, and its public key file written when it is
/// not there.
fn settle_opened(key: &keyset::KeyFiles, secret: &ContentSecret) {
    if key.is_archived() {
        if let Err(e) = syncsafe::remove_file(key.archived_path()) {
            log::warn!("content key: archived marker of {} not removed: {e}", key_id(key));
        }
    }
    if key.public().is_none() && !key.pub_path().is_file() {
        if let Err(e) = syncsafe::write_atomic_synced(key.pub_path(), format!("{}\n", B64.encode(secret.public().0))) {
            log::warn!("content key: public key of {} not written: {e}", key_id(key));
        }
    }
}

/// Makes the open key of lowest fingerprint the content public key and
/// records it in `content.<machine>.active`. With `show_words`, a change
/// from the key that file named before marks the new key's words pending.
fn note_active(app: &AppState, show_words: bool) -> Result<()> {
    let Some(active) = keyring::open_publics().into_iter().min_by_key(keyset::fingerprint) else { return Ok(()) };
    keyring::set_content_public(active);
    let fp = keyset::fingerprint(&active);
    let before = syncsafe::read_to_string(active_path(app)).ok();
    if before.as_deref().map(str::trim) == Some(fp.as_str()) {
        return Ok(());
    }
    syncsafe::write_atomic(active_path(app), &fp)?;
    if show_words && before.is_some() {
        syncsafe::write_atomic(words_pending_path(app), &fp)?;
    }
    Ok(())
}

/// Writes a new key's four files, `content.<fp>.pub` last so a key whose
/// public key is in the profile has the others there too, and marks its
/// words pending. The key is held open and, as the only open key, becomes
/// the active key.
fn mint_content_key(app: &AppState, pass: &str) -> Result<Vec<String>> {
    let (secret, public) = ContentSecret::generate();
    let words = recovery::generate_words();
    let phrase = Zeroizing::new(words.join(" "));
    let secret_bytes = secret.to_bytes();
    let key = keyset::files_of(app.profile.root(), &public);
    syncsafe::write_atomic_synced(key.vault_path(), seal_with_passphrase(&*secret_bytes, pass)?)?;
    syncsafe::write_atomic_synced(key.words_path(), seal_with_passphrase(phrase.as_bytes(), pass)?)?;
    syncsafe::write_atomic_synced(key.recovery_path(), recovery::wrap_with_words(&phrase, &secret_bytes))?;
    syncsafe::write_atomic(words_pending_path(app), keyset::fingerprint(&public))?;
    syncsafe::write_atomic_synced(key.pub_path(), format!("{}\n", B64.encode(public.0)))?;
    crate::migrations::sync_dir(app.profile.root())?;
    keyring::add_secret(secret);
    note_tried(app, vault_version(&key));
    note_active(app, false)?;
    crate::commands::library::invalidate_library_cache();
    Ok(words.to_vec())
}

/// Every key's vault and words files re-sealed under `new_pass`, ready for
/// `stage_resealed`. A file that is absent or does not open with `old_pass`
/// is left as it is.
fn content_files_resealed(app: &AppState, old_pass: &str, new_pass: &str) -> Result<Vec<(PathBuf, Vec<u8>)>> {
    let mut out = Vec::new();
    for key in keyset::list(app.profile.root()) {
        for path in [key.vault_path(), key.words_path()] {
            if !path.is_file() {
                continue;
            }
            match open_with_passphrase(&path, old_pass) {
                Some(plain) => out.push((path, seal_with_passphrase(&plain, new_pass)?)),
                None => log::warn!("content key: {} does not open with the current key; left as it is", path.display()),
            }
        }
    }
    Ok(out)
}

/// Writes each re-sealed file beside its original as `<name>.next`.
fn stage_resealed(files: &[(PathBuf, Vec<u8>)]) -> Result<()> {
    for (path, bytes) in files {
        syncsafe::write_atomic(next_path(path), bytes)?;
    }
    Ok(())
}

/// Moves each staged `<name>.next` over its original.
fn commit_resealed(files: &[(PathBuf, Vec<u8>)]) -> Result<()> {
    for (path, _) in files {
        syncsafe::rename(next_path(path), path)?;
    }
    Ok(())
}

/// `<name>.next` beside `path`: a content file re-sealed under the new vault
/// key, written before `keys.vault.json` and moved over the file after it.
fn next_path(path: &std::path::Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".next");
    path.with_file_name(name)
}

/// Sessions the background seal has finished out of the sessions it found
/// holding plaintext or under another content key; also the
/// `encryption:progress` payload.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct MigrationProgress {
    pub done: u64,
    pub total: u64,
}

#[derive(Debug, Serialize)]
pub struct EncryptionStatus {
    /// "ok" (a content key is open), "missing" (none is open, and the
    /// profile has no key yet or one of its keys has not fully synced in)
    /// or "unopenable" (the recovery words reopen a key, or a new one
    /// starts).
    pub content_key: &'static str,
    /// The active key's recovery words are not confirmed seen on this
    /// machine.
    pub words_pending: bool,
    pub migration: MigrationProgress,
}

/// Whether a content key is open. While the vault is open, a key whose
/// vault file has synced in since the keys were last opened is opened
/// first (never minting). "missing" while no key is open and the profile
/// has no key files, or a key whose vault file is not there; "unopenable"
/// while no key is open and every key's vault file is there. With the vault
/// locked, a profile whose keys all have their vault files reports "ok".
pub fn encryption_status_impl(app: &AppState, vs: &VaultState) -> EncryptionStatus {
    encryption_status_opening(app, vs).0
}

/// [`encryption_status_impl`], and whether this call opened a key.
fn encryption_status_opening(app: &AppState, vs: &VaultState) -> (EncryptionStatus, bool) {
    let mut opened = false;
    if vs.is_unlocked() && untried_vault(app) {
        match open_content_key_with(app, vs, false) {
            Ok((_, now)) => opened = now,
            Err(e) => log::warn!("content key: {e:?}"),
        }
    }
    let set = keyset::list(app.profile.root());
    let content_key = if keyring::is_unlocked() {
        "ok"
    } else if set.is_empty() || set.iter().any(|k| !k.vault_path().is_file()) {
        "missing"
    } else if vs.is_unlocked() {
        "unopenable"
    } else {
        "ok"
    };
    let words_pending = words_pending_path(app).is_file() || app.profile.root().join(LEGACY_WORDS_PENDING).is_file();
    let status = EncryptionStatus { content_key, words_pending, migration: crate::migrations::seal_progress(app.profile.root()) };
    (status, opened)
}

/// The person has seen the recovery words: clears this machine's
/// words-pending marker and the one earlier builds wrote.
pub fn recovery_words_acknowledge_impl(app: &AppState) -> Result<()> {
    for path in [words_pending_path(app), app.profile.root().join(LEGACY_WORDS_PENDING)] {
        if path.is_file() {
            syncsafe::remove_file(&path)?;
        }
    }
    Ok(())
}

/// The active key's recovery words, opened with `passphrase`. In machine
/// mode the open vault's key opens them and `passphrase` is not read.
pub fn recovery_words_show_impl(app: &AppState, vs: &VaultState, passphrase: &str) -> Result<Vec<String>> {
    let key = if opened_with_machine_key(vs) { vault_key(vs)? } else { Zeroizing::new(passphrase.to_string()) };
    let no_words = || AppError::Config("no recovery words on this profile".into());
    let active = keyring::content_public().map(|p| keyset::fingerprint(&p)).ok_or_else(no_words)?;
    let path = keyset::list(app.profile.root())
        .into_iter()
        .find(|k| k.fingerprint().as_deref() == Some(active.as_str()) && k.words_path().is_file())
        .map(|k| k.words_path())
        .ok_or_else(no_words)?;
    let plain = open_with_passphrase(&path, &key).ok_or_else(|| AppError::Config("wrong passphrase".into()))?;
    let phrase = std::str::from_utf8(&plain).map_err(|_| AppError::Config("recovery words unreadable".into()))?;
    Ok(phrase.split(' ').map(str::to_string).collect())
}

/// Reopens a content key of the profile that is not open with its recovery
/// words: the first key whose recovery file the words open. Its vault and
/// words files are re-sealed under the vault's current key (its public key
/// file written when it is not there, its archived marker removed), it is
/// held open, and the active key is chosen again. Words that open the
/// unfingerprinted recovery file to a key other than the one its
/// `content.pub` names reopen that key under its fingerprinted names,
/// beside the unfingerprinted files. Before unfingerprinted files with no
/// `content.pub` are re-sealed, a `content.vault.json` that opens and holds
/// another key is copied, with its words file, to that key's fingerprinted
/// names. Words that open only keys already open are refused; any other
/// wrong entry is [`RECOVERY_INCORRECT`].
pub fn recovery_words_restore_impl(app: &AppState, vs: &VaultState, words: &str) -> Result<()> {
    let _guard = CONTENT_MINT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let pass = vault_key(vs)?;
    let incorrect = || AppError::Config(RECOVERY_INCORRECT.into());
    let phrase = Zeroizing::new(recovery::normalize_entry(words).ok_or_else(incorrect)?);
    let held: Vec<String> = keyring::open_publics().iter().map(keyset::fingerprint).collect();
    let root = app.profile.root();
    let mut already_open = false;
    for key in keyset::list(root) {
        let Ok(file) = syncsafe::read(key.recovery_path()) else { continue };
        let Ok(bytes) = recovery::unwrap_with_words(&phrase, &file).map(Zeroizing::new) else { continue };
        let secret = ContentSecret::from_bytes(*bytes);
        let public = secret.public();
        if held.contains(&keyset::fingerprint(&public)) {
            already_open = true;
            continue;
        }
        let target = if key.is_key_of(&secret) {
            if key.is_legacy() && key.public().is_none() {
                move_out_other_legacy_key(&key, &public, &pass)?;
            }
            key
        } else if key.is_legacy() {
            let target = keyset::files_of(root, &public);
            syncsafe::write_atomic_synced(target.recovery_path(), &file)?;
            target
        } else {
            continue;
        };
        syncsafe::write_atomic_synced(target.vault_path(), seal_with_passphrase(&*bytes, &pass)?)?;
        syncsafe::write_atomic_synced(target.words_path(), seal_with_passphrase(phrase.as_bytes(), &pass)?)?;
        if target.public().is_none() || !target.pub_path().is_file() {
            syncsafe::write_atomic_synced(target.pub_path(), format!("{}\n", B64.encode(public.0)))?;
        }
        if target.is_archived() {
            syncsafe::remove_file(target.archived_path())?;
        }
        crate::migrations::sync_dir(root)?;
        keyring::add_secret(secret);
        note_tried(app, vault_version(&target));
        note_active(app, false)?;
        crate::commands::library::invalidate_library_cache();
        return Ok(());
    }
    Err(if already_open { AppError::Config("the content key these recovery words open is already open".into()) } else { incorrect() })
}

/// Copies the unfingerprinted `content.vault.json` to the fingerprinted
/// names of the key it holds, when it opens with `pass` and holds a key
/// other than `restoring`, with `content.words.vault.json` when its words
/// do not open `restoring`'s recovery file and that key's `.pub` file.
fn move_out_other_legacy_key(legacy: &keyset::KeyFiles, restoring: &sealfile::ContentPublic, pass: &str) -> Result<()> {
    let Some(plain) = open_with_passphrase(&legacy.vault_path(), pass) else { return Ok(()) };
    let Ok(bytes) = <[u8; 32]>::try_from(plain.as_slice()) else { return Ok(()) };
    let other = ContentSecret::from_bytes(bytes).public();
    if other == *restoring {
        return Ok(());
    }
    let root = legacy.vault_path().parent().map(std::path::Path::to_path_buf).unwrap_or_default();
    let to = keyset::files_of(&root, &other);
    syncsafe::write_atomic_synced(to.vault_path(), syncsafe::read(legacy.vault_path())?)?;
    let words_open_restoring = open_with_passphrase(&legacy.words_path(), pass)
        .and_then(|w| String::from_utf8(w.to_vec()).ok())
        .zip(syncsafe::read(legacy.recovery_path()).ok())
        .and_then(|(w, file)| recovery::unwrap_with_words(&w, &file).ok())
        .is_some_and(|b| ContentSecret::from_bytes(b).public() == *restoring);
    if legacy.words_path().is_file() && !words_open_restoring {
        syncsafe::write_atomic_synced(to.words_path(), syncsafe::read(legacy.words_path())?)?;
    }
    if !to.pub_path().is_file() {
        syncsafe::write_atomic_synced(to.pub_path(), format!("{}\n", B64.encode(other.0)))?;
    }
    log::info!("content key: {} moved to its own names before a restore", keyset::fingerprint(&other));
    Ok(())
}

/// Starts a new content key while no key of the profile opens (the
/// recovery words are lost) and every key's vault file is there as it was
/// last tried (the status is "unopenable"): marks every key of the profile archived
/// ([`keyset::KeyFiles::archived_path`]), then makes a key pair and
/// recovery words, held open as the active key, with the words marked
/// pending. The profile's other keys and the sessions wrapped to them stay
/// as they are; their recovery words reopen them later
/// ([`recovery_words_restore_impl`]). Returns the new words.
pub fn content_key_start_new_impl(app: &AppState, vs: &VaultState) -> Result<Vec<String>> {
    let _guard = CONTENT_MINT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let pass = vault_key(vs)?;
    if keyring::is_unlocked() {
        return Err(AppError::Config("a content key is already open".into()));
    }
    let set = keyset::list(app.profile.root());
    if set.is_empty() || set.iter().any(|k| !k.vault_path().is_file() || !was_tried(app, k)) {
        return Err(AppError::Config("a content key of this profile has not synced in or not been tried yet".into()));
    }
    keyring::set_profile_root(app.profile.root(), Some(&active_file_name()));
    for key in &set {
        if !key.is_archived() {
            syncsafe::write_atomic_synced(key.archived_path(), [])?;
        }
    }
    mint_content_key(app, &pass)
}

/// What follows a content key opening after the vault did (the recovery
/// words, a new key, or a key that synced in): the one-shot migrations run
/// again, the background seal among them, and so does orphan recovery; the
/// phone-sync listener comes up when it is wanted; `encryption:keys_changed`
/// tells the windows that sessions may have opened.
pub fn content_key_reopened<R: tauri::Runtime>(handle: &tauri::AppHandle<R>) {
    use tauri::{Emitter, Manager};
    {
        let app = handle.state::<AppState>();
        let vs = handle.state::<VaultState>();
        let emitter = handle.clone();
        crate::migrations::run_on_unlock(&app, &vs, move |progress| {
            if let Err(e) = emitter.emit("encryption:progress", progress) {
                log::warn!("seal: couldn't announce progress: {e:?}");
            }
        });
    }
    crate::commands::finalize::spawn_orphan_recovery(handle.clone());
    crate::sync::server::apply_sync_state(handle);
    crate::commands::library::invalidate_library_cache();
    if let Err(e) = handle.emit("encryption:keys_changed", ()) {
        log::warn!("content key: couldn't announce the change: {e:?}");
    }
}

/// [`recovery_words_restore_impl`], then [`content_key_reopened`].
pub fn recovery_words_restore<R: tauri::Runtime>(handle: &tauri::AppHandle<R>, words: &str) -> Result<()> {
    use tauri::Manager;
    {
        let app = handle.state::<AppState>();
        let vs = handle.state::<VaultState>();
        recovery_words_restore_impl(&app, &vs, words)?;
    }
    content_key_reopened(handle);
    Ok(())
}

/// [`content_key_start_new_impl`], then [`content_key_reopened`].
pub fn content_key_start_new<R: tauri::Runtime>(handle: &tauri::AppHandle<R>) -> Result<Vec<String>> {
    use tauri::Manager;
    let words = {
        let app = handle.state::<AppState>();
        let vs = handle.state::<VaultState>();
        content_key_start_new_impl(&app, &vs)?
    };
    content_key_reopened(handle);
    Ok(words)
}

/// [`open_synced_keys`], then [`encryption_status_impl`].
pub fn encryption_status<R: tauri::Runtime>(handle: &tauri::AppHandle<R>) -> EncryptionStatus {
    use tauri::Manager;
    open_synced_keys(handle);
    encryption_status_impl(&handle.state::<AppState>(), &handle.state::<VaultState>())
}

/// While the vault is open, tries every vault file that has synced in or
/// changed since the content keys were last opened ([`untried_vault`]).
/// When a key opened, [`content_key_reopened`] follows; when none did,
/// `encryption:keys_changed` still tells the windows the status may have
/// changed. Returns whether a key opened.
pub fn open_synced_keys<R: tauri::Runtime>(handle: &tauri::AppHandle<R>) -> bool {
    use tauri::{Emitter, Manager};
    let (tried, opened) = {
        let app = handle.state::<AppState>();
        let vs = handle.state::<VaultState>();
        if !vs.is_unlocked() || !untried_vault(&app) {
            return false;
        }
        match open_content_key_with(&app, &vs, false) {
            Ok((_, now)) => (true, now),
            Err(e) => {
                log::warn!("content key: {e:?}");
                (false, false)
            }
        }
    };
    if opened {
        content_key_reopened(handle);
    } else if tried {
        if let Err(e) = handle.emit("encryption:keys_changed", ()) {
            log::warn!("content key: couldn't announce the change: {e:?}");
        }
    }
    opened
}

/// How often [`spawn_key_watch`] looks for vault files that synced in.
const KEY_WATCH_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);

/// Starts, once per process, the thread that runs [`open_synced_keys`]
/// every [`KEY_WATCH_INTERVAL`].
pub fn spawn_key_watch<R: tauri::Runtime>(handle: tauri::AppHandle<R>) {
    static STARTED: std::sync::Once = std::sync::Once::new();
    STARTED.call_once(move || {
        let spawned = std::thread::Builder::new().name("content-key-watch".into()).spawn(move || loop {
            std::thread::sleep(KEY_WATCH_INTERVAL);
            open_synced_keys(&handle);
        });
        if let Err(e) = spawned {
            log::warn!("content key: watch thread not started: {e}");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::ProfileDir;
    use sealfile::{keyring, ContentPublic, ContentSecret, SealError, SessionKey};

    const PASS: &str = "correct-horse-battery-staple-extended-22";
    const PASS2: &str = "lantern-orchard-copper-violet-meadow-77";
    const INCORRECT: &str = "The recovery key you entered is incorrect.";

    /// The keyring is process-wide; these tests have it to themselves.
    fn alone() -> std::sync::RwLockWriteGuard<'static, ()> {
        crate::test_keys::exclusive()
    }

    struct Fixture {
        _dir: tempfile::TempDir,
        app: AppState,
        vs: VaultState,
    }

    fn fresh() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let app = AppState::new(ProfileDir::at(dir.path().join("daisy")).unwrap());
        keyring::lock();
        keyring::forget_content_public();
        Fixture { _dir: dir, app, vs: VaultState::new() }
    }

    fn session(app: &AppState, id: &str) -> PathBuf {
        let dir = app.profile.sessions_dir().join(id);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// The profile's one key.
    fn only_key(app: &AppState) -> keyset::KeyFiles {
        let set = keyset::list(app.profile.root());
        assert_eq!(set.len(), 1, "one key in the set");
        set.into_iter().next().unwrap()
    }

    fn key_paths(key: &keyset::KeyFiles) -> [PathBuf; 4] {
        [key.pub_path(), key.vault_path(), key.recovery_path(), key.words_path()]
    }

    fn status(f: &Fixture) -> String {
        encryption_status_impl(&f.app, &f.vs).content_key.to_string()
    }

    /// Init, then the content-key step `vault_opened` runs; returns the words.
    fn init_and_mint(f: &Fixture) -> Vec<String> {
        init_vault_impl(&f.app, &f.vs, PASS).unwrap();
        match open_content_key(&f.app, &f.vs).unwrap() {
            ContentKeyOpen::Minted(words) => words,
            _ => panic!("first unlock mints the content key"),
        }
    }

    fn relock(f: &Fixture, pass: &str) -> ContentKeyOpen {
        lock_vault_impl(&f.app, &f.vs).unwrap();
        assert!(!keyring::is_unlocked());
        unlock_vault_impl(&f.app, &f.vs, pass).unwrap();
        open_content_key(&f.app, &f.vs).unwrap()
    }

    #[test]
    fn first_unlock_creates_content_key_files() {
        let _g = alone();
        let f = fresh();
        let words = init_and_mint(&f);
        assert_eq!(words.len(), 5);
        let key = only_key(&f.app);
        assert!(!key.is_legacy());
        for p in key_paths(&key) {
            assert!(p.is_file(), "{} written", p.display());
        }
        assert!(keyring::is_unlocked());
        assert_eq!(status(&f), "ok");
        assert!(encryption_status_impl(&f.app, &f.vs).words_pending);
        assert_eq!(std::fs::read_to_string(active_path(&f.app)).unwrap(), key.fingerprint().unwrap());
        assert!(matches!(relock(&f, PASS), ContentKeyOpen::Opened));
        assert_eq!(only_key(&f.app).fingerprint(), key.fingerprint(), "never re-minted");
        assert_eq!(recovery_words_show_impl(&f.app, &f.vs, PASS).unwrap(), words);
    }

    #[test]
    fn mint_sets_words_pending_and_acknowledge_clears_it() {
        let _g = alone();
        let f = fresh();
        init_vault_impl(&f.app, &f.vs, PASS).unwrap();
        assert!(!encryption_status_impl(&f.app, &f.vs).words_pending);
        assert!(matches!(open_content_key(&f.app, &f.vs).unwrap(), ContentKeyOpen::Minted(_)));
        assert!(encryption_status_impl(&f.app, &f.vs).words_pending);
        // A relaunch before the words were seen still reports them pending.
        assert!(matches!(relock(&f, PASS), ContentKeyOpen::Opened));
        assert!(encryption_status_impl(&f.app, &f.vs).words_pending);
        recovery_words_acknowledge_impl(&f.app).unwrap();
        assert!(!encryption_status_impl(&f.app, &f.vs).words_pending);
        recovery_words_acknowledge_impl(&f.app).unwrap();
        // The marker earlier builds wrote counts, and goes with it.
        std::fs::write(f.app.profile.root().join(LEGACY_WORDS_PENDING), b"").unwrap();
        assert!(encryption_status_impl(&f.app, &f.vs).words_pending);
        recovery_words_acknowledge_impl(&f.app).unwrap();
        assert!(!encryption_status_impl(&f.app, &f.vs).words_pending);
    }

    #[test]
    fn unlock_after_passphrase_change_opens_content_key() {
        let _g = alone();
        let f = fresh();
        let words = init_and_mint(&f);
        let dir = session(&f.app, "daisy-1");
        let key = keyring::create_session_key(&dir).unwrap();
        change_vault_passphrase_impl(&f.app, &f.vs, PASS, PASS2).unwrap();
        assert!(matches!(relock(&f, PASS2), ContentKeyOpen::Opened));
        assert_eq!(keyring::session_key(&dir).unwrap(), key);
        assert_eq!(recovery_words_show_impl(&f.app, &f.vs, PASS2).unwrap(), words);
        assert!(recovery_words_show_impl(&f.app, &f.vs, PASS).is_err());
    }

    #[test]
    fn leaving_or_replacing_the_passphrase_needs_the_current_one() {
        let _g = alone();
        let f = fresh();
        let words = init_and_mint(&f);
        for current in [None, Some(""), Some("not the passphrase, not even close")] {
            for target in [None, Some(PASS2)] {
                let err = switch_vault_mode_impl(&f.app, &f.vs, current, target).unwrap_err();
                assert!(err.to_string().contains("wrong current passphrase"), "{current:?} -> {target:?}: {err}");
            }
        }
        assert_eq!(read_vault_kind(&f.app), "passphrase");
        assert!(matches!(relock(&f, PASS), ContentKeyOpen::Opened), "the vault still opens with its passphrase");
        assert_eq!(recovery_words_show_impl(&f.app, &f.vs, PASS).unwrap(), words);
        if crate::machine_id::machine_passphrase().is_empty() {
            return;
        }
        assert_eq!(switch_vault_mode_impl(&f.app, &f.vs, Some(PASS), None).unwrap(), "machine");
        assert_eq!(switch_vault_mode_impl(&f.app, &f.vs, None, Some(PASS2)).unwrap(), "passphrase", "a machine-mode vault needs no passphrase to add one");
    }

    #[test]
    fn a_chosen_passphrase_cannot_start_like_a_machine_key() {
        let _g = alone();
        let reserved = "daisy.v1.machine:a passphrase that looks like a machine key";
        let f = fresh();
        assert_eq!(init_vault_impl(&f.app, &f.vs, reserved).unwrap_err().to_string(), AppError::Config(RESERVED_PASSPHRASE.into()).to_string());
        assert!(!vault_path(&f.app).is_file(), "nothing is written");
        init_and_mint(&f);
        assert!(change_vault_passphrase_impl(&f.app, &f.vs, PASS, reserved).is_err());
        assert!(switch_vault_mode_impl(&f.app, &f.vs, Some(PASS), Some(reserved)).is_err());
        assert!(matches!(relock(&f, PASS), ContentKeyOpen::Opened), "the vault keeps its passphrase");
    }

    #[test]
    fn edited_vault_files_do_not_skip_the_passphrase() {
        let _g = alone();
        let f = fresh();
        init_and_mint(&f);
        std::fs::write(vault_kind_path(&f.app), "machine\n").unwrap();
        assert_eq!(vault_kind_impl(&f.app, &f.vs), "passphrase", "an open passphrase vault reads as one");
        assert!(recovery_words_show_impl(&f.app, &f.vs, "").is_err(), "no words without the passphrase");
        assert!(switch_vault_mode_impl(&f.app, &f.vs, None, None).is_err());
        // A vault file sealed under a passphrase of the editor's choosing.
        const FORGED: &str = "a passphrase the editor picked, long enough";
        let env = encrypt(&serde_json::to_vec(&DecryptedKeys::default()).unwrap(), FORGED).unwrap();
        std::fs::write(vault_path(&f.app), serde_json::to_vec(&env).unwrap()).unwrap();
        let err = switch_vault_mode_impl(&f.app, &f.vs, Some(FORGED), None).unwrap_err();
        assert!(err.to_string().contains("wrong current passphrase"), "{err}");
        let err = switch_vault_mode_impl(&f.app, &f.vs, Some(FORGED), Some(PASS2)).unwrap_err();
        assert!(err.to_string().contains("wrong current passphrase"), "{err}");
    }

    #[test]
    fn mode_switch_reseals_content_key() {
        let _g = alone();
        let f = fresh();
        let words = init_and_mint(&f);
        assert_eq!(switch_vault_mode_impl(&f.app, &f.vs, Some(PASS), Some(PASS2)).unwrap(), "passphrase");
        assert!(matches!(relock(&f, PASS2), ContentKeyOpen::Opened));
        assert_eq!(recovery_words_show_impl(&f.app, &f.vs, PASS2).unwrap(), words);

        if crate::machine_id::machine_passphrase().is_empty() {
            return;
        }
        assert_eq!(switch_vault_mode_impl(&f.app, &f.vs, Some(PASS2), None).unwrap(), "machine");
        lock_vault_impl(&f.app, &f.vs).unwrap();
        assert!(unlock_if_machine_mode_impl(&f.app, &f.vs).unwrap());
        assert!(matches!(open_content_key(&f.app, &f.vs).unwrap(), ContentKeyOpen::Opened));
        assert_eq!(recovery_words_show_impl(&f.app, &f.vs, "").unwrap(), words);
    }

    #[test]
    fn reset_keeps_content_files_and_restore_with_words_reopens() {
        let _g = alone();
        let f = fresh();
        let words = init_and_mint(&f);
        let dir = session(&f.app, "daisy-1");
        let key = keyring::create_session_key(&dir).unwrap();
        let files = only_key(&f.app);

        reset_vault_impl(&f.app, &f.vs).unwrap();
        for p in key_paths(&files) {
            assert!(p.is_file(), "{} kept through reset", p.display());
        }
        init_vault_impl(&f.app, &f.vs, PASS2).unwrap();
        assert!(matches!(open_content_key(&f.app, &f.vs).unwrap(), ContentKeyOpen::Unopenable));
        assert_eq!(status(&f), "unopenable");
        assert_eq!(only_key(&f.app).fingerprint(), files.fingerprint(), "never re-minted");

        let wrong = recovery_words_restore_impl(&f.app, &f.vs, "abandon zoo ladder act quantum").unwrap_err();
        assert_eq!(wrong.to_string(), format!("config: {INCORRECT}"));
        let off_list = recovery_words_restore_impl(&f.app, &f.vs, "nothing here is a list word").unwrap_err();
        assert_eq!(off_list.to_string(), format!("config: {INCORRECT}"));

        let typed = words.iter().map(|w| w.to_uppercase()).collect::<Vec<_>>().join(", ");
        recovery_words_restore_impl(&f.app, &f.vs, &typed).unwrap();
        assert_eq!(status(&f), "ok");
        assert_eq!(keyring::session_key(&dir).unwrap(), key);
        assert!(matches!(relock(&f, PASS2), ContentKeyOpen::Opened));
        assert_eq!(keyring::session_key(&dir).unwrap(), key);
        assert_eq!(recovery_words_show_impl(&f.app, &f.vs, PASS2).unwrap(), words);
        assert!(recovery_words_restore_impl(&f.app, &f.vs, &typed).is_err(), "the key is open already");
    }

    #[test]
    fn stale_content_vault_reports_unopenable_and_never_mints() {
        let _g = alone();
        let f = fresh();
        init_and_mint(&f);
        let files = only_key(&f.app);
        // An older build changes the passphrase and leaves the content files alone.
        let env: Envelope = serde_json::from_slice(&std::fs::read(vault_path(&f.app)).unwrap()).unwrap();
        let plain = decrypt(&env, PASS).unwrap();
        let rekeyed = encrypt(&plain, PASS2).unwrap();
        std::fs::write(vault_path(&f.app), serde_json::to_vec_pretty(&rekeyed).unwrap()).unwrap();

        assert!(matches!(relock(&f, PASS2), ContentKeyOpen::Unopenable));
        assert_eq!(status(&f), "unopenable");
        assert!(!keyring::is_unlocked());
        assert_eq!(only_key(&f.app).fingerprint(), files.fingerprint(), "never re-minted");
        // A passphrase change leaves the stale file as it is.
        change_vault_passphrase_impl(&f.app, &f.vs, PASS2, PASS2).unwrap();
        assert!(matches!(open_content_key(&f.app, &f.vs).unwrap(), ContentKeyOpen::Unopenable));
    }

    /// Another desktop's profile under `pass`, with its own content key.
    fn other_desktop_mints(pass: &str) -> (tempfile::TempDir, AppState, Vec<String>) {
        let td = tempfile::tempdir().unwrap();
        let other = AppState::new(ProfileDir::at(td.path().join("daisy")).unwrap());
        let vs = VaultState::new();
        keyring::lock();
        init_vault_impl(&other, &vs, pass).unwrap();
        let ContentKeyOpen::Minted(words) = open_content_key(&other, &vs).unwrap() else { panic!("mints") };
        keyring::lock();
        keyring::forget_content_public();
        (td, other, words)
    }

    /// The other desktop's key file `which`, arriving by sync.
    fn sync_key_file(from: &AppState, to: &AppState, which: fn(&keyset::KeyFiles) -> PathBuf) {
        let src = which(&only_key(from));
        std::fs::copy(&src, to.profile.root().join(src.file_name().unwrap())).unwrap();
    }

    fn other_public(other: &AppState) -> ContentPublic {
        only_key(other).public().unwrap()
    }

    #[test]
    fn a_session_wrapped_to_a_key_not_here_yet_does_not_stop_a_mint() {
        let _g = alone();
        let (_other_dir, other, _) = other_desktop_mints(PASS);
        let f = fresh();
        let dir = session(&f.app, "daisy-from-another-desktop");
        std::fs::write(dir.join("key.json"), sealfile::keyfile::wrap_session_key(&other_public(&other), &SessionKey::random()))
            .unwrap();
        init_vault_impl(&f.app, &f.vs, PASS).unwrap();
        assert!(matches!(open_content_key(&f.app, &f.vs).unwrap(), ContentKeyOpen::Minted(_)));
        assert!(matches!(keyring::session_key(&dir), Err(SealError::OtherContentKey)), "locked until its key arrives");
    }

    #[test]
    fn a_key_that_syncs_in_later_opens_beside_the_one_minted_here() {
        let _g = alone();
        let (_other_dir, other, _) = other_desktop_mints(PASS);
        let theirs = other_public(&other);
        let f = fresh();
        let dir = session(&f.app, "daisy-1");
        let their_key = SessionKey::random();
        std::fs::write(dir.join("key.json"), sealfile::keyfile::wrap_session_key(&theirs, &their_key)).unwrap();
        init_and_mint(&f);
        let mine = only_key(&f.app).public().unwrap();
        recovery_words_acknowledge_impl(&f.app).unwrap();

        // Their public key lands first: nothing to open yet.
        sync_key_file(&other, &f.app, keyset::KeyFiles::pub_path);
        assert_eq!(status(&f), "ok");
        assert_eq!(keyring::open_publics(), vec![mine]);
        sync_key_file(&other, &f.app, keyset::KeyFiles::vault_path);
        let (s, opened) = encryption_status_opening(&f.app, &f.vs);
        assert!(opened, "the status call opened their key");
        assert_eq!(s.content_key, "ok");
        assert!(!encryption_status_opening(&f.app, &f.vs).1, "only the call that opened it says so");
        assert_eq!(keyring::session_key(&dir).unwrap(), their_key);

        // The active key is the one of lowest fingerprint; a change marks its words pending here.
        let active = [mine, theirs].into_iter().min_by_key(keyset::fingerprint).unwrap();
        assert_eq!(keyring::content_public(), Some(active));
        assert_eq!(std::fs::read_to_string(active_path(&f.app)).unwrap(), keyset::fingerprint(&active));
        assert_eq!(encryption_status_impl(&f.app, &f.vs).words_pending, active != mine);
        assert!(matches!(relock(&f, PASS), ContentKeyOpen::Opened));
        assert_eq!(keyring::open_publics().len(), 2);
        assert_eq!(keyring::content_public(), Some(active));
    }

    #[test]
    fn a_public_key_without_its_vault_file_waits_for_sync() {
        let _g = alone();
        let (_other_dir, other, _) = other_desktop_mints(PASS);
        let f = fresh();
        sync_key_file(&other, &f.app, keyset::KeyFiles::pub_path);
        assert_eq!(status(&f), "missing", "with the vault locked");
        init_vault_impl(&f.app, &f.vs, PASS).unwrap();
        assert!(matches!(open_content_key(&f.app, &f.vs).unwrap(), ContentKeyOpen::Missing));
        assert_eq!(status(&f), "missing");
        assert_eq!(keyset::list(f.app.profile.root()).len(), 1, "nothing was minted");
        sync_key_file(&other, &f.app, keyset::KeyFiles::vault_path);
        assert_eq!(status(&f), "ok", "the status call opened the content key");
    }

    #[test]
    fn restore_while_waiting_writes_the_keys_files() {
        let _g = alone();
        let (_other_dir, other, words) = other_desktop_mints(PASS2);
        let f = fresh();
        sync_key_file(&other, &f.app, keyset::KeyFiles::recovery_path);
        init_vault_impl(&f.app, &f.vs, PASS).unwrap();
        assert!(matches!(open_content_key(&f.app, &f.vs).unwrap(), ContentKeyOpen::Missing));

        let wrong = recovery_words_restore_impl(&f.app, &f.vs, "abandon zoo ladder act quantum").unwrap_err();
        assert_eq!(wrong.to_string(), format!("config: {INCORRECT}"));
        recovery_words_restore_impl(&f.app, &f.vs, &words.join(" ")).unwrap();
        assert_eq!(status(&f), "ok");
        let key = only_key(&f.app);
        assert_eq!(std::fs::read(key.pub_path()).unwrap(), std::fs::read(only_key(&other).pub_path()).unwrap());
        assert!(matches!(relock(&f, PASS), ContentKeyOpen::Opened));
    }

    #[test]
    fn a_profile_with_unfingerprinted_key_files_opens_and_restores() {
        let _g = alone();
        let f = fresh();
        let (secret, public) = ContentSecret::generate();
        let words = recovery::generate_words();
        let phrase = words.join(" ");
        let root = f.app.profile.root();
        std::fs::write(root.join("content.vault.json"), seal_with_passphrase(&*secret.to_bytes(), PASS).unwrap()).unwrap();
        std::fs::write(root.join("content.words.vault.json"), seal_with_passphrase(phrase.as_bytes(), PASS).unwrap()).unwrap();
        std::fs::write(root.join("content.recovery.json"), recovery::wrap_with_words(&phrase, &secret.to_bytes())).unwrap();
        std::fs::write(root.join("content.pub"), format!("{}\n", B64.encode(public.0))).unwrap();
        let dir = session(&f.app, "daisy-1");
        let key = SessionKey::random();
        std::fs::write(dir.join("key.json"), sealfile::keyfile::wrap_session_key(&public, &key)).unwrap();

        init_vault_impl(&f.app, &f.vs, PASS).unwrap();
        assert!(matches!(open_content_key(&f.app, &f.vs).unwrap(), ContentKeyOpen::Opened));
        assert!(only_key(&f.app).is_legacy(), "nothing renames the files");
        assert_eq!(keyring::session_key(&dir).unwrap(), key);
        assert_eq!(recovery_words_show_impl(&f.app, &f.vs, PASS).unwrap(), words);

        reset_vault_impl(&f.app, &f.vs).unwrap();
        init_vault_impl(&f.app, &f.vs, PASS2).unwrap();
        assert!(matches!(open_content_key(&f.app, &f.vs).unwrap(), ContentKeyOpen::Unopenable));
        recovery_words_restore_impl(&f.app, &f.vs, &phrase).unwrap();
        assert!(matches!(relock(&f, PASS2), ContentKeyOpen::Opened));
        assert_eq!(keyring::session_key(&dir).unwrap(), key);
    }

    #[test]
    fn a_new_key_when_the_words_are_lost_and_the_old_words_found_later() {
        let _g = alone();
        let f = fresh();
        let old_words = init_and_mint(&f);
        let old = session(&f.app, "daisy-1");
        sealfile::fs::write(old.join("notes.md"), b"before the reset").unwrap();
        let old_public = only_key(&f.app).public().unwrap();

        reset_vault_impl(&f.app, &f.vs).unwrap();
        init_vault_impl(&f.app, &f.vs, PASS2).unwrap();
        assert!(matches!(open_content_key(&f.app, &f.vs).unwrap(), ContentKeyOpen::Unopenable));
        let new_words = content_key_start_new_impl(&f.app, &f.vs).unwrap();
        assert_ne!(new_words, old_words);
        assert!(content_key_start_new_impl(&f.app, &f.vs).is_err(), "only while no key is open");
        assert_eq!(status(&f), "ok");
        assert!(encryption_status_impl(&f.app, &f.vs).words_pending);
        assert_eq!(keyset::list(f.app.profile.root()).len(), 2, "the old key's files stay");
        let new_public = keyring::content_public().unwrap();
        assert_ne!(new_public, old_public);
        assert_eq!(recovery_words_show_impl(&f.app, &f.vs, PASS2).unwrap(), new_words);

        // The old session is locked; a new one records under the new key.
        assert_eq!(sealfile::fs::read(old.join("notes.md")).unwrap_err().kind(), std::io::ErrorKind::PermissionDenied);
        let fresh_session = session(&f.app, "daisy-2");
        sealfile::fs::write(fresh_session.join("notes.md"), b"after").unwrap();
        assert!(matches!(relock(&f, PASS2), ContentKeyOpen::Opened));
        assert_eq!(keyring::open_publics(), vec![new_public]);

        // The old words turn up.
        recovery_words_restore_impl(&f.app, &f.vs, &old_words.join(" ")).unwrap();
        assert_eq!(sealfile::fs::read(old.join("notes.md")).unwrap(), b"before the reset");
        assert!(matches!(relock(&f, PASS2), ContentKeyOpen::Opened));
        assert_eq!(keyring::open_publics().len(), 2, "the old key opens with the passphrase now");
        assert_eq!(sealfile::fs::read(old.join("notes.md")).unwrap(), b"before the reset");
        assert_eq!(sealfile::fs::read(fresh_session.join("notes.md")).unwrap(), b"after");
    }

    #[test]
    fn a_locked_machine_never_seals_to_a_key_a_new_one_replaced() {
        let _g = alone();
        // The old key must sort first for the lowest-fingerprint rule to pick it.
        let (f, old_words, old_public, new_public) = loop {
            let f = fresh();
            let old_words = init_and_mint(&f);
            let old_public = only_key(&f.app).public().unwrap();
            reset_vault_impl(&f.app, &f.vs).unwrap();
            init_vault_impl(&f.app, &f.vs, PASS2).unwrap();
            assert!(matches!(open_content_key(&f.app, &f.vs).unwrap(), ContentKeyOpen::Unopenable));
            content_key_start_new_impl(&f.app, &f.vs).unwrap();
            let new_public = keyring::content_public().unwrap();
            if keyset::fingerprint(&old_public) < keyset::fingerprint(&new_public) {
                break (f, old_words, old_public, new_public);
            }
        };
        let old_key = keyset::files_of(f.app.profile.root(), &old_public);
        assert!(old_key.is_archived());
        assert!(!keyset::files_of(f.app.profile.root(), &new_public).is_archived());
        content_key_start_new_impl(&f.app, &f.vs).unwrap_err();

        // A machine that never unlocked this profile starts a recording with the vault locked.
        lock_vault_impl(&f.app, &f.vs).unwrap();
        std::fs::remove_file(active_path(&f.app)).unwrap();
        keyring::forget_content_public();
        keyring::set_profile_root(f.app.profile.root(), Some(&active_file_name()));
        let rec = session(&f.app, "daisy-1700000500");
        let _pin = keyring::pin(&rec);
        sealfile::fs::write(rec.join("notes.md"), b"agenda").unwrap();
        let wrapped = sealfile::keyfile::wrapped_to(&std::fs::read(rec.join("key.json")).unwrap()).unwrap();
        assert_eq!(wrapped, new_public, "the archived key sorts first and is passed over");

        // A machine whose active file names the archived key passes it over too.
        std::fs::write(active_path(&f.app), keyset::fingerprint(&old_public)).unwrap();
        keyring::forget_content_public();
        let rec2 = session(&f.app, "daisy-1700000600");
        let _pin2 = keyring::pin(&rec2);
        sealfile::fs::write(rec2.join("notes.md"), b"agenda").unwrap();
        let wrapped = sealfile::keyfile::wrapped_to(&std::fs::read(rec2.join("key.json")).unwrap()).unwrap();
        assert_eq!(wrapped, new_public, "the active file names an archived key");

        // The old words turn up: the old key is current again.
        unlock_vault_impl(&f.app, &f.vs, PASS2).unwrap();
        assert!(matches!(open_content_key(&f.app, &f.vs).unwrap(), ContentKeyOpen::Opened));
        recovery_words_restore_impl(&f.app, &f.vs, &old_words.join(" ")).unwrap();
        assert!(!old_key.is_archived(), "restoring the words removes the marker");
        assert_eq!(keyring::content_public(), Some(old_public), "lowest open fingerprint");
    }

    #[test]
    fn a_key_that_opens_by_passphrase_loses_its_archived_marker_and_gets_its_public_key_file() {
        let _g = alone();
        let f = fresh();
        init_and_mint(&f);
        let key = only_key(&f.app);
        let public = key.public().unwrap();
        std::fs::write(key.archived_path(), b"").unwrap();
        std::fs::remove_file(key.pub_path()).unwrap();
        assert!(matches!(relock(&f, PASS), ContentKeyOpen::Opened), "opens by its fingerprint");
        assert!(!key.is_archived(), "it opened: it is current");
        assert_eq!(only_key(&f.app).public(), Some(public), "the public key file is written");

        // A locked machine wraps to it again.
        lock_vault_impl(&f.app, &f.vs).unwrap();
        keyring::forget_content_public();
        assert_eq!(keyring::content_public(), Some(public));
    }

    #[test]
    fn a_new_key_starts_only_while_every_key_is_there_and_tried() {
        let _g = alone();
        let (_other_dir, other, _) = other_desktop_mints(PASS2);
        let f = fresh();
        sync_key_file(&other, &f.app, keyset::KeyFiles::pub_path);
        init_vault_impl(&f.app, &f.vs, PASS).unwrap();
        assert!(matches!(open_content_key(&f.app, &f.vs).unwrap(), ContentKeyOpen::Missing));
        assert!(content_key_start_new_impl(&f.app, &f.vs).is_err(), "missing: the vault file has not synced in");
        assert!(!only_key(&f.app).is_archived());

        sync_key_file(&other, &f.app, keyset::KeyFiles::vault_path);
        assert!(content_key_start_new_impl(&f.app, &f.vs).is_err(), "the vault file that arrived is not tried yet");
        assert_eq!(status(&f), "unopenable", "tried by the status call");
        content_key_start_new_impl(&f.app, &f.vs).unwrap();
        assert_eq!(keyset::list(f.app.profile.root()).iter().filter(|k| k.is_archived()).count(), 1);
    }

    #[test]
    fn a_vault_file_replaced_after_it_was_tried_is_tried_again() {
        let _g = alone();
        let (_other_dir, other, _) = other_desktop_mints(PASS);
        let f = fresh();
        init_and_mint(&f);
        let theirs = only_key(&other);
        // Their vault file first arrives sealed under an old passphrase.
        let plain = open_with_passphrase(&theirs.vault_path(), PASS).unwrap();
        let ours = f.app.profile.root().join(theirs.vault_path().file_name().unwrap());
        std::fs::write(&ours, seal_with_passphrase(&plain, PASS2).unwrap()).unwrap();
        sync_key_file(&other, &f.app, keyset::KeyFiles::pub_path);
        let (_, opened) = encryption_status_opening(&f.app, &f.vs);
        assert!(!opened);
        assert!(!untried_vault(&f.app));
        assert_eq!(keyring::open_publics().len(), 1);

        // The resealed one replaces it.
        std::fs::write(&ours, std::fs::read(theirs.vault_path()).unwrap()).unwrap();
        assert!(untried_vault(&f.app), "a new version of the file");
        let (_, opened) = encryption_status_opening(&f.app, &f.vs);
        assert!(opened);
        assert_eq!(keyring::open_publics().len(), 2);
    }

    #[test]
    fn restoring_unfingerprinted_words_keeps_another_key_the_vault_file_holds() {
        let _g = alone();
        let f = fresh();
        let root = f.app.profile.root().to_path_buf();
        // The sync tool kept the vault and words files of one desktop and the recovery file of another.
        let (x, x_public) = ContentSecret::generate();
        let (y, y_public) = ContentSecret::generate();
        let (x_words, y_words) = (recovery::generate_words().join(" "), recovery::generate_words().join(" "));
        std::fs::write(root.join("content.vault.json"), seal_with_passphrase(&*x.to_bytes(), PASS).unwrap()).unwrap();
        std::fs::write(root.join("content.words.vault.json"), seal_with_passphrase(x_words.as_bytes(), PASS).unwrap()).unwrap();
        std::fs::write(root.join("content.recovery.json"), recovery::wrap_with_words(&y_words, &y.to_bytes())).unwrap();
        let (dx, dy) = (session(&f.app, "daisy-x"), session(&f.app, "daisy-y"));
        let (kx, ky) = (SessionKey::random(), SessionKey::random());
        std::fs::write(dx.join("key.json"), sealfile::keyfile::wrap_session_key(&x_public, &kx)).unwrap();
        std::fs::write(dy.join("key.json"), sealfile::keyfile::wrap_session_key(&y_public, &ky)).unwrap();

        init_vault_impl(&f.app, &f.vs, PASS).unwrap();
        assert!(matches!(open_content_key(&f.app, &f.vs).unwrap(), ContentKeyOpen::Opened));
        assert_eq!(keyring::open_publics(), vec![x_public]);
        recovery_words_restore_impl(&f.app, &f.vs, &y_words).unwrap();
        assert_eq!(keyring::session_key(&dy).unwrap(), ky);

        assert!(matches!(relock(&f, PASS), ContentKeyOpen::Opened));
        let mut open = keyring::open_publics();
        open.sort_by_key(keyset::fingerprint);
        let mut want = vec![x_public, y_public];
        want.sort_by_key(keyset::fingerprint);
        assert_eq!(open, want, "both keys open by passphrase");
        assert_eq!(keyring::session_key(&dx).unwrap(), kx);
        assert_eq!(keyring::session_key(&dy).unwrap(), ky);
    }

    #[test]
    fn restoring_words_with_no_public_key_file_moves_another_key_out_of_the_unfingerprinted_files() {
        let _g = alone();
        let f = fresh();
        let root = f.app.profile.root().to_path_buf();
        let (x, x_public) = ContentSecret::generate();
        let (y, y_public) = ContentSecret::generate();
        let (x_words, y_words) = (recovery::generate_words().join(" "), recovery::generate_words().join(" "));
        std::fs::write(root.join("content.vault.json"), seal_with_passphrase(&*x.to_bytes(), PASS).unwrap()).unwrap();
        std::fs::write(root.join("content.words.vault.json"), seal_with_passphrase(x_words.as_bytes(), PASS).unwrap()).unwrap();
        std::fs::write(root.join("content.recovery.json"), recovery::wrap_with_words(&y_words, &y.to_bytes())).unwrap();
        let legacy = keyset::list(&root).pop().unwrap();
        assert!(legacy.is_legacy() && legacy.public().is_none());

        // Restored before anything opened x (and wrote its public key file).
        init_vault_impl(&f.app, &f.vs, PASS).unwrap();
        recovery_words_restore_impl(&f.app, &f.vs, &y_words).unwrap();
        let moved = keyset::files_of(&root, &x_public);
        assert!(moved.vault_path().is_file() && moved.words_path().is_file() && moved.pub_path().is_file());
        assert_eq!(open_with_passphrase(&moved.words_path(), PASS).unwrap().as_slice(), x_words.as_bytes());
        assert_eq!(std::fs::read_to_string(root.join("content.pub")).unwrap().trim(), B64.encode(y_public.0));

        assert!(matches!(relock(&f, PASS), ContentKeyOpen::Opened));
        assert_eq!(keyring::open_publics().len(), 2, "x and y both open");
    }

    #[test]
    fn passphrase_change_cut_short_after_the_vault_still_opens() {
        let _g = alone();
        let f = fresh();
        let words = init_and_mint(&f);
        let key = only_key(&f.app);
        let before: Vec<Vec<u8>> = [key.vault_path(), key.words_path()].iter().map(|p| std::fs::read(p).unwrap()).collect();
        change_vault_passphrase_impl(&f.app, &f.vs, PASS, PASS2).unwrap();
        // The crash: keys.vault.json is under PASS2, the content files are
        // still under PASS and their re-sealed copies are staged beside them.
        for (path, old) in [key.vault_path(), key.words_path()].iter().zip(&before) {
            std::fs::rename(path, next_path(path)).unwrap();
            std::fs::write(path, old).unwrap();
        }
        assert!(matches!(relock(&f, PASS2), ContentKeyOpen::Opened));
        assert!(!next_path(&key.vault_path()).exists());
        assert!(!next_path(&key.words_path()).exists());
        assert_eq!(recovery_words_show_impl(&f.app, &f.vs, PASS2).unwrap(), words);
    }

    #[test]
    fn passphrase_change_cut_short_between_the_two_renames_moves_the_words() {
        let _g = alone();
        let f = fresh();
        let words = init_and_mint(&f);
        let key = only_key(&f.app);
        let old_words = std::fs::read(key.words_path()).unwrap();
        change_vault_passphrase_impl(&f.app, &f.vs, PASS, PASS2).unwrap();
        // The vault file moved; the words file did not.
        std::fs::rename(key.words_path(), next_path(&key.words_path())).unwrap();
        std::fs::write(key.words_path(), old_words).unwrap();
        assert!(matches!(relock(&f, PASS2), ContentKeyOpen::Opened));
        assert!(!next_path(&key.words_path()).exists());
        assert_eq!(recovery_words_show_impl(&f.app, &f.vs, PASS2).unwrap(), words);
    }

    #[test]
    fn key_json_for_other_content_key_is_unopenable() {
        let _g = alone();
        let f = fresh();
        init_and_mint(&f);
        let fp = only_key(&f.app).fingerprint();
        let (_, other) = ContentSecret::generate();
        let dir = session(&f.app, "daisy-2");
        std::fs::write(dir.join("key.json"), sealfile::keyfile::wrap_session_key(&other, &SessionKey::random())).unwrap();

        assert!(matches!(keyring::session_key(&dir), Err(SealError::OtherContentKey)));
        assert!(matches!(relock(&f, PASS), ContentKeyOpen::Opened));
        assert!(matches!(keyring::session_key(&dir), Err(SealError::OtherContentKey)));
        assert_eq!(only_key(&f.app).fingerprint(), fp, "never re-minted");
    }

    #[test]
    fn lock_keeps_pinned_session_key() {
        let _g = alone();
        let f = fresh();
        init_and_mint(&f);
        let a = session(&f.app, "daisy-a");
        let b = session(&f.app, "daisy-b");
        let ka = keyring::create_session_key(&a).unwrap();
        keyring::create_session_key(&b).unwrap();
        let guard = keyring::pin(&a);

        lock_vault_impl(&f.app, &f.vs).unwrap();
        assert_eq!(keyring::session_key(&a).unwrap(), ka);
        assert!(matches!(keyring::session_key(&b), Err(SealError::Locked)));
        drop(guard);
        assert!(matches!(keyring::session_key(&a), Err(SealError::Locked)));
    }
}
