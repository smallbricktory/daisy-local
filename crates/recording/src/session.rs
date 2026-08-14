//! Session-on-disk: directory layout + atomic manifest persistence.

use crate::error::{RecordingError, Result};
use crate::manifest::SessionManifest;
use std::path::{Path, PathBuf};

/// Directory of chunk `idx` under a session root: `<root>/chunks/NNNN`.
pub fn chunk_dir(root: &Path, idx: u32) -> PathBuf {
    root.join(format!("chunks/{idx:04}"))
}

/// Path of `name` inside chunk `idx`'s directory. Built from one
/// forward-slash literal, so with an empty `root` it yields the
/// manifest-relative form that serializes with `/` on every OS.
pub fn chunk_file(root: &Path, idx: u32, name: &str) -> PathBuf {
    root.join(format!("chunks/{idx:04}/{name}"))
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
    /// The on-disk `manifest.json` is the source of truth: the command layer
    /// (Tauri) patches it directly (title, tag_ids, meeting_id, notes link,
    /// recording_segments) and the recorder must not clobber those fields when
    /// it persists the fields it owns (chunk list, aec_mode, finalization).
    /// Reading-modifying-writing here keeps the in-memory copy in sync with
    /// disk while only mutating what the closure touches.
    ///
    /// A byte-equality check skips the atomic-rename + fsync when the
    /// closure's mutation leaves the serialized manifest unchanged.
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
            .last()
            .map(|c| c.index + 1)
            .unwrap_or(1);
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
