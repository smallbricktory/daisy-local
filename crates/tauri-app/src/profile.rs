//! Profile directory resolution (cross-platform) + override mechanism.

use directories::ProjectDirs;
use std::path::{Path, PathBuf};

/// Expand a leading `~/` or `~` in a path to the user's home directory.
/// Returns the input unchanged if it doesn't start with `~`.
pub fn expand_home(path: impl AsRef<Path>) -> PathBuf {
    let p = path.as_ref();
    let Some(s) = p.to_str() else { return p.to_path_buf() };
    if s == "~" {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home);
        }
    } else if let Some(rest) = s.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    p.to_path_buf()
}

/// The Daisy profile directory: home of all user data.
///
/// Resolution order:
/// 1. `DAISY_PROFILE_DIR` env var, if set
/// 2. settings.json `profile_dir` override (resolved by the caller)
/// 3. platform default (XDG/Library/AppData)
#[derive(Debug, Clone)]
pub struct ProfileDir {
    root: PathBuf,
}

impl ProfileDir {
    /// Resolve the platform-default profile dir. Creates it if missing.
    pub fn platform_default() -> std::io::Result<Self> {
        let pd = ProjectDirs::from("ai", "daisy", "Daisy")
            .ok_or_else(|| std::io::Error::new(
                std::io::ErrorKind::Other,
                "could not determine platform-default profile directory",
            ))?;
        let root = pd.data_dir().to_path_buf();
        syncsafe::create_dir_all(&root)?;
        syncsafe::create_dir_all(root.join("sessions"))?;
        Ok(Self { root })
    }

    /// Override with an explicit path. Creates the dir if missing.
    /// Expands `~` and `~/` prefixes to the user's home directory.
    pub fn at(root: impl AsRef<Path>) -> std::io::Result<Self> {
        let root = expand_home(root.as_ref());
        syncsafe::create_dir_all(root.join("sessions"))?;
        Ok(Self { root })
    }

    /// Resolve via the env var if set, else platform default.
    pub fn resolve() -> std::io::Result<Self> {
        if let Ok(s) = std::env::var("DAISY_PROFILE_DIR") {
            if !s.is_empty() {
                return Self::at(s);
            }
        }
        Self::platform_default()
    }

    /// Resolution order:
    /// 1. DAISY_PROFILE_DIR env override
    /// 2. bootstrap.json (created during first-run wizard)
    /// 3. None — caller should kick off the first-run wizard
    pub fn resolve_with_bootstrap() -> std::io::Result<Option<Self>> {
        if let Ok(s) = std::env::var("DAISY_PROFILE_DIR") {
            if !s.is_empty() {
                return Ok(Some(Self::at(s)?));
            }
        }
        match crate::bootstrap::Bootstrap::load()? {
            Some(b) => Ok(Some(Self::at(&b.profile_dir)?)),
            None => Ok(None),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn sessions_dir(&self) -> PathBuf {
        self.root.join("sessions")
    }
    pub fn settings_path(&self) -> PathBuf {
        self.root.join("settings.json")
    }
    pub fn models_dir(&self) -> PathBuf {
        self.root.join("models")
    }
    /// Canonical path for a Whisper GGML model file of the given size key
    /// (e.g. `"base.en"` → `ggml-base.en.bin`). Resolution order:
    ///   1. `$DAISY_WHISPER_MODEL_DIR/ggml-<size>.bin`, when that file exists.
    ///   2. `<profile>/models/ggml-<size>.bin`.
    /// The returned path may or may not exist on disk.
    pub fn whisper_model_path(&self, size: &str) -> PathBuf {
        let filename = format!("ggml-{size}.bin");
        if let Ok(dir) = std::env::var("DAISY_WHISPER_MODEL_DIR") {
            let bundled = PathBuf::from(dir).join(&filename);
            if bundled.is_file() {
                return bundled;
            }
        }
        self.models_dir().join(filename)
    }
    /// Resolves a session directory from an id. Only the final path component
    /// of `session_id` is used: ids containing `/`, `\`, `..`, or an absolute
    /// path collapse to a plain name under `sessions_dir`. An id with no
    /// usable component maps to the literal directory name `_invalid_`.
    pub fn session_path(&self, session_id: &str) -> PathBuf {
        let safe = std::path::Path::new(session_id)
            .file_name()
            .and_then(|s| s.to_str())
            .filter(|s| !s.is_empty() && *s != "." && *s != "..")
            .unwrap_or("_invalid_");
        self.sessions_dir().join(safe)
    }
    /// Directory where the logger writes its rotated log files. Created on
    /// demand by `logging::init`.
    pub fn logs_dir(&self) -> PathBuf {
        self.root.join("logs")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// Creates a ProfileDir rooted in a unique temp dir; the returned path is
    /// the caller's to clean up.
    fn temp_profile() -> (ProfileDir, PathBuf) {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let tmp = std::env::temp_dir()
            .join(format!("daisy-profile-test-{}-{}", std::process::id(), n));
        let _ = std::fs::remove_dir_all(&tmp);
        let profile = ProfileDir::at(&tmp).expect("ProfileDir::at should create the dir");
        (profile, tmp)
    }

    #[test]
    fn session_path_resolves_normal_id() {
        let (profile, tmp) = temp_profile();

        let got = profile.session_path("daisy-123");
        assert!(
            got.ends_with("sessions/daisy-123"),
            "expected ...sessions/daisy-123, got {got:?}"
        );
        assert_eq!(got, profile.sessions_dir().join("daisy-123"));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn session_path_blocks_traversal() {
        let (profile, tmp) = temp_profile();
        let base = profile.sessions_dir();
        // Each id must resolve under sessions_dir.
        for id in ["../../etc", "/etc", "..", ".", "a/b/c", "../secret", "/etc/passwd", ""] {
            let got = profile.session_path(id);
            assert!(
                got.starts_with(&base),
                "session_path({id:?}) = {got:?} escaped {base:?}"
            );
            // It never equals the sessions dir itself.
            assert_ne!(got, base, "session_path({id:?}) collapsed to the sessions dir");
        }

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
