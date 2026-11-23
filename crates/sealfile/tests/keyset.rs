// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! A profile holding several content keys: the key set on disk, sessions
//! whose files are sealed under the keys of two desktops, and moving a
//! session onto one key.

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use base64::Engine as _;
use sealfile::keyfile::{derive_session_key, wrap_session_key, wrapped_to};
use sealfile::{fs, keyring, keyset, records, stream, ContentPublic, ContentSecret, SessionKey};

/// The keyring is process-wide; these tests take turns.
fn alone() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

fn pub_text(p: &ContentPublic) -> String {
    format!("{}\n", base64::engine::general_purpose::STANDARD.encode(p.0))
}

fn copy(secret: &ContentSecret) -> ContentSecret {
    ContentSecret::from_bytes(*secret.to_bytes())
}

#[test]
fn the_set_lists_fingerprinted_and_unfingerprinted_keys() {
    let td = tempfile::tempdir().unwrap();
    let root = td.path();
    let (_, a) = ContentSecret::generate();
    let (_, b) = ContentSecret::generate();
    let (_, legacy) = ContentSecret::generate();
    let fa = keyset::files_of(root, &a);
    std::fs::write(fa.pub_path(), pub_text(&a)).unwrap();
    std::fs::write(fa.vault_path(), b"{}").unwrap();
    // Only b's recovery file has synced in.
    std::fs::write(keyset::files_of(root, &b).recovery_path(), b"{}").unwrap();
    std::fs::write(root.join("content.pub"), pub_text(&legacy)).unwrap();
    // A pub whose name names another key is not trusted.
    let wrong = format!("content.{}.pub", keyset::fingerprint(&b));
    std::fs::write(root.join(&wrong), pub_text(&a)).unwrap();
    std::fs::write(root.join("content.0123.pub"), pub_text(&a)).unwrap();
    std::fs::write(root.join("content.sync-conflict-x.pub"), pub_text(&a)).unwrap();

    let set = keyset::list(root);
    let mut want = vec![keyset::fingerprint(&a), keyset::fingerprint(&b), keyset::fingerprint(&legacy)];
    want.sort();
    assert_eq!(set.iter().map(|k| k.fingerprint().unwrap()).collect::<Vec<_>>(), want);
    let by_fp = |p: &ContentPublic| set.iter().find(|k| k.fingerprint() == Some(keyset::fingerprint(p))).unwrap();
    assert_eq!(by_fp(&a).public(), Some(a));
    assert!(!by_fp(&a).is_legacy());
    assert_eq!(by_fp(&b).public(), None);
    assert!(by_fp(&legacy).is_legacy());
    assert_eq!(by_fp(&legacy).vault_path(), root.join("content.vault.json"));
    assert_eq!(keyset::fingerprint(&a).len(), 16);
}

#[test]
fn the_active_file_picks_the_key_new_sessions_wrap_to() {
    let _g = alone();
    let td = tempfile::tempdir().unwrap();
    let root = td.path();
    let mut keys: Vec<ContentPublic> = (0..2).map(|_| ContentSecret::generate().1).collect();
    keys.sort_by_key(keyset::fingerprint);
    for k in &keys {
        std::fs::write(keyset::files_of(root, k).pub_path(), pub_text(k)).unwrap();
    }
    assert_eq!(keyring::profile_content_public(root, Some("content.here.active")), Some(keys[0]), "lowest fingerprint");
    std::fs::write(root.join("content.here.active"), keyset::fingerprint(&keys[1])).unwrap();
    assert_eq!(keyring::profile_content_public(root, Some("content.here.active")), Some(keys[1]));
    assert_eq!(keyring::profile_content_public(root, None), Some(keys[0]));

    // An archived key is passed over, the active file's included.
    std::fs::write(keyset::files_of(root, &keys[1]).archived_path(), b"").unwrap();
    assert!(keyset::files_of(root, &keys[1]).is_archived());
    assert_eq!(keyset::list(root).len(), 2, "the marker makes no key");
    assert_eq!(keyring::profile_content_public(root, Some("content.here.active")), Some(keys[0]));
    std::fs::write(keyset::files_of(root, &keys[0]).archived_path(), b"").unwrap();
    assert_eq!(keyring::profile_content_public(root, None), Some(keys[0]), "every key archived: all count");
}

/// A session folder `id` under `root/sessions`.
fn session_dir(root: &Path, id: &str) -> PathBuf {
    let dir = root.join("sessions").join(id);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A session two desktops sealed while apart, each under its own content
/// key, as the sync tool left it: `key.json` from desktop A, a conflict
/// copy of desktop B's, and files from both.
struct Merged {
    _td: tempfile::TempDir,
    dir: PathBuf,
    a: ContentSecret,
    b: ContentSecret,
}

fn merged() -> Merged {
    let td = tempfile::tempdir().unwrap();
    let dir = session_dir(td.path(), "daisy-1700000000");
    let (a, b) = (ContentSecret::generate().0, ContentSecret::generate().0);
    let id = "daisy-1700000000";
    let (ka, kb) = (derive_session_key(&a, id), derive_session_key(&b, id));
    std::fs::write(dir.join("key.json"), wrap_session_key(&a.public(), &ka)).unwrap();
    std::fs::write(dir.join("key.sync-conflict-20261027-101010-ABCDEFG.json"), wrap_session_key(&b.public(), &kb)).unwrap();
    std::fs::write(dir.join("manifest.json"), stream::seal_whole(&ka, "manifest.json", b"{\"from\":\"a\"}")).unwrap();
    std::fs::write(dir.join("notes.md"), stream::seal_whole(&kb, "notes.md", b"from b")).unwrap();
    let log = format!(
        "{}\n{}\n{}\n",
        records::RECORDS_FIRST_LINE,
        records::seal_record(&kb, "live_transcript.jsonl", b"{\"n\":1}"),
        records::seal_record(&kb, "live_transcript.jsonl", b"{\"n\":2}"),
    );
    std::fs::write(dir.join("live_transcript.jsonl"), log).unwrap();
    Merged { _td: td, dir, a, b }
}

#[test]
fn a_session_merged_from_two_keys_reads_under_both() {
    let _g = alone();
    let m = merged();
    keyring::lock();
    keyring::unlock(copy(&m.a));
    assert_eq!(fs::read(m.dir.join("manifest.json")).unwrap(), b"{\"from\":\"a\"}");
    assert_eq!(fs::read(m.dir.join("notes.md")).unwrap_err().kind(), std::io::ErrorKind::InvalidData, "b's key is not held");

    assert!(keyring::add_secret(copy(&m.b)));
    assert!(!keyring::add_secret(copy(&m.b)), "held once");
    assert_eq!(fs::read(m.dir.join("notes.md")).unwrap(), b"from b");
    assert_eq!(fs::read_records(m.dir.join("live_transcript.jsonl")).unwrap(), vec![b"{\"n\":1}".to_vec(), b"{\"n\":2}".to_vec()]);
    // A write lands under the session's own key, beside b's lines.
    fs::append_record(m.dir.join("live_transcript.jsonl"), b"{\"n\":3}").unwrap();
    assert_eq!(fs::read_records(m.dir.join("live_transcript.jsonl")).unwrap().len(), 3);
    keyring::lock();
}

#[test]
fn a_key_made_while_locked_reads_from_a_conflict_copy_of_key_json() {
    let _g = alone();
    let td = tempfile::tempdir().unwrap();
    let dir = session_dir(td.path(), "daisy-2");
    let (a, b) = (ContentSecret::generate().0, ContentSecret::generate().0);
    let random = SessionKey::random();
    std::fs::write(dir.join("key.json"), wrap_session_key(&a.public(), &derive_session_key(&a, "daisy-2"))).unwrap();
    std::fs::write(dir.join("key-LAPTOP.json"), wrap_session_key(&b.public(), &random)).unwrap();
    std::fs::write(dir.join("notes.md"), stream::seal_whole(&random, "notes.md", b"locked note")).unwrap();
    keyring::lock();
    keyring::unlock(copy(&a));
    keyring::add_secret(copy(&b));
    assert_eq!(fs::read(dir.join("notes.md")).unwrap(), b"locked note");
    keyring::lock();
}

#[test]
fn a_session_under_a_key_not_held_is_permission_denied() {
    let _g = alone();
    let td = tempfile::tempdir().unwrap();
    let dir = session_dir(td.path(), "daisy-3");
    let (old, current) = (ContentSecret::generate().0, ContentSecret::generate().0);
    let key = derive_session_key(&old, "daisy-3");
    std::fs::write(dir.join("key.json"), wrap_session_key(&old.public(), &key)).unwrap();
    std::fs::write(dir.join("manifest.json"), stream::seal_whole(&key, "manifest.json", b"{}")).unwrap();
    keyring::lock();
    keyring::unlock(copy(&current));
    assert_eq!(fs::read(dir.join("manifest.json")).unwrap_err().kind(), std::io::ErrorKind::PermissionDenied);
    assert_eq!(fs::write(dir.join("notes.md"), b"x").unwrap_err().kind(), std::io::ErrorKind::PermissionDenied);
    assert!(!dir.join("notes.md").exists());
    keyring::add_secret(old);
    assert_eq!(fs::read(dir.join("manifest.json")).unwrap(), b"{}");
    keyring::lock();
}

#[test]
fn rekey_moves_every_file_onto_the_sessions_own_key_and_rewrap_onto_the_active_key() {
    let _g = alone();
    let m = merged();
    keyring::lock();
    // b is the active key: key.json (wrapped to a) moves to it.
    keyring::unlock(copy(&m.b));
    keyring::add_secret(copy(&m.a));
    let own = keyring::session_key(&m.dir).unwrap();
    assert_eq!(own, derive_session_key(&m.a, "daisy-1700000000"));

    assert!(!fs::needs_rekey(&m.dir.join("manifest.json")));
    for name in ["notes.md", "live_transcript.jsonl"] {
        let path = m.dir.join(name);
        assert!(fs::needs_rekey(&path), "{name}");
        let before = fs::read(&path).unwrap();
        assert!(fs::rekey(&path).unwrap(), "{name}");
        assert!(!fs::needs_rekey(&path), "{name}");
        assert_eq!(fs::read(&path).unwrap(), before, "{name} reads as it did");
    }
    assert!(!fs::rekey(&m.dir.join("manifest.json")).unwrap());
    assert!(keyring::wrapped_to_other_open_key(&m.dir));
    assert!(keyring::rewrap_session_key(&m.dir).unwrap());
    assert!(!keyring::rewrap_session_key(&m.dir).unwrap());
    assert_eq!(wrapped_to(&std::fs::read(m.dir.join("key.json")).unwrap()).unwrap(), m.b.public());

    // b alone now opens every file.
    keyring::lock();
    keyring::unlock(copy(&m.b));
    std::fs::remove_file(m.dir.join("key.sync-conflict-20261027-101010-ABCDEFG.json")).unwrap();
    assert_eq!(fs::read(m.dir.join("notes.md")).unwrap(), b"from b");
    assert_eq!(fs::read(m.dir.join("manifest.json")).unwrap(), b"{\"from\":\"a\"}");
    assert_eq!(fs::read_records(m.dir.join("live_transcript.jsonl")).unwrap().len(), 2);
    keyring::lock();
}

#[test]
fn rekey_keeps_a_torn_stream_torn() {
    use std::io::Write as _;
    let _g = alone();
    let td = tempfile::tempdir().unwrap();
    let dir = session_dir(td.path(), "daisy-4");
    let (a, b) = (ContentSecret::generate().0, ContentSecret::generate().0);
    std::fs::write(dir.join("key.json"), wrap_session_key(&a.public(), &derive_session_key(&a, "daisy-4"))).unwrap();
    let kb = derive_session_key(&b, "daisy-4");
    let samples: Vec<u8> = (0..40_000u32).flat_map(|i| (i as i16).to_le_bytes()).collect();
    let header = stream::StreamHeader::pcm(stream::PcmFormat { sample_rate: 16_000, channels: 1, bits_per_sample: 16 });
    let path = dir.join("chunks/0001/mic.wav");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut w = stream::StreamWriter::new(std::fs::File::create(&path).unwrap(), &kb, "chunks/0001/mic.wav", header).unwrap();
    w.write_all(&samples).unwrap();
    w.into_inner_unfinished().sync_all().unwrap();

    keyring::lock();
    keyring::unlock(copy(&a));
    keyring::add_secret(copy(&b));
    let read_all = || {
        let mut r = fs::open_stream(&path).unwrap();
        let mut out = Vec::new();
        r.read_to_end(&mut out).unwrap();
        (r.truncated(), out)
    };
    let before = read_all();
    assert!(before.0);
    assert!(fs::rekey(&path).unwrap());
    assert_eq!(read_all(), before);
    keyring::lock();
}

#[test]
fn a_pcm_stream_torn_in_its_first_segment_reads_as_empty() {
    let _g = alone();
    let td = tempfile::tempdir().unwrap();
    let dir = session_dir(td.path(), "daisy-5");
    keyring::lock();
    keyring::unlock(ContentSecret::generate().0);
    let header = stream::StreamHeader::pcm(stream::PcmFormat { sample_rate: 16_000, channels: 1, bits_per_sample: 16 });
    let wav = dir.join("mic.wav");
    let mut w = fs::create_stream(&wav, header).unwrap();
    std::io::Write::write_all(&mut w, &[1u8; 4000]).unwrap();
    w.finish().unwrap();
    // A crash part way through writing the first segment.
    let len = std::fs::metadata(&wav).unwrap().len();
    std::fs::OpenOptions::new().write(true).open(&wav).unwrap().set_len(len - 100).unwrap();
    let r = fs::open_stream(&wav).unwrap();
    assert!(r.is_empty() && r.truncated());

    // A file written whole that does not open is an error.
    fs::write(dir.join("notes.md"), b"short").unwrap();
    let len = std::fs::metadata(dir.join("notes.md")).unwrap().len();
    std::fs::OpenOptions::new().write(true).open(dir.join("notes.md")).unwrap().set_len(len - 3).unwrap();
    assert!(fs::open_stream(dir.join("notes.md")).is_err());
    keyring::lock();
}

#[test]
fn a_torn_first_segment_reads_as_empty_only_while_no_key_of_the_profile_is_missing() {
    let _g = alone();
    let td = tempfile::tempdir().unwrap();
    let root = td.path();
    let dir = session_dir(root, "daisy-6");
    let (a, b) = (ContentSecret::generate().0, ContentSecret::generate().0);
    std::fs::write(keyset::files_of(root, &a.public()).pub_path(), pub_text(&a.public())).unwrap();
    // b's key synced in its public key; its vault file has not arrived.
    std::fs::write(keyset::files_of(root, &b.public()).pub_path(), pub_text(&b.public())).unwrap();
    keyring::lock();
    keyring::unlock(copy(&a));
    keyring::set_profile_root(root, None);
    let header = stream::StreamHeader::pcm(stream::PcmFormat { sample_rate: 16_000, channels: 1, bits_per_sample: 16 });
    let wav = dir.join("mic.wav");
    let mut w = fs::create_stream(&wav, header).unwrap();
    std::io::Write::write_all(&mut w, &[1u8; 4000]).unwrap();
    w.finish().unwrap();
    let len = std::fs::metadata(&wav).unwrap().len();
    std::fs::OpenOptions::new().write(true).open(&wav).unwrap().set_len(len - 100).unwrap();

    assert!(!keyring::holds_every_profile_key());
    assert_eq!(fs::open_stream(&wav).err().map(|e| e.kind()), Some(std::io::ErrorKind::InvalidData), "b may open it");
    {
        let _pin = keyring::pin(&dir);
        assert!(fs::open_stream(&wav).unwrap().is_empty(), "a chunk this process is writing");
    }
    keyring::add_secret(copy(&b));
    assert!(keyring::holds_every_profile_key());
    assert!(fs::open_stream(&wav).unwrap().is_empty());
    keyring::lock();
}

#[test]
fn a_file_sealed_to_any_held_key_opens() {
    let _g = alone();
    let (a, b) = (ContentSecret::generate().0, ContentSecret::generate().0);
    let sealed = sealfile::keyfile::seal_to(&b.public(), "inbox", b"upload");
    keyring::lock();
    keyring::unlock(copy(&a));
    assert!(matches!(keyring::open_sealed_to("inbox", &sealed), Err(sealfile::SealError::OtherContentKey)));
    keyring::add_secret(b);
    assert_eq!(keyring::open_sealed_to("inbox", &sealed).unwrap(), b"upload");
    keyring::lock();
}
