// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Session keys derive from the canonical session id, so an import in
//! progress and the finished session agree on the key. With no content
//! public key held, the keyring reads the profile's. Cached keys go with
//! their folder.

use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use sealfile::keyfile::{derive_session_key, unwrap_session_key, wrap_session_key, wrapped_to};
use sealfile::keyring::{self, session_id};
use sealfile::{ContentSecret, SealError, SessionKey};

/// The keyring is process-wide; these tests take turns.
fn alone() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

#[test]
fn importing_folder_derives_from_the_session_id() {
    let _g = alone();
    assert_eq!(session_id(Path::new("/p/sessions/daisy-7")).unwrap(), "daisy-7");
    assert_eq!(session_id(Path::new("/p/sessions/.daisy-7.importing")).unwrap(), "daisy-7");
    assert_eq!(session_id(Path::new("/p/sessions/.importing")).unwrap(), ".importing");

    let dir = tempfile::tempdir().unwrap();
    let importing = dir.path().join(".daisy-7.importing");
    let secret = ContentSecret::from_bytes([11u8; 32]);
    let want = derive_session_key(&secret, "daisy-7");
    keyring::unlock(secret);
    assert_eq!(keyring::create_session_key(&importing).unwrap(), want);
    keyring::lock();
}

/// A locked keyring holding no content public key, reading `root`'s.
fn locked_with_profile(root: &Path) {
    keyring::lock();
    keyring::forget_content_public();
    keyring::set_profile_root(root, None);
}

fn session_with_key(root: &Path, id: &str, wrapped_to_key: &sealfile::ContentPublic) -> std::path::PathBuf {
    let dir = root.join("sessions").join(id);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("key.json"), wrap_session_key(wrapped_to_key, &SessionKey::random())).unwrap();
    dir
}

#[test]
fn content_public_is_learned_from_agreeing_session_keys() {
    let _g = alone();
    let td = tempfile::tempdir().unwrap();
    let (_, public) = ContentSecret::generate();
    session_with_key(td.path(), "daisy-1", &public);
    session_with_key(td.path(), "daisy-2", &public);
    locked_with_profile(td.path());

    let fresh = td.path().join("sessions/daisy-3");
    let _pin = keyring::pin(&fresh);
    keyring::create_session_key(&fresh).unwrap();
    assert_eq!(wrapped_to(&std::fs::read(fresh.join("key.json")).unwrap()).unwrap(), public);
    assert_eq!(keyring::content_public(), Some(public));
}

#[test]
fn disagreeing_session_keys_leave_no_content_public() {
    let _g = alone();
    let td = tempfile::tempdir().unwrap();
    session_with_key(td.path(), "daisy-1", &ContentSecret::generate().1);
    session_with_key(td.path(), "daisy-2", &ContentSecret::generate().1);
    locked_with_profile(td.path());
    let fresh = td.path().join("sessions/daisy-3");
    assert!(matches!(keyring::create_session_key(&fresh), Err(SealError::NoContentKey)));
    assert!(!fresh.join("key.json").exists());
}

#[test]
fn content_pub_arriving_later_is_read() {
    let _g = alone();
    let td = tempfile::tempdir().unwrap();
    locked_with_profile(td.path());
    assert_eq!(keyring::content_public(), None);
    let (_, public) = ContentSecret::generate();
    let text = {
        use base64::Engine as _;
        format!("{}\n", base64::engine::general_purpose::STANDARD.encode(public.0))
    };
    std::fs::write(td.path().join(keyring::CONTENT_PUB), text).unwrap();
    assert_eq!(keyring::content_public(), Some(public));
    std::fs::write(td.path().join(keyring::CONTENT_PUB), "not a key").unwrap();
    keyring::forget_content_public();
    assert_eq!(keyring::content_public(), None, "a content.pub that does not parse is no key");
}

#[test]
fn forget_drops_the_cached_key_of_a_replaced_folder() {
    let _g = alone();
    let td = tempfile::tempdir().unwrap();
    let secret = ContentSecret::generate().0;
    let public = secret.public();
    let dir = td.path().join("sessions/daisy-1");
    std::fs::create_dir_all(&dir).unwrap();
    keyring::lock();
    keyring::unlock(ContentSecret::from_bytes(*secret.to_bytes()));
    let old = keyring::create_session_key(&dir).unwrap();

    // The folder is deleted and another lands under the same name.
    std::fs::remove_dir_all(&dir).unwrap();
    std::fs::create_dir_all(&dir).unwrap();
    let replacement = SessionKey::random();
    std::fs::write(dir.join("key.json"), wrap_session_key(&public, &replacement)).unwrap();
    assert_eq!(keyring::session_key(&dir).unwrap(), old, "cached until forgotten");
    keyring::forget(&dir);
    assert_eq!(keyring::session_key(&dir).unwrap(), replacement);
    assert_eq!(unwrap_session_key(&secret, &std::fs::read(dir.join("key.json")).unwrap()).unwrap(), replacement);
    keyring::lock();
}

#[test]
fn a_key_created_while_locked_is_kept_only_while_pinned() {
    let _g = alone();
    let td = tempfile::tempdir().unwrap();
    keyring::lock();
    keyring::set_content_public(ContentSecret::generate().1);
    let loose = td.path().join("sessions/daisy-1");
    keyring::create_session_key(&loose).unwrap();
    assert!(matches!(keyring::session_key(&loose), Err(SealError::Locked)));

    let pinned = td.path().join("sessions/daisy-2");
    let pin = keyring::pin(&pinned);
    let key = keyring::create_session_key(&pinned).unwrap();
    assert_eq!(keyring::session_key(&pinned).unwrap(), key);
    drop(pin);
    assert!(matches!(keyring::session_key(&pinned), Err(SealError::Locked)));
}
