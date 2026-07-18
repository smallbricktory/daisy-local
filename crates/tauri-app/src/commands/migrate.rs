//! Tauri command for migrating the profile directory to a new location.
//!
//! Files are copied recursively; bootstrap is updated only after all files
//! are copied successfully. Old data is left in place.

use crate::error::{AppError, Result};
use crate::profile::expand_home;
use std::path::Path;
use walkdir::WalkDir;

/// Copy every file under `src` into `dst`, preserving relative structure.
/// `dst` must exist and be empty before this is called.
fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    for entry in WalkDir::new(src).min_depth(1) {
        let entry = entry?;
        let rel = entry
            .path()
            .strip_prefix(src)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;
        let target = dst.join(rel);
        if entry.file_type().is_dir() {
            syncsafe::create_dir_all(&target)?;
        } else {
            if let Some(parent) = target.parent() {
                syncsafe::create_dir_all(parent)?;
            }
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

/// Check whether a directory is effectively empty (no children at all).
fn is_dir_empty(path: &Path) -> std::io::Result<bool> {
    let mut iter = std::fs::read_dir(path)?;
    Ok(iter.next().is_none())
}

pub fn move_profile_impl(old_root: &Path, new_path_raw: &Path) -> Result<()> {
    let new_root = expand_home(new_path_raw);

    // Reject same-path moves.
    let old_canonical = old_root
        .canonicalize()
        .unwrap_or_else(|_| old_root.to_path_buf());
    let new_canonical = if new_root.exists() {
        new_root.canonicalize().unwrap_or_else(|_| new_root.clone())
    } else {
        new_root.clone()
    };
    if old_canonical == new_canonical {
        return Err(AppError::Io(
            "destination is the same as the current profile directory".into(),
        ));
    }

    if !new_root.exists() {
        syncsafe::create_dir_all(&new_root)
            .map_err(|e| AppError::Io(format!("create {}: {e}", new_root.display())))?;
    }

    let empty = is_dir_empty(&new_root)
        .map_err(|e| AppError::Io(format!("read {}: {e}", new_root.display())))?;
    if !empty {
        return Err(AppError::Io(format!(
            "destination {} is not empty; choose an empty folder",
            new_root.display()
        )));
    }

    // On mid-copy failure both locations are left intact.
    copy_dir_recursive(old_root, &new_root)
        .map_err(|e| AppError::Io(format!("copy failed: {e}")))?;

    crate::bootstrap::Bootstrap::save(&new_root)
        .map_err(|e| AppError::Io(format!("update bootstrap: {e}")))?;

    Ok(())
}

/// What a candidate profile directory looks like, for the switch-profile
/// flow's confirm dialogs.
#[derive(serde::Serialize)]
pub struct ProfileProbe {
    /// Same directory as the running profile (after home expansion +
    /// canonicalization) — switching would be a no-op.
    pub is_current: bool,
    /// Carries a profile signature (vault or settings file).
    pub has_profile: bool,
    /// Absent or has no children at all.
    pub empty: bool,
}

pub fn probe_profile_dir_impl(current_root: &Path, path_raw: &Path) -> Result<ProfileProbe> {
    let root = expand_home(path_raw);
    let canon = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    let is_current = root.exists() && canon(&root) == canon(current_root);
    let has_profile = root.join("keys.vault.json").is_file() || root.join("settings.json").is_file();
    let empty = !root.exists()
        || is_dir_empty(&root).map_err(|e| AppError::Io(format!("read {}: {e}", root.display())))?;
    Ok(ProfileProbe { is_current, has_profile, empty })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write_file(base: &Path, rel: &str, content: &str) {
        let p = base.join(rel);
        if let Some(parent) = p.parent() {
            syncsafe::create_dir_all(parent).unwrap();
        }
        syncsafe::write(p, content).unwrap();
    }

    #[test]
    fn probe_classifies_candidate_dirs() {
        let cur = TempDir::new().unwrap();
        write_file(cur.path(), "settings.json", "{}");

        // Current dir: is_current, has_profile.
        let p = probe_profile_dir_impl(cur.path(), cur.path()).unwrap();
        assert!(p.is_current && p.has_profile && !p.empty);

        // Existing foreign profile (vault only).
        let other = TempDir::new().unwrap();
        write_file(other.path(), "keys.vault.json", "{}");
        let p = probe_profile_dir_impl(cur.path(), other.path()).unwrap();
        assert!(!p.is_current && p.has_profile && !p.empty);

        // Empty dir and a nonexistent dir both read as empty non-profiles.
        let empty = TempDir::new().unwrap();
        let p = probe_profile_dir_impl(cur.path(), empty.path()).unwrap();
        assert!(!p.is_current && !p.has_profile && p.empty);
        let p = probe_profile_dir_impl(cur.path(), &empty.path().join("new-sub")).unwrap();
        assert!(!p.is_current && !p.has_profile && p.empty);

        // Non-empty dir without profile signature.
        let mixed = TempDir::new().unwrap();
        write_file(mixed.path(), "notes.txt", "x");
        let p = probe_profile_dir_impl(cur.path(), mixed.path()).unwrap();
        assert!(!p.is_current && !p.has_profile && !p.empty);
    }

    #[test]
    fn copies_files_recursively() {
        let src = TempDir::new().unwrap();
        let dst = TempDir::new().unwrap();

        write_file(src.path(), "settings.json", r#"{"schema_version":1}"#);
        write_file(src.path(), "sessions/abc/manifest.json", "{}");
        write_file(src.path(), "keys.vault.json", "vault");

        copy_dir_recursive(src.path(), dst.path()).unwrap();

        assert!(dst.path().join("settings.json").exists());
        assert!(dst.path().join("sessions/abc/manifest.json").exists());
        assert!(dst.path().join("keys.vault.json").exists());
    }

    #[test]
    fn rejects_same_path() {
        let dir = TempDir::new().unwrap();
        let err = move_profile_impl(dir.path(), dir.path()).unwrap_err();
        assert!(err.to_string().contains("same as the current"));
    }

    #[test]
    fn rejects_non_empty_destination() {
        let src = TempDir::new().unwrap();
        let dst = TempDir::new().unwrap();
        write_file(dst.path(), "existing.txt", "occupied");

        let err = move_profile_impl(src.path(), dst.path()).unwrap_err();
        assert!(err.to_string().contains("not empty"));
    }
}
