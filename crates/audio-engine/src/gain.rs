//! Per-OS microphone input-gain get/set, keyed by the same `source_id` capture
//! uses. Used by the mic auto-gain loop to lower a clipping mic and restore it
//! on stop. Best-effort: returns `None`/`false` when a device doesn't expose a
//! settable input volume (auto-gain then just no-ops — recording continues).

/// Current input gain (0..1), or `None` if unknown / not exposed.
pub fn input_gain(source_id: u32) -> Option<f32> {
    imp::input_gain(source_id)
}

/// Set input gain (0..1). Returns true if it was applied.
pub fn set_input_gain(source_id: u32, scalar: f32) -> bool {
    imp::set_input_gain(source_id, scalar.clamp(0.0, 1.0))
}

// ── Linux: wpctl shell-out ────────────────────────────────────────────────────
#[cfg(target_os = "linux")]
mod imp {
    pub fn input_gain(source_id: u32) -> Option<f32> {
        let out = std::process::Command::new("wpctl")
            .args(["get-volume", &source_id.to_string()])
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        // "Volume: 0.50" (may trail "[MUTED]").
        let s = String::from_utf8_lossy(&out.stdout);
        s.split_whitespace().nth(1)?.parse::<f32>().ok()
    }

    pub fn set_input_gain(source_id: u32, scalar: f32) -> bool {
        std::process::Command::new("wpctl")
            .args(["set-volume", &source_id.to_string(), &format!("{scalar:.3}")])
            .status()
            .map(|st| st.success())
            .unwrap_or(false)
    }
}

// ── macOS: CoreAudio kAudioDevicePropertyVolumeScalar, input scope ────────────
#[cfg(target_os = "macos")]
mod imp {
    use std::os::raw::c_void;

    #[repr(C)]
    struct Addr {
        selector: u32,
        scope: u32,
        element: u32,
    }
    fn fourcc(s: &[u8; 4]) -> u32 {
        u32::from_be_bytes(*s)
    }

    #[link(name = "CoreAudio", kind = "framework")]
    extern "C" {
        fn AudioObjectHasProperty(id: u32, addr: *const Addr) -> bool;
        fn AudioObjectIsPropertySettable(id: u32, addr: *const Addr, out: *mut bool) -> i32;
        fn AudioObjectGetPropertyData(
            id: u32,
            addr: *const Addr,
            qd: u32,
            q: *const c_void,
            size: *mut u32,
            data: *mut c_void,
        ) -> i32;
        fn AudioObjectSetPropertyData(
            id: u32,
            addr: *const Addr,
            qd: u32,
            q: *const c_void,
            size: u32,
            data: *const c_void,
        ) -> i32;
    }

    // Element 0 = master; 1.. = per-channel fallback when there's no master.
    const ELEMENTS: [u32; 3] = [0, 1, 2];

    pub fn input_gain(id: u32) -> Option<f32> {
        for &element in &ELEMENTS {
            let a = Addr { selector: fourcc(b"volm"), scope: fourcc(b"inpt"), element };
            unsafe {
                if !AudioObjectHasProperty(id, &a) {
                    continue;
                }
                let mut v: f32 = 0.0;
                let mut size = 4u32;
                if AudioObjectGetPropertyData(id, &a, 0, std::ptr::null(), &mut size, &mut v as *mut f32 as *mut c_void) == 0 {
                    return Some(v.clamp(0.0, 1.0));
                }
            }
        }
        None
    }

    pub fn set_input_gain(id: u32, scalar: f32) -> bool {
        let mut applied = false;
        for &element in &ELEMENTS {
            let a = Addr { selector: fourcc(b"volm"), scope: fourcc(b"inpt"), element };
            unsafe {
                if !AudioObjectHasProperty(id, &a) {
                    continue;
                }
                let mut settable = false;
                if AudioObjectIsPropertySettable(id, &a, &mut settable) != 0 || !settable {
                    continue;
                }
                if AudioObjectSetPropertyData(id, &a, 0, std::ptr::null(), 4, &scalar as *const f32 as *const c_void) == 0 {
                    applied = true;
                    if element == 0 {
                        break; // master set; no need to touch per-channel
                    }
                }
            }
        }
        applied
    }
}

// ── Windows: IAudioEndpointVolume on the source's IMMDevice ───────────────────
#[cfg(target_os = "windows")]
mod imp {
    // The WASAPI backend lives at crate::wasapi (src/windows/mod.rs, aliased
    // apart from the `windows` crate name).
    pub fn input_gain(source_id: u32) -> Option<f32> {
        crate::wasapi::endpoint_volume(source_id)
    }
    pub fn set_input_gain(source_id: u32, scalar: f32) -> bool {
        crate::wasapi::set_endpoint_volume(source_id, scalar)
    }
}

// ── Other targets: no-op ──────────────────────────────────────────────────────
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
mod imp {
    pub fn input_gain(_source_id: u32) -> Option<f32> {
        None
    }
    pub fn set_input_gain(_source_id: u32, _scalar: f32) -> bool {
        false
    }
}
