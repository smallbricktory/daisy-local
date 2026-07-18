//! Profile binding.
//!
//! `binding.json` (in the profile root):
//!   { profile_id, owner, mac? }
//!     owner = "license:<email>"  → any install holding that license: allowed.
//!     owner = "install:<id>"     → only that install (mac verified with its
//!                                  machine-local key): allowed; else Foreign.

use crate::commands::license::{license_status_impl, InstallRecord, LicenseStatus};
use crate::error::{AppError, Result};
use crate::state::AppState;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::PathBuf;

#[derive(Serialize, Deserialize)]
struct BindingFile {
    profile_id: String,
    owner: String,
    #[serde(default)]
    mac: Option<String>,
}

#[derive(Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum BindingState {
    Ok,
    Foreign,
}

fn binding_path(app: &AppState) -> PathBuf {
    app.profile.root().join("binding.json")
}

fn mac_for(key_hex: &str, profile_id: &str) -> String {
    let mut h = Sha256::new();
    h.update(key_hex.as_bytes());
    h.update(b":");
    h.update(profile_id.as_bytes());
    h.finalize().iter().map(|x| format!("{x:02x}")).collect()
}

fn write_binding(path: &PathBuf, b: &BindingFile) -> Result<()> {
    let tmp = path.with_extension("json.tmp");
    syncsafe::write(&tmp, serde_json::to_vec_pretty(b).map_err(|e| AppError::Config(e.to_string()))?)
        .map_err(|e| AppError::Io(e.to_string()))?;
    syncsafe::rename(&tmp, path).map_err(|e| AppError::Io(e.to_string()))?;
    Ok(())
}

/// Check (and self-heal) the current profile's binding. Returns `Foreign` only
/// when an unlicensed install opens a profile claimed by a different install;
/// every other case returns `Ok` and writes/refreshes the binding.
pub fn profile_binding_check_impl(app: &AppState) -> Result<BindingState> {
    let rec = InstallRecord::load_or_create()?;
    let status = license_status_impl()?;
    let path = binding_path(app);
    let existing: Option<BindingFile> =
        syncsafe::read(&path).ok().and_then(|b| serde_json::from_slice(&b).ok());

    // Licensed: bind/refresh to the license and allow.
    if let LicenseStatus::Licensed { email, .. } = &status {
        let pid = existing
            .as_ref()
            .map(|b| b.profile_id.clone())
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        write_binding(&path, &BindingFile { profile_id: pid, owner: format!("license:{email}"), mac: None })?;
        return Ok(BindingState::Ok);
    }

    // Unlicensed (trial / expired).
    match existing {
        // Fresh or pre-binding profile: claim it for this install.
        None => {
            let pid = uuid::Uuid::new_v4().to_string();
            let mac = mac_for(&rec.key_hex, &pid);
            write_binding(
                &path,
                &BindingFile { profile_id: pid, owner: format!("install:{}", rec.install_id), mac: Some(mac) },
            )?;
            Ok(BindingState::Ok)
        }
        Some(b) => {
            // License-owned bindings are allowed in trial.
            if b.owner.starts_with("license:") {
                return Ok(BindingState::Ok);
            }
            let expect = mac_for(&rec.key_hex, &b.profile_id);
            let mine = b.owner == format!("install:{}", rec.install_id)
                && b.mac.as_deref() == Some(expect.as_str());
            Ok(if mine { BindingState::Ok } else { BindingState::Foreign })
        }
    }
}
