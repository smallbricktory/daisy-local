//! WASAPI source enumeration.
//!
//! Every active capture endpoint becomes a `Source { kind: Mic }` and every
//! active render endpoint becomes a `Source { kind: Monitor }` whose
//! `node_name` is `<device_id>.monitor`. The `Source.id` is a stable u32
//! hash of the device's WASAPI ID string (which is a GUID-form CLSID).
//!
//! Friendly names are pulled from `PKEY_Device_FriendlyName` via the
//! IPropertyStore the endpoint exposes; on failure the raw device ID is used
//! as the label.

use crate::error::{Error, Result};
use crate::source::{Source, SourceKind};

use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Media::Audio::{
    eCapture, eRender, IAudioClient, IMMDevice, DEVICE_STATE_ACTIVE,
};
use windows::Win32::System::Com::{CoTaskMemFree, CLSCTX_ALL, STGM_READ};
use windows::Win32::System::Variant::VT_LPWSTR;
use windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore;

use super::{create_enumerator, hash_device_id, pcwstr_to_string};

pub(super) fn enumerate_sources() -> Result<Vec<Source>> {
    let enumerator = create_enumerator()?;
    let mut sources = Vec::new();

    // Capture endpoints — microphones / line-ins.
    let cap = unsafe {
        enumerator
            .EnumAudioEndpoints(eCapture, DEVICE_STATE_ACTIVE)
            .map_err(|e| Error::PipeWire(format!("EnumAudioEndpoints(eCapture): {e}")))?
    };
    let n = unsafe {
        cap.GetCount()
            .map_err(|e| Error::PipeWire(format!("capture GetCount: {e}")))?
    };
    for i in 0..n {
        let device = unsafe {
            cap.Item(i)
                .map_err(|e| Error::PipeWire(format!("capture Item({i}): {e}")))?
        };
        if let Some(src) = build_source(&device, SourceKind::Mic) {
            sources.push(src);
        }
    }

    // Render endpoints — exposed as Monitor (the WASAPI loopback target).
    let ren = unsafe {
        enumerator
            .EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)
            .map_err(|e| Error::PipeWire(format!("EnumAudioEndpoints(eRender): {e}")))?
    };
    let n = unsafe {
        ren.GetCount()
            .map_err(|e| Error::PipeWire(format!("render GetCount: {e}")))?
    };
    for i in 0..n {
        let device = unsafe {
            ren.Item(i)
                .map_err(|e| Error::PipeWire(format!("render Item({i}): {e}")))?
        };
        if let Some(src) = build_source(&device, SourceKind::Monitor) {
            sources.push(src);
        }
    }

    // Synthetic "default render endpoint, captured in loopback" source: a
    // stable handle for "system audio" without committing to a specific
    // device id. `windows::resolve_device` short-circuits this node_name to
    // `GetDefaultAudioEndpoint(eRender, eConsole)`; the capture pipeline
    // grabs whatever Windows is currently routing sound through, including
    // after a mid-recording output switch. `VirtualSink::monitor_source_name()`
    // returns the same string on Windows.
    sources.push(Source {
        id: hash_device_id("wasapi-loopback"),
        node_name: "wasapi-loopback".to_string(),
        description: "Default output (loopback)".to_string(),
        kind: SourceKind::Monitor,
        default_sample_rate: 48_000,
        default_channels: 2,
    });

    Ok(sources)
}

fn build_source(device: &IMMDevice, kind: SourceKind) -> Option<Source> {
    let id_str = read_device_id(device)?;
    let id = hash_device_id(&id_str);
    let friendly = read_friendly_name(device).unwrap_or_else(|| id_str.clone());

    let (default_sample_rate, default_channels) =
        read_mix_format_defaults(device).unwrap_or((48_000, 2));

    let (node_name, description) = match kind {
        SourceKind::Mic => (id_str.clone(), friendly.clone()),
        SourceKind::Monitor => (
            format!("{id_str}.monitor"),
            format!("Monitor of {friendly}"),
        ),
    };

    Some(Source {
        id,
        node_name,
        description,
        kind,
        default_sample_rate,
        default_channels,
    })
}

/// Read the device ID string (CLSID-form GUID) and free the LPWSTR.
fn read_device_id(device: &IMMDevice) -> Option<String> {
    let pwstr = unsafe { device.GetId().ok()? };
    if pwstr.is_null() {
        return None;
    }
    let s = pcwstr_to_string(windows::core::PCWSTR(pwstr.0 as *const u16));
    unsafe {
        CoTaskMemFree(Some(pwstr.0 as *const _));
    }
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

fn read_friendly_name(device: &IMMDevice) -> Option<String> {
    let store: IPropertyStore = unsafe { device.OpenPropertyStore(STGM_READ).ok()? };
    let prop = unsafe { store.GetValue(&PKEY_Device_FriendlyName).ok()? };
    let vt = unsafe { prop.Anonymous.Anonymous.vt };
    if vt != VT_LPWSTR {
        return None;
    }
    let pwstr = unsafe { prop.Anonymous.Anonymous.Anonymous.pwszVal };
    if pwstr.is_null() {
        return None;
    }
    let s = pcwstr_to_string(windows::core::PCWSTR(pwstr.0 as *const u16));
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
    // PROPVARIANT::Drop will release the LPWSTR; nothing else to do.
}

fn read_mix_format_defaults(device: &IMMDevice) -> Option<(u32, u16)> {
    let client: IAudioClient = unsafe { device.Activate::<IAudioClient>(CLSCTX_ALL, None).ok()? };
    let fmt_ptr = unsafe { client.GetMixFormat().ok()? };
    let wfx = unsafe { *fmt_ptr };
    let sr = wfx.nSamplesPerSec;
    let ch = wfx.nChannels;
    unsafe {
        CoTaskMemFree(Some(fmt_ptr as *const _));
    }
    Some((sr, ch))
}
