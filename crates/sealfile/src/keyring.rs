// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! The process's keys: the content public key new session keys are wrapped
//! to, every content secret open while the vault is (the profile's key set,
//! [`crate::keyset`]), and the session keys unwrapped or created so far.
//!
//! With no content public key held, the keyring reads it from the
//! registered profile ([`set_profile_root`]) each time it needs one: the key
//! this machine last used, else the set's key of lowest fingerprint, or,
//! while the profile has no key files, the key every session `key.json` is
//! wrapped to when they all agree.
//!
//! A session's files open under its own key (its `key.json`) or, failing
//! that, under one of its other keys ([`other_session_keys`]): the key
//! derived from each open content secret, and the keys in sync conflict
//! copies of its `key.json`.
//!
//! [`lock`] drops the secrets and every session key that is not pinned. A
//! pinned session key (a recording or finalize in progress) lives until
//! its last [`PinGuard`] drops.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use crate::keyfile::{derive_session_key, unwrap_session_key, wrap_session_key};
use crate::{ContentPublic, ContentSecret, SealError, SessionKey};

/// The file in a session folder that holds its wrapped key.
pub const KEY_FILE: &str = "key.json";

#[derive(Default)]
struct Ring {
    public: Option<ContentPublic>,
    profile: Option<PathBuf>,
    /// The profile file naming the fingerprint of the key this machine
    /// wraps new session keys to.
    active_file: Option<String>,
    secrets: Vec<ContentSecret>,
    keys: HashMap<PathBuf, SessionKey>,
    pins: HashMap<PathBuf, usize>,
}

impl Ring {
    fn secret_of(&self, public: &ContentPublic) -> Option<&ContentSecret> {
        self.secrets.iter().find(|s| s.public() == *public)
    }
}

static RING: Mutex<Option<Ring>> = Mutex::new(None);

fn with_ring<T>(f: impl FnOnce(&mut Ring) -> T) -> T {
    let mut guard: MutexGuard<'_, Option<Ring>> = RING.lock().unwrap_or_else(|e| e.into_inner());
    f(guard.get_or_insert_with(Ring::default))
}

/// Records the content public key that new session keys are wrapped to.
pub fn set_content_public(p: ContentPublic) {
    with_ring(|r| r.public = Some(p));
}

/// Forgets the content public key; the next use reads it from the profile
/// again.
pub fn forget_content_public() {
    with_ring(|r| r.public = None);
}

/// The profile folder whose content public key the keyring reads while it
/// holds none, and the profile file in it that names the fingerprint of
/// the key this machine last wrapped new session keys to.
pub fn set_profile_root(root: &Path, active_file: Option<&str>) {
    with_ring(|r| {
        r.profile = Some(root.to_path_buf());
        r.active_file = active_file.map(str::to_string);
    });
}

/// The content public key: the one held, else the registered profile's
/// ([`profile_content_public`]), which is then held.
pub fn content_public() -> Option<ContentPublic> {
    let (held, profile, active_file) = with_ring(|r| (r.public, r.profile.clone(), r.active_file.clone()));
    if held.is_some() {
        return held;
    }
    let found = profile_content_public(&profile?, active_file.as_deref())?;
    with_ring(|r| *r.public.get_or_insert(found)).into()
}

/// The profile's content public key for new session keys, among the keys
/// of the set ([`crate::keyset::list`]) that are not archived (all of them
/// when every one is): the key whose fingerprint the profile file
/// `active_file` names, else the key of lowest fingerprint. A profile with
/// no key files falls back to [`sessions_content_public`]. `None` when the
/// set has files but no public key that parses.
pub fn profile_content_public(root: &Path, active_file: Option<&str>) -> Option<ContentPublic> {
    let set = crate::keyset::list(root);
    if set.is_empty() {
        return sessions_content_public(&root.join("sessions"));
    }
    let current: Vec<ContentPublic> = set.iter().filter(|k| !k.is_archived()).filter_map(|k| k.public()).collect();
    let publics = if current.is_empty() { set.iter().filter_map(|k| k.public()).collect() } else { current };
    let named = active_file
        .and_then(|name| syncsafe::read_to_string(root.join(name)).ok())
        .and_then(|fp| publics.iter().find(|p| crate::keyset::fingerprint(p) == fp.trim()).copied());
    named.or_else(|| publics.first().copied())
}

/// The content public key every session `key.json` under `sessions_dir` is
/// wrapped to; `None` when there is none, or when one does not read or is
/// wrapped to another key.
pub fn sessions_content_public(sessions_dir: &Path) -> Option<ContentPublic> {
    let mut agreed = None;
    for entry in std::fs::read_dir(sessions_dir).ok()?.flatten() {
        let path = entry.path().join(KEY_FILE);
        if !path.is_file() {
            continue;
        }
        let public = syncsafe::read(&path).ok().and_then(|b| crate::keyfile::wrapped_to(&b).ok())?;
        if *agreed.get_or_insert(public) != public {
            return None;
        }
    }
    agreed
}

/// The profile file that holds the content public key of the key set's
/// unfingerprinted files.
pub const CONTENT_PUB: &str = "content.pub";

/// Drops the cached key of the session folder `dir` (the folder was
/// deleted, or replaced by another).
pub fn forget(dir: &Path) {
    with_ring(|r| {
        r.keys.remove(dir);
    });
}

/// Holds `secret` as the only content secret until [`lock`]; its public
/// half becomes the content public key. Pinned session keys are kept.
pub fn unlock(secret: ContentSecret) {
    with_ring(|r| {
        r.public = Some(secret.public());
        r.secrets = vec![secret];
    });
}

/// Holds `secret` beside the content secrets already held, until [`lock`].
/// The content public key stays as it is. False when it was held already.
pub fn add_secret(secret: ContentSecret) -> bool {
    with_ring(|r| {
        if r.secret_of(&secret.public()).is_some() {
            return false;
        }
        r.secrets.push(secret);
        true
    })
}

/// Holds exactly `secrets` (one of each key) until [`lock`], in place of
/// those held before; session keys that are not pinned are dropped. The
/// content public key stays as it is.
pub fn hold_secrets(secrets: Vec<ContentSecret>) {
    with_ring(|r| {
        r.secrets.clear();
        for secret in secrets {
            if r.secret_of(&secret.public()).is_none() {
                r.secrets.push(secret);
            }
        }
        let pins = &r.pins;
        r.keys.retain(|dir, _| pins.contains_key(dir));
    });
}

/// The public halves of the content secrets held.
pub fn open_publics() -> Vec<ContentPublic> {
    with_ring(|r| r.secrets.iter().map(ContentSecret::public).collect())
}

/// Drops the content secrets and every session key that is not pinned.
pub fn lock() {
    with_ring(|r| {
        r.secrets.clear();
        let pins = &r.pins;
        r.keys.retain(|dir, _| pins.contains_key(dir));
    });
}

/// True while any content secret is held.
pub fn is_unlocked() -> bool {
    with_ring(|r| !r.secrets.is_empty())
}

/// The session's own key: from the cache, else unwrapped from its
/// `key.json` with the content secret it is wrapped to.
/// [`SealError::Locked`] when it is not cached and no content secret is
/// held; [`SealError::OtherContentKey`] when `key.json` is wrapped to a
/// content key whose secret is not held; [`SealError::Format`] when the
/// session has no `key.json`.
pub fn session_key(session_dir: &Path) -> Result<SessionKey, SealError> {
    if let Some(key) = with_ring(|r| r.keys.get(session_dir).cloned()) {
        return Ok(key);
    }
    let key_json = syncsafe::read(session_dir.join(KEY_FILE)).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => SealError::Format(format!("no {KEY_FILE} in {}", session_dir.display())),
        _ => SealError::Io(e),
    })?;
    let wrapped_to = crate::keyfile::wrapped_to(&key_json)?;
    with_ring(|r| {
        if let Some(key) = r.keys.get(session_dir) {
            return Ok(key.clone());
        }
        if r.secrets.is_empty() {
            return Err(SealError::Locked);
        }
        let secret = r.secret_of(&wrapped_to).ok_or(SealError::OtherContentKey)?;
        let key = unwrap_session_key(secret, &key_json)?;
        r.keys.insert(session_dir.to_path_buf(), key.clone());
        Ok(key)
    })
}

/// The session's keys other than its own ([`session_key`]): the key derived
/// from each content secret held, then the keys in the session's sync
/// conflict copies of `key.json` that a held secret unwraps. Empty while no
/// content secret is held.
pub fn other_session_keys(session_dir: &Path) -> Vec<SessionKey> {
    let own = session_key(session_dir).ok();
    let Ok(id) = session_id(session_dir) else { return Vec::new() };
    let copies: Vec<Vec<u8>> = std::fs::read_dir(session_dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| syncsafe::is_key_file_copy(&e.file_name().to_string_lossy()))
        .filter_map(|e| syncsafe::read(e.path()).ok())
        .collect();
    with_ring(|r| {
        let mut out: Vec<SessionKey> = Vec::new();
        let mut push = |key: SessionKey| {
            if own.as_ref() != Some(&key) && !out.contains(&key) {
                out.push(key);
            }
        };
        for secret in &r.secrets {
            push(derive_session_key(secret, &id));
        }
        for bytes in &copies {
            for secret in &r.secrets {
                if let Ok(key) = unwrap_session_key(secret, bytes) {
                    push(key);
                }
            }
        }
        out
    })
}

/// True when the session's `key.json` is wrapped to a content key other
/// than the content public key ([`content_public`]) and its secret is held.
pub fn wrapped_to_other_open_key(session_dir: &Path) -> bool {
    let Some(active) = content_public() else { return false };
    let Ok(wrapped_to) = syncsafe::read(session_dir.join(KEY_FILE)).map_err(SealError::Io).and_then(|b| crate::keyfile::wrapped_to(&b))
    else {
        return false;
    };
    wrapped_to != active && with_ring(|r| r.secret_of(&wrapped_to).is_some())
}

/// Rewrites the session's `key.json` with the same session key wrapped to
/// the content public key ([`content_public`]), when it is wrapped to
/// another content key whose secret is held. Holds the session's write
/// lock: the new `key.json` is written beside it as `key.json.rewrap.tmp`
/// and synced, read back and unwrapped with the held secret, and renamed
/// over it only when it unwraps to the session key; then the folder is
/// synced. Returns whether it was rewritten.
pub fn rewrap_session_key(session_dir: &Path) -> Result<bool, SealError> {
    rewrap_session_key_with(session_dir, &|| false)
}

/// [`rewrap_session_key`], calling `written` once the new `key.json` is
/// written and synced beside the old one; true from it stops the call
/// there with nothing cleaned up, as a kill would.
pub(crate) fn rewrap_session_key_with(session_dir: &Path, written: &dyn Fn() -> bool) -> Result<bool, SealError> {
    let _lock = crate::fs::lock_session(session_dir);
    if !wrapped_to_other_open_key(session_dir) {
        return Ok(false);
    }
    let active = content_public().ok_or(SealError::NoContentKey)?;
    let key = session_key(session_dir)?;
    let path = session_dir.join(KEY_FILE);
    let tmp = session_dir.join(REWRAP_TMP);
    let stopped = || SealError::Io(std::io::Error::new(std::io::ErrorKind::Interrupted, "stopped"));
    let done = (|| -> Result<(), SealError> {
        use std::io::Write as _;
        let mut f = syncsafe::retry(|| std::fs::File::create(&tmp))?;
        f.write_all(&wrap_session_key(&active, &key))?;
        f.sync_all()?;
        drop(f);
        if written() {
            return Err(stopped());
        }
        let back = syncsafe::read(&tmp)?;
        let unwrapped = with_ring(|r| r.secret_of(&active).map(|s| unwrap_session_key(s, &back)));
        if !matches!(unwrapped, Some(Ok(k)) if k == key) {
            return Err(SealError::Format(format!("the rewrapped {KEY_FILE} does not unwrap to the session key")));
        }
        syncsafe::rename(&tmp, &path)?;
        Ok(())
    })();
    match &done {
        Err(SealError::Io(e)) if e.kind() == std::io::ErrorKind::Interrupted => {}
        Err(_) => {
            let _ = std::fs::remove_file(&tmp);
        }
        Ok(()) => {}
    }
    done?;
    crate::fs::sync_parent(&path)?;
    Ok(true)
}

/// The name [`rewrap_session_key`] writes the new `key.json` under before
/// it replaces the old one.
pub const REWRAP_TMP: &str = "key.json.rewrap.tmp";

/// The session's key, creating it when the session has no `key.json` yet.
/// Created while the secret of the content public key is held, the key is
/// derived from it and the session id ([`session_id`]); otherwise it is
/// random.
/// Either way it is wrapped to the content public key ([`content_public`])
/// and written to `key.json`. An existing `key.json` is never replaced. A
/// key created while locked is kept only while the session is pinned.
pub fn create_session_key(session_dir: &Path) -> Result<SessionKey, SealError> {
    let path = session_dir.join(KEY_FILE);
    if !path.is_file() {
        content_public();
    }
    let created = with_ring(|r| -> Result<Option<SessionKey>, SealError> {
        let cached = r.keys.get(session_dir).cloned();
        if path.is_file() {
            return Ok(cached);
        }
        let public = r.public.ok_or(SealError::NoContentKey)?;
        let key = match (cached, r.secret_of(&public)) {
            (Some(key), _) => key,
            (None, Some(secret)) => derive_session_key(secret, &session_id(session_dir)?),
            (None, None) => SessionKey::random(),
        };
        syncsafe::create_dir_all(session_dir)?;
        syncsafe::write_atomic_synced(&path, wrap_session_key(&public, &key))?;
        if !r.secrets.is_empty() || r.pins.contains_key(session_dir) {
            r.keys.insert(session_dir.to_path_buf(), key.clone());
        }
        Ok(Some(key))
    })?;
    match created {
        Some(key) => Ok(key),
        None => session_key(session_dir),
    }
}

/// `plain` sealed to the content public key ([`crate::keyfile::seal_to`]),
/// for a file outside a session. [`SealError::NoContentKey`] when the
/// profile has none.
pub fn seal_to(label: &str, plain: &[u8]) -> Result<Vec<u8>, SealError> {
    let public = content_public().ok_or(SealError::NoContentKey)?;
    Ok(crate::keyfile::seal_to(&public, label, plain))
}

/// Opens a [`seal_to`] file with the content secret it was sealed to.
/// [`SealError::Locked`] while no secret is held,
/// [`SealError::OtherContentKey`] when that secret is not.
pub fn open_sealed_to(label: &str, sealed: &[u8]) -> Result<Vec<u8>, SealError> {
    with_ring(|r| {
        if r.secrets.is_empty() {
            return Err(SealError::Locked);
        }
        for secret in &r.secrets {
            match crate::keyfile::open_with(secret, label, sealed) {
                Err(SealError::OtherContentKey) => {}
                other => return other,
            }
        }
        Err(SealError::OtherContentKey)
    })
}

/// The session id a folder holds: its name, or `<id>` for an import in
/// progress (`.<id>.importing`).
pub fn session_id(session_dir: &Path) -> Result<String, SealError> {
    let name = session_dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .ok_or_else(|| SealError::Format("session folder has no name".into()))?;
    Ok(match name.strip_prefix('.').and_then(|n| n.strip_suffix(".importing")) {
        Some(id) if !id.is_empty() => id.to_string(),
        _ => name,
    })
}

/// True while the session's key is pinned ([`pin`]).
pub fn is_pinned(session_dir: &Path) -> bool {
    with_ring(|r| r.pins.contains_key(session_dir))
}

/// True when the secret of every key of the registered profile's set
/// ([`crate::keyset::list`]) is held; a key whose fingerprint is not known
/// is not. True while no profile is registered.
pub fn holds_every_profile_key() -> bool {
    let (profile, held) =
        with_ring(|r| (r.profile.clone(), r.secrets.iter().map(|s| crate::keyset::fingerprint(&s.public())).collect::<Vec<_>>()));
    let Some(root) = profile else { return true };
    crate::keyset::list(&root).iter().all(|k| k.fingerprint().is_some_and(|fp| held.contains(&fp)))
}

/// Keeps the session's cached key through [`lock`] until the guard drops.
pub fn pin(session_dir: &Path) -> PinGuard {
    let dir = session_dir.to_path_buf();
    with_ring(|r| *r.pins.entry(dir.clone()).or_insert(0) += 1);
    PinGuard { dir }
}

#[must_use = "the session key is kept only while the guard lives"]
pub struct PinGuard {
    dir: PathBuf,
}

impl Drop for PinGuard {
    fn drop(&mut self) {
        with_ring(|r| {
            let left = r.pins.get_mut(&self.dir).map(|n| {
                *n -= 1;
                *n
            });
            if left == Some(0) {
                r.pins.remove(&self.dir);
                if r.secrets.is_empty() {
                    r.keys.remove(&self.dir);
                }
            }
        });
    }
}
