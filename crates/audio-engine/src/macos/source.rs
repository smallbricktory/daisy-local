//! macOS source enumeration: CoreAudio input devices → Mic sources, plus one
//! synthetic "System audio" Monitor (the Core Audio system tap).

use crate::{Source, SourceKind};
use crate::error::Result;
use std::os::raw::c_void;

/// Stable id for the synthetic system-audio source (matches the VirtualSink
/// sentinel "system-audio").
pub(crate) fn system_source_id() -> u32 {
    fnv1a("system-audio")
}

fn fnv1a(s: &str) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for b in s.bytes() {
        h ^= b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    h
}

pub(crate) fn list_sources_blocking() -> Result<Vec<Source>> {
    let mut out = enumerate_input_devices();
    out.push(Source {
        id: system_source_id(),
        node_name: "system-audio".to_string(),
        description: "System audio".to_string(),
        kind: SourceKind::Monitor,
        default_sample_rate: 16_000,
        default_channels: 1,
    });
    Ok(out)
}

// ── Minimal CoreAudio FFI for device enumeration ──────────────────────────

#[repr(C)]
struct AudioObjectPropertyAddress {
    selector: u32,
    scope: u32,
    element: u32,
}

const K_AUDIO_OBJECT_SYSTEM_OBJECT: u32 = 1;

fn fourcc(s: &[u8; 4]) -> u32 {
    u32::from_be_bytes(*s)
}

#[link(name = "CoreAudio", kind = "framework")]
extern "C" {
    fn AudioObjectGetPropertyDataSize(
        id: u32,
        addr: *const AudioObjectPropertyAddress,
        qd: u32,
        q: *const c_void,
        size: *mut u32,
    ) -> i32;
    fn AudioObjectGetPropertyData(
        id: u32,
        addr: *const AudioObjectPropertyAddress,
        qd: u32,
        q: *const c_void,
        size: *mut u32,
        data: *mut c_void,
    ) -> i32;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFStringGetCStringPtr(s: *const c_void, encoding: u32) -> *const std::os::raw::c_char;
    fn CFStringGetCString(
        s: *const c_void,
        buf: *mut std::os::raw::c_char,
        size: isize,
        encoding: u32,
    ) -> bool;
    fn CFRelease(cf: *const c_void);
}
const K_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;

fn enumerate_input_devices() -> Vec<Source> {
    // kAudioHardwarePropertyDevices = 'dev#', global scope = 'glob', elem 0.
    let addr = AudioObjectPropertyAddress {
        selector: fourcc(b"dev#"),
        scope: fourcc(b"glob"),
        element: 0,
    };
    let mut size: u32 = 0;
    let rc = unsafe {
        AudioObjectGetPropertyDataSize(
            K_AUDIO_OBJECT_SYSTEM_OBJECT,
            &addr,
            0,
            std::ptr::null(),
            &mut size,
        )
    };
    if rc != 0 || size == 0 {
        return Vec::new();
    }
    let count = size as usize / std::mem::size_of::<u32>();
    let mut ids = vec![0u32; count];
    let rc = unsafe {
        AudioObjectGetPropertyData(
            K_AUDIO_OBJECT_SYSTEM_OBJECT,
            &addr,
            0,
            std::ptr::null(),
            &mut size,
            ids.as_mut_ptr() as *mut c_void,
        )
    };
    if rc != 0 {
        return Vec::new();
    }
    ids.into_iter().filter_map(device_to_source).collect()
}

fn device_to_source(id: u32) -> Option<Source> {
    if input_channel_count(id) == 0 {
        return None;
    }
    // Skip aggregate devices: the system-tap ("DaisySystemTap"), the macOS
    // "CADefaultDeviceAggregate", and user multi-device aggregates all carry
    // input channels, but an aggregate cannot be set as an AVAudioEngine
    // input (AudioUnitSetProperty CurrentDevice →
    // kAudioUnitErr_InvalidPropertyValue / rc=-10851).
    if device_transport_type(id) == fourcc(b"grup") {
        return None;
    }
    let name = device_name(id).unwrap_or_else(|| format!("Input {id}"));
    Some(Source {
        id,
        node_name: name.clone(),
        description: name,
        kind: SourceKind::Mic,
        default_sample_rate: 48_000,
        default_channels: 1,
    })
}

/// kAudioDevicePropertyTransportType ('tran'). 'grup' = aggregate. Returns 0 if
/// the property is unavailable.
fn device_transport_type(id: u32) -> u32 {
    let addr = AudioObjectPropertyAddress {
        selector: fourcc(b"tran"),
        scope: fourcc(b"glob"),
        element: 0,
    };
    let mut val: u32 = 0;
    let mut size = std::mem::size_of::<u32>() as u32;
    if unsafe {
        AudioObjectGetPropertyData(
            id,
            &addr,
            0,
            std::ptr::null(),
            &mut size,
            &mut val as *mut u32 as *mut c_void,
        )
    } != 0
    {
        return 0;
    }
    val
}

fn input_channel_count(id: u32) -> u32 {
    // kAudioDevicePropertyStreamConfiguration = 'slay', input scope = 'inpt'.
    let addr = AudioObjectPropertyAddress {
        selector: fourcc(b"slay"),
        scope: fourcc(b"inpt"),
        element: 0,
    };
    let mut size: u32 = 0;
    if unsafe { AudioObjectGetPropertyDataSize(id, &addr, 0, std::ptr::null(), &mut size) } != 0
        || size == 0
    {
        return 0;
    }
    let mut buf = vec![0u8; size as usize];
    if unsafe {
        AudioObjectGetPropertyData(
            id,
            &addr,
            0,
            std::ptr::null(),
            &mut size,
            buf.as_mut_ptr() as *mut c_void,
        )
    } != 0
    {
        return 0;
    }
    // AudioBufferList: u32 mNumberBuffers, then [AudioBuffer{mNumberChannels:u32,
    // mDataByteSize:u32, mData:ptr}].
    if buf.len() < 4 {
        return 0;
    }
    let n = u32::from_ne_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
    let mut ch = 0u32;
    // AudioBufferList = { u32 mNumberBuffers; AudioBuffer mBuffers[1]; }.
    // AudioBuffer holds a pointer (8-byte aligned); mBuffers starts at
    // offset 8 — there are 4 padding bytes after mNumberBuffers, not
    // immediately at offset 4.
    let mut off = 8usize;
    let buffer_stride = std::mem::size_of::<u32>() * 2 + std::mem::size_of::<*const c_void>();
    for _ in 0..n {
        if off + 4 > buf.len() {
            break;
        }
        ch += u32::from_ne_bytes([buf[off], buf[off + 1], buf[off + 2], buf[off + 3]]);
        off += buffer_stride;
    }
    ch
}

fn device_name(id: u32) -> Option<String> {
    // kAudioObjectPropertyName = 'lnam', global scope. Returns a CFStringRef.
    let addr = AudioObjectPropertyAddress {
        selector: fourcc(b"lnam"),
        scope: fourcc(b"glob"),
        element: 0,
    };
    let mut size: u32 = std::mem::size_of::<*const c_void>() as u32;
    let mut cfstr: *const c_void = std::ptr::null();
    if unsafe {
        AudioObjectGetPropertyData(
            id,
            &addr,
            0,
            std::ptr::null(),
            &mut size,
            &mut cfstr as *mut _ as *mut c_void,
        )
    } != 0
        || cfstr.is_null()
    {
        return None;
    }
    let s = cfstring_to_string(cfstr);
    unsafe { CFRelease(cfstr) };
    s
}

fn cfstring_to_string(cfstr: *const c_void) -> Option<String> {
    unsafe {
        let p = CFStringGetCStringPtr(cfstr, K_CF_STRING_ENCODING_UTF8);
        if !p.is_null() {
            return std::ffi::CStr::from_ptr(p).to_str().ok().map(|s| s.to_string());
        }
        let mut buf = vec![0i8; 256];
        if CFStringGetCString(cfstr, buf.as_mut_ptr(), buf.len() as isize, K_CF_STRING_ENCODING_UTF8) {
            return std::ffi::CStr::from_ptr(buf.as_ptr())
                .to_str()
                .ok()
                .map(|s| s.to_string());
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_source_id_is_stable() {
        assert_eq!(system_source_id(), fnv1a("system-audio"));
        assert_ne!(system_source_id(), 0);
    }

    #[test]
    fn list_includes_system_monitor() {
        let srcs = list_sources_blocking().unwrap();
        assert!(srcs
            .iter()
            .any(|s| s.kind == SourceKind::Monitor && s.node_name == "system-audio"));
    }
}
