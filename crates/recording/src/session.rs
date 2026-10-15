// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Session-on-disk: directory layout + atomic manifest persistence.

use crate::error::{RecordingError, Result};
use crate::manifest::SessionManifest;
use std::path::{Path, PathBuf};

/// Directory of chunk `idx` under a session root: `<root>/chunks/NNNN`,
/// joined with the platform separator.
pub fn chunk_dir(root: &Path, idx: u32) -> PathBuf {
    root.join("chunks").join(format!("{idx:04}"))
}

/// Manifest-relative path of `name` in chunk `idx`: `chunks/NNNN/<name>`,
/// with `/` on every OS so a manifest reads the same everywhere. Resolve it
/// against a session root with [`syncsafe::join_rel`].
pub fn chunk_rel(idx: u32, name: &str) -> PathBuf {
    PathBuf::from(format!("chunks/{idx:04}/{name}"))
}

#[derive(Debug)]
pub struct Session {
    root: PathBuf,
    manifest: SessionManifest,
}

impl Session {
    /// Create a new session directory. Errors if `root` already exists.
    pub fn create(root: &Path, manifest: SessionManifest) -> Result<Self> {
        if root.exists() {
            return Err(RecordingError::SessionExists(root.to_path_buf()));
        }
        syncsafe::create_dir_all(root.join("chunks")).map_err(|e| RecordingError::Io {
            path: root.to_path_buf(),
            source: e,
        })?;
        let s = Self {
            root: root.to_path_buf(),
            manifest,
        };
        s.write_manifest()?;
        Ok(s)
    }

    /// Load an existing session by reading its manifest.
    pub fn load(root: &Path) -> Result<Self> {
        if !root.is_dir() {
            return Err(RecordingError::SessionMissing(root.to_path_buf()));
        }
        let mp = root.join("manifest.json");
        let bytes = syncsafe::read(&mp).map_err(|e| RecordingError::Io {
            path: mp.clone(),
            source: e,
        })?;
        let manifest: SessionManifest = serde_json::from_slice(&bytes)?;
        Ok(Self {
            root: root.to_path_buf(),
            manifest,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn manifest(&self) -> &SessionManifest {
        &self.manifest
    }

    /// Re-read the manifest from disk, apply `f`, and atomically persist
    /// only if the result differs from what was on disk.
    ///
    /// The on-disk `manifest.json` is the source of truth; only the fields the
    /// closure touches change.
    pub fn update_manifest<F: FnOnce(&mut SessionManifest)>(&mut self, f: F) -> Result<()> {
        let mp = self.root.join("manifest.json");
        let bytes = syncsafe::read(&mp).map_err(|e| RecordingError::Io {
            path: mp.clone(),
            source: e,
        })?;
        let mut manifest: SessionManifest = serde_json::from_slice(&bytes)?;
        f(&mut manifest);
        let new_bytes = serde_json::to_vec_pretty(&manifest)?;
        self.manifest = manifest;
        if new_bytes == bytes {
            return Ok(());
        }
        self.write_manifest_bytes(&new_bytes)
    }

    /// Allocate the next chunk directory (`chunks/NNNN`). Returns its index and
    /// absolute path; creates the directory. Does NOT mutate the manifest —
    /// the caller (Recorder) is responsible for appending a `ChunkManifest`.
    pub fn allocate_chunk_dir(&mut self) -> Result<(u32, PathBuf)> {
        let next = self
            .manifest
            .chunks
            .iter()
            .map(|c| c.index)
            .max()
            .map_or(1, |i| i.saturating_add(1));
        let dir = chunk_dir(&self.root, next);
        syncsafe::create_dir_all(&dir).map_err(|e| RecordingError::Io {
            path: dir.clone(),
            source: e,
        })?;
        Ok((next, dir))
    }

    fn write_manifest(&self) -> Result<()> {
        let bytes = serde_json::to_vec_pretty(&self.manifest)?;
        self.write_manifest_bytes(&bytes)
    }

    fn write_manifest_bytes(&self, bytes: &[u8]) -> Result<()> {
        let final_path = self.root.join("manifest.json");
        syncsafe::write_atomic_synced(&final_path, bytes).map_err(|e| RecordingError::Io {
            path: final_path,
            source: e,
        })?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunk_dir_joins_with_the_platform_separator() {
        let want: PathBuf = ["root", "chunks", "0007"].iter().collect();
        assert_eq!(chunk_dir(Path::new("root"), 7), want);
    }

    #[test]
    fn chunk_rel_is_slash_separated_on_every_os() {
        assert_eq!(chunk_rel(7, "mic.wav").to_str(), Some("chunks/0007/mic.wav"));
        let want: PathBuf = ["root", "chunks", "0007", "mic.wav"].iter().collect();
        assert_eq!(syncsafe::join_rel("root", chunk_rel(7, "mic.wav")), want);
    }
}
