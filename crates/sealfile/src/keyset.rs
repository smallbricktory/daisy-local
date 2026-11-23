// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! The profile's content key set: every content key pair whose files sit
//! in the profile folder. A key's files are named by its fingerprint
//! ([`fingerprint`]):
//!
//! - `content.<fp>.pub`: the public key, base64.
//! - `content.<fp>.vault.json`: the secret, sealed like the vault.
//! - `content.<fp>.recovery.json`: the secret under the recovery words.
//! - `content.<fp>.words.vault.json`: the recovery words, sealed like the
//!   vault.
//!
//! The same four files without a fingerprint (`content.pub`, …) are one key
//! of the set. A key superseded by a new one while it did not open carries
//! the marker `content.<fp>.archived` (`content.archived` without a
//! fingerprint) until its recovery words reopen it.

use std::path::{Path, PathBuf};

use sha2::Digest as _;

use crate::{ContentPublic, ContentSecret};

/// The name stem of the key files without a fingerprint.
const LEGACY_STEM: &str = "content";

/// The name endings of a key's four files, after its stem.
const SUFFIXES: [&str; 4] = ["pub", "vault.json", "recovery.json", "words.vault.json"];

/// The name ending of a key's archived marker, after its stem. The marker
/// alone does not make a key of the set.
const ARCHIVED: &str = "archived";

/// A content key's fingerprint: the first 16 hex digits of the SHA-256 of
/// its public key.
pub fn fingerprint(public: &ContentPublic) -> String {
    sha2::Sha256::digest(public.0)[..8].iter().map(|b| format!("{b:02x}")).collect()
}

/// One key of the set and where its files are.
#[derive(Debug, Clone)]
pub struct KeyFiles {
    root: PathBuf,
    /// `None` for the unfingerprinted names.
    named: Option<String>,
    public: Option<ContentPublic>,
}

impl KeyFiles {
    /// The key's fingerprint: from its file names, or from its public key
    /// for the unfingerprinted names. `None` when neither is known.
    pub fn fingerprint(&self) -> Option<String> {
        self.named.clone().or_else(|| self.public.as_ref().map(fingerprint))
    }

    /// The public key from the key's `.pub` file; `None` while that file is
    /// absent, does not parse, or names a key of another fingerprint.
    pub fn public(&self) -> Option<ContentPublic> {
        self.public
    }

    /// True for the files without a fingerprint in their names.
    pub fn is_legacy(&self) -> bool {
        self.named.is_none()
    }

    fn path(&self, suffix: &str) -> PathBuf {
        match &self.named {
            Some(fp) => self.root.join(format!("{LEGACY_STEM}.{fp}.{suffix}")),
            None => self.root.join(format!("{LEGACY_STEM}.{suffix}")),
        }
    }

    pub fn pub_path(&self) -> PathBuf {
        self.path(SUFFIXES[0])
    }

    pub fn vault_path(&self) -> PathBuf {
        self.path(SUFFIXES[1])
    }

    pub fn recovery_path(&self) -> PathBuf {
        self.path(SUFFIXES[2])
    }

    pub fn words_path(&self) -> PathBuf {
        self.path(SUFFIXES[3])
    }

    /// The marker of a key superseded while it did not open.
    pub fn archived_path(&self) -> PathBuf {
        self.path(ARCHIVED)
    }

    /// True while the key carries its archived marker.
    pub fn is_archived(&self) -> bool {
        self.archived_path().is_file()
    }

    /// True when `secret` is this key: its public half is the key's public
    /// key, or has the key's fingerprint while the public key is not known.
    /// Unfingerprinted files with no public key accept any secret.
    pub fn is_key_of(&self, secret: &ContentSecret) -> bool {
        let public = secret.public();
        match (&self.public, &self.named) {
            (Some(p), _) => *p == public,
            (None, Some(fp)) => fingerprint(&public) == *fp,
            (None, None) => true,
        }
    }
}

/// The files of the key `public`, under fingerprinted names in `root`.
pub fn files_of(root: &Path, public: &ContentPublic) -> KeyFiles {
    KeyFiles { root: root.to_path_buf(), named: Some(fingerprint(public)), public: Some(*public) }
}

/// Every key in the profile folder `root` that has at least one of its four
/// files there, ordered by fingerprint; for one fingerprint, the
/// fingerprinted names come first. Unfingerprinted files with no public key
/// come last.
pub fn list(root: &Path) -> Vec<KeyFiles> {
    let mut named = std::collections::BTreeSet::new();
    let mut legacy = false;
    for entry in std::fs::read_dir(root).into_iter().flatten().flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(rest) = name.strip_prefix(LEGACY_STEM).and_then(|r| r.strip_prefix('.')) else { continue };
        if SUFFIXES.contains(&rest) {
            legacy = true;
        } else if let Some((fp, suffix)) = rest.split_at_checked(16).and_then(|(fp, s)| Some((fp, s.strip_prefix('.')?))) {
            if SUFFIXES.contains(&suffix) && fp.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
                named.insert(fp.to_string());
            }
        }
    }
    let read_pub = |path: PathBuf| syncsafe::read_to_string(path).ok().and_then(|t| crate::keyfile::parse_content_pub(&t));
    let mut out: Vec<KeyFiles> = named
        .into_iter()
        .map(|fp| {
            let mut key = KeyFiles { root: root.to_path_buf(), named: Some(fp), public: None };
            key.public = read_pub(key.pub_path()).filter(|p| Some(fingerprint(p)) == key.named);
            key
        })
        .collect();
    if legacy {
        let mut key = KeyFiles { root: root.to_path_buf(), named: None, public: None };
        key.public = read_pub(key.pub_path());
        out.push(key);
    }
    out.sort_by_key(|k| (k.fingerprint().is_none(), k.fingerprint(), k.is_legacy()));
    out
}
