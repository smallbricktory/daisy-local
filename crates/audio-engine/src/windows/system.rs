// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! WASAPI render-endpoint resolution for system-loopback capture.
//!
//! The `IAudioClient` returned by these functions is the *render* endpoint;
//! the capture engine sets `AUDCLNT_STREAMFLAGS_LOOPBACK` when initializing
//! the client, which makes the render device produce the audio mix as input
//! data.

use crate::error::{Error, Result};
use crate::routing::{classify_windows_form_factor, OutputClass};

use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_EnumeratorName;
use windows::Win32::Media::Audio::{
    eConsole, eRender, IMMDevice, PKEY_AudioEndpoint_FormFactor,
};
use windows::Win32::System::Com::STGM_READ;
use windows::Win32::System::Variant::{VT_LPWSTR, VT_UI4};
use windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore;

use super::{create_enumerator, pcwstr_to_string};

/// Resolve the user's current default render endpoint (the speakers / output
/// device that Windows is sending mixed audio to right now): what the
/// `LOOPBACK_NODE` source resolves to at open.
pub(super) fn default_render_device() -> Result<IMMDevice> {
    let enumerator = create_enumerator()?;
    let device = unsafe {
        enumerator
            .GetDefaultAudioEndpoint(eRender, eConsole)
            .map_err(|e| Error::Wasapi(format!("GetDefaultAudioEndpoint: {e}")))?
    };
    Ok(device)
}

/// Classify the default render endpoint as headphone- vs speaker-class for AEC.
/// Bluetooth (most reliable headphone signal — form factor is often wrong for
/// BT) is detected via the device enumerator name; otherwise the endpoint's
/// `PKEY_AudioEndpoint_FormFactor` is mapped by `classify_windows_form_factor`.
pub(super) fn default_output_class() -> Result<OutputClass> {
    let device = default_render_device()?;
    let store: IPropertyStore = unsafe {
        device
            .OpenPropertyStore(STGM_READ)
            .map_err(|e| Error::Wasapi(format!("OpenPropertyStore: {e}")))?
    };

    // Bluetooth check first.
    if let Ok(prop) = unsafe { store.GetValue(&PKEY_Device_EnumeratorName) } {
        let vt = unsafe { prop.Anonymous.Anonymous.vt };
        if vt == VT_LPWSTR {
            let pwstr = unsafe { prop.Anonymous.Anonymous.Anonymous.pwszVal };
            if !pwstr.is_null() {
                let s = pcwstr_to_string(windows::core::PCWSTR(pwstr.0 as *const u16))
                    .to_ascii_uppercase();
                if s.contains("BTHENUM") || s.contains("BLUETOOTH") {
                    return Ok(OutputClass::Headphone);
                }
            }
        }
    }

    // Fall back to endpoint form factor.
    let prop = unsafe {
        store
            .GetValue(&PKEY_AudioEndpoint_FormFactor)
            .map_err(|e| Error::Wasapi(format!("GetValue(FormFactor): {e}")))?
    };
    let vt = unsafe { prop.Anonymous.Anonymous.vt };
    if vt != VT_UI4 {
        return Ok(OutputClass::Unknown);
    }
    let form_factor = unsafe { prop.Anonymous.Anonymous.Anonymous.ulVal };
    Ok(classify_windows_form_factor(form_factor))
}
