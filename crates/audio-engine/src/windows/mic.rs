//! WASAPI microphone (capture-endpoint) device resolution.

use crate::error::{Error, Result};

use windows::Win32::Media::Audio::{eCapture, IMMDevice, DEVICE_STATE_ACTIVE};

use super::{create_enumerator, hash_device_id, pcwstr_to_string};

/// Walk the active capture endpoints and return the IMMDevice whose hashed
/// device ID matches `target_id`.
pub(super) fn find_capture_device(target_id: u32) -> Result<IMMDevice> {
    let enumerator = create_enumerator()?;
    let coll = unsafe {
        enumerator
            .EnumAudioEndpoints(eCapture, DEVICE_STATE_ACTIVE)
            .map_err(|e| Error::PipeWire(format!("EnumAudioEndpoints(eCapture): {e}")))?
    };
    let n = unsafe {
        coll.GetCount()
            .map_err(|e| Error::PipeWire(format!("GetCount: {e}")))?
    };
    for i in 0..n {
        let device = unsafe {
            coll.Item(i)
                .map_err(|e| Error::PipeWire(format!("Item({i}): {e}")))?
        };
        let pwstr = unsafe {
            device
                .GetId()
                .map_err(|e| Error::PipeWire(format!("GetId: {e}")))?
        };
        let id_str = pcwstr_to_string(windows::core::PCWSTR(pwstr.0 as *const u16));
        unsafe {
            windows::Win32::System::Com::CoTaskMemFree(Some(pwstr.0 as *const _));
        }
        if hash_device_id(&id_str) == target_id {
            return Ok(device);
        }
    }
    Err(Error::SourceNotFound(format!("mic id={target_id}")))
}
