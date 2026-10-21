// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Backend helpers for native file-picker flows. The backend opens the
//! picker itself, so a path reaches the filesystem only after the user chose
//! it in an OS dialog: exports write to the chosen destination, and imports
//! accept only the file the last open dialog returned. Directory picks are
//! kept per purpose, and each command that takes a directory accepts only the
//! last pick made for its purpose.

use crate::error::{AppError, Result};
use crate::state::VaultState;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Settings: the folder to move the profile into.
pub const PURPOSE_PROFILE_MOVE: &str = "profile-move";
/// Settings, Unlock and ProfileBlocked: an existing or fresh profile folder to switch to.
pub const PURPOSE_PROFILE_SWITCH: &str = "profile-switch";
/// Wizard: the folder to keep the new profile in.
pub const PURPOSE_PROFILE_NEW: &str = "profile-new";
/// Workflow editor: the folder a run-prompt step writes its artifact copy to.
pub const PURPOSE_WORKFLOW_OUTPUT: &str = "workflow-output";

const DIR_PURPOSES: [&str; 4] = [
    PURPOSE_PROFILE_MOVE,
    PURPOSE_PROFILE_SWITCH,
    PURPOSE_PROFILE_NEW,
    PURPOSE_WORKFLOW_OUTPUT,
];

/// Ok when `purpose` is one the directory picker serves.
pub fn validate_dir_purpose(purpose: &str) -> Result<()> {
    if DIR_PURPOSES.contains(&purpose) {
        Ok(())
    } else {
        Err(AppError::Config(format!("unknown folder purpose: {purpose}")))
    }
}

/// Ok when the folder picker may serve `purpose` in the vault's current state.
/// A locked vault serves only the profile-selection purposes.
pub fn validate_dir_pick(vs: &VaultState, purpose: &str) -> Result<()> {
    validate_dir_purpose(purpose)?;
    if vs.is_unlocked() || purpose == PURPOSE_PROFILE_SWITCH || purpose == PURPOSE_PROFILE_NEW {
        Ok(())
    } else {
        Err(AppError::VaultLocked)
    }
}

/// The path with symlinks and `..` resolved; the path itself when it cannot
/// be resolved.
fn canonical(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// Paths the user chose in an OS file dialog and that a later command may
/// read. Managed Tauri state.
#[derive(Default)]
pub struct DialogPicks {
    audio: Mutex<Option<PathBuf>>,
    dirs: Mutex<HashMap<String, PathBuf>>,
}

impl DialogPicks {
    /// Remembers the file the open dialog returned; replaces any earlier pick.
    pub fn record_audio(&self, path: PathBuf) {
        *self.audio.lock().unwrap() = Some(canonical(&path));
    }

    /// Ok only when `path` is the file the last dialog returned.
    pub fn check_audio(&self, path: &Path) -> Result<()> {
        match self.audio.lock().unwrap().as_deref() {
            Some(picked) if *picked == canonical(path) => Ok(()),
            _ => Err(AppError::Config(
                "the audio file was not chosen in the file picker".into(),
            )),
        }
    }

    /// Remembers the folder the dialog returned for `purpose`; replaces any
    /// earlier pick for that purpose.
    pub fn record_dir(&self, purpose: &str, path: PathBuf) {
        self.dirs.lock().unwrap().insert(purpose.to_string(), canonical(&path));
    }

    /// Ok only when `path` is the folder last picked for `purpose`.
    pub fn check_dir(&self, purpose: &str, path: &Path) -> Result<()> {
        match self.dirs.lock().unwrap().get(purpose) {
            Some(picked) if *picked == canonical(path) => Ok(()),
            _ => Err(AppError::Config(
                "the folder was not chosen in the folder picker".into(),
            )),
        }
    }

    /// Forgets the pick for `purpose`.
    pub fn clear_dir(&self, purpose: &str) {
        self.dirs.lock().unwrap().remove(purpose);
    }

    /// Forgets every pick, for when the vault closes.
    pub fn clear_all(&self) {
        *self.audio.lock().unwrap() = None;
        self.dirs.lock().unwrap().clear();
    }

    /// Forgets the pick after the import succeeded.
    pub fn clear_audio(&self) {
        *self.audio.lock().unwrap() = None;
    }
}

/// Refuses a file-dialog command while the vault is locked.
pub fn require_unlocked(vs: &VaultState) -> Result<()> {
    if vs.is_unlocked() {
        Ok(())
    } else {
        Err(AppError::VaultLocked)
    }
}

pub fn save_text_file_impl(path: &std::path::Path, contents: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            syncsafe::create_dir_all(parent)
                .map_err(|e| AppError::Config(format!("create parent dir: {e}")))?;
        }
    }
    syncsafe::write(path, contents).map_err(|e| AppError::Config(format!("write: {e}")))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_dialog_commands_refuse_a_locked_vault() {
        let vs = VaultState::default();
        assert!(matches!(require_unlocked(&vs), Err(AppError::VaultLocked)));
        *vs.keys.lock().unwrap() = Some(Default::default());
        assert!(require_unlocked(&vs).is_ok());
    }

    #[test]
    fn a_folder_pick_serves_only_its_own_purpose() {
        let picks = DialogPicks::default();
        let dir = Path::new("/tmp/out");
        assert!(picks.check_dir(PURPOSE_PROFILE_MOVE, dir).is_err());
        picks.record_dir(PURPOSE_PROFILE_MOVE, dir.to_path_buf());
        assert!(picks.check_dir(PURPOSE_PROFILE_MOVE, dir).is_ok());
        assert!(picks.check_dir(PURPOSE_PROFILE_SWITCH, dir).is_err());
        assert!(picks.check_dir(PURPOSE_PROFILE_MOVE, Path::new("/etc")).is_err());
        picks.clear_dir(PURPOSE_PROFILE_MOVE);
        assert!(picks.check_dir(PURPOSE_PROFILE_MOVE, dir).is_err());
        assert!(validate_dir_purpose("anything-else").is_err());
        assert!(validate_dir_purpose(PURPOSE_WORKFLOW_OUTPUT).is_ok());
    }

    #[test]
    fn a_locked_vault_serves_only_profile_selection_picks() {
        let vs = VaultState::default();
        assert!(validate_dir_pick(&vs, PURPOSE_PROFILE_SWITCH).is_ok());
        assert!(validate_dir_pick(&vs, PURPOSE_PROFILE_NEW).is_ok());
        assert!(matches!(
            validate_dir_pick(&vs, PURPOSE_PROFILE_MOVE),
            Err(AppError::VaultLocked)
        ));
        assert!(matches!(
            validate_dir_pick(&vs, PURPOSE_WORKFLOW_OUTPUT),
            Err(AppError::VaultLocked)
        ));
        assert!(validate_dir_pick(&vs, "bogus").is_err());
        *vs.keys.lock().unwrap() = Some(Default::default());
        assert!(validate_dir_pick(&vs, PURPOSE_PROFILE_MOVE).is_ok());
        assert!(validate_dir_pick(&vs, PURPOSE_WORKFLOW_OUTPUT).is_ok());
    }

    #[test]
    fn clearing_all_picks_forgets_files_and_folders() {
        let picks = DialogPicks::default();
        let dir = tempfile::tempdir().unwrap();
        picks.record_dir(PURPOSE_PROFILE_MOVE, dir.path().to_path_buf());
        picks.record_audio(dir.path().join("a.wav"));
        picks.clear_all();
        assert!(picks.check_dir(PURPOSE_PROFILE_MOVE, dir.path()).is_err());
        assert!(picks.check_audio(&dir.path().join("a.wav")).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn picks_compare_canonical_paths() {
        let picks = DialogPicks::default();
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        std::fs::create_dir(&real).unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        picks.record_dir(PURPOSE_PROFILE_MOVE, link.clone());
        assert!(picks.check_dir(PURPOSE_PROFILE_MOVE, &real).is_ok());
        assert!(picks.check_dir(PURPOSE_PROFILE_MOVE, &link.join("..").join("real")).is_ok());
        assert!(picks.check_dir(PURPOSE_PROFILE_MOVE, dir.path()).is_err());
    }

    #[test]
    fn only_the_last_picked_audio_path_is_accepted() {
        let picks = DialogPicks::default();
        assert!(picks.check_audio(Path::new("/tmp/a.wav")).is_err());
        picks.record_audio(PathBuf::from("/tmp/a.wav"));
        assert!(picks.check_audio(Path::new("/tmp/a.wav")).is_ok());
        assert!(picks.check_audio(Path::new("/etc/passwd")).is_err());
        picks.record_audio(PathBuf::from("/tmp/b.wav"));
        assert!(picks.check_audio(Path::new("/tmp/a.wav")).is_err());
        picks.clear_audio();
        assert!(picks.check_audio(Path::new("/tmp/b.wav")).is_err());
    }
}
