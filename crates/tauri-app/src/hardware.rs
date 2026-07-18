//! Form-factor detection for live captions.

/// Whether this machine runs live captions, detected fresh each launch: on
/// for macs, desktop-class machines, and battery laptops with a dedicated
/// GPU; off only on battery laptops without one. Finalize transcribes
/// locally either way.
pub fn live_captions_enabled(is_mac: bool, has_battery: bool, has_gpu: bool) -> bool {
    is_mac || !has_battery || has_gpu
}

/// This machine's name, the key into `Settings::live_captions_by_machine`.
pub fn machine_name() -> String {
    sysinfo::System::host_name().unwrap_or_else(|| "unknown-machine".into())
}

/// Benchmark gate: captions are on at or above this batch xRT (60 s clip,
/// cores-2 threads).
pub const LIVE_CAPTIONS_MIN_BENCH_XRT: f64 = 5.0;

/// A resolved live-captions decision and where it came from.
#[derive(Debug, Clone, serde::Serialize)]
pub struct LiveCaptionsResolution {
    pub enabled: bool,
    /// "override" | "manual" | "bench" | "hardware"
    pub source: &'static str,
    pub machine: String,
    pub bench_xrt: Option<f64>,
}

/// The live-captions decision for this machine, in precedence order:
/// `DAISY_LIVE_CAPTIONS` env override → this machine's manual on/off →
/// this machine's benchmark verdict → hardware detection.
pub fn resolve_live_captions(settings: &crate::settings::Settings) -> LiveCaptionsResolution {
    let machine = machine_name();
    if let Some(forced) = live_captions_override() {
        return LiveCaptionsResolution { enabled: forced, source: "override", machine, bench_xrt: None };
    }
    let entry = settings.live_captions_by_machine.get(&machine);
    if let Some(e) = entry {
        match e.choice {
            crate::settings::LiveCaptionsChoice::On => {
                return LiveCaptionsResolution { enabled: true, source: "manual", machine, bench_xrt: e.bench_xrt };
            }
            crate::settings::LiveCaptionsChoice::Off => {
                return LiveCaptionsResolution { enabled: false, source: "manual", machine, bench_xrt: e.bench_xrt };
            }
            crate::settings::LiveCaptionsChoice::Auto => {}
        }
        if let Some(xrt) = e.bench_xrt {
            return LiveCaptionsResolution {
                enabled: xrt >= LIVE_CAPTIONS_MIN_BENCH_XRT,
                source: "bench",
                machine,
                bench_xrt: Some(xrt),
            };
        }
    }
    LiveCaptionsResolution {
        enabled: live_captions_enabled(cfg!(target_os = "macos"), has_battery(), has_whisper_gpu()),
        source: "hardware",
        machine,
        bench_xrt: None,
    }
}

/// Reads `DAISY_LIVE_CAPTIONS`: `on`/`off` forces the decision; unset or
/// any other value means "detect".
pub fn live_captions_override() -> Option<bool> {
    parse_live_captions_override(&std::env::var("DAISY_LIVE_CAPTIONS").ok()?)
}

fn parse_live_captions_override(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "on" => Some(true),
        "off" => Some(false),
        other => {
            log::warn!("DAISY_LIVE_CAPTIONS={other:?} ignored (use \"on\" or \"off\")");
            None
        }
    }
}

/// True when live whisper can offload to a GPU on this machine: a discrete
/// Vulkan device with a working driver is present. Integrated GPUs don't
/// count.
pub fn has_whisper_gpu() -> bool {
    discrete_vulkan_device_present()
}

/// Probes the system Vulkan loader for a discrete GPU. Loads libvulkan at
/// runtime; any failure (no loader, no driver, no devices) is `false`.
fn discrete_vulkan_device_present() -> bool {
    let Ok(entry) = (unsafe { ash::Entry::load() }) else {
        return false;
    };
    let info = ash::vk::InstanceCreateInfo::default();
    let Ok(instance) = (unsafe { entry.create_instance(&info, None) }) else {
        return false;
    };
    let found = unsafe { instance.enumerate_physical_devices() }
        .map(|devices| {
            devices.iter().any(|d| {
                let props = unsafe { instance.get_physical_device_properties(*d) };
                props.device_type == ash::vk::PhysicalDeviceType::DISCRETE_GPU
            })
        })
        .unwrap_or(false);
    unsafe { instance.destroy_instance(None) };
    found
}

/// True if the machine has a battery. Best-effort; returns `false` when it
/// cannot tell.
#[cfg(target_os = "linux")]
pub fn has_battery() -> bool {
    has_system_battery_in(std::path::Path::new("/sys/class/power_supply"))
}

/// True when `dir` contains a *system* battery. Wireless peripherals
/// (mice, keyboards, controllers) also expose `type: Battery` but carry
/// `scope: Device`; counting them classifies desktops as laptops and
/// silently turns live captions off.
#[cfg(any(target_os = "linux", test))]
pub(crate) fn has_system_battery_in(dir: &std::path::Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    for e in entries.flatten() {
        let p = e.path();
        let is_battery = syncsafe::read_to_string(p.join("type"))
            .map(|t| t.trim() == "Battery")
            .unwrap_or(false);
        if !is_battery {
            continue;
        }
        let peripheral = syncsafe::read_to_string(p.join("scope"))
            .map(|s| s.trim().eq_ignore_ascii_case("device"))
            .unwrap_or(false);
        if !peripheral {
            return true;
        }
    }
    false
}

#[cfg(target_os = "windows")]
pub fn has_battery() -> bool {
    use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
    // BatteryFlag 128 means no system battery; any other value means a
    // battery is present.
    let mut s = SYSTEM_POWER_STATUS::default();
    if unsafe { GetSystemPowerStatus(&mut s) }.is_ok() {
        s.BatteryFlag != 128
    } else {
        false
    }
}

#[cfg(not(any(target_os = "linux", target_os = "windows")))]
pub fn has_battery() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn override_parses_on_off_and_ignores_junk() {
        assert_eq!(parse_live_captions_override("on"), Some(true));
        assert_eq!(parse_live_captions_override(" OFF "), Some(false));
        assert_eq!(parse_live_captions_override("auto"), None);
        assert_eq!(parse_live_captions_override(""), None);
    }

    #[test]
    fn resolution_precedence_manual_then_bench_then_hardware() {
        use crate::settings::{LiveCaptionsChoice, MachineLiveCaptions, Settings};
        let name = machine_name();
        let mut s = Settings::defaults();

        // No entry → hardware source.
        assert_eq!(resolve_live_captions(&s).source, "hardware");

        // Bench verdict gates by threshold.
        s.live_captions_by_machine.insert(name.clone(), MachineLiveCaptions {
            choice: LiveCaptionsChoice::Auto,
            bench_xrt: Some(LIVE_CAPTIONS_MIN_BENCH_XRT - 0.1),
            benched_at_unix_seconds: Some(0),
        });
        let r = resolve_live_captions(&s);
        assert_eq!((r.source, r.enabled), ("bench", false));
        s.live_captions_by_machine.get_mut(&name).unwrap().bench_xrt =
            Some(LIVE_CAPTIONS_MIN_BENCH_XRT);
        let r = resolve_live_captions(&s);
        assert_eq!((r.source, r.enabled), ("bench", true));

        // Manual choice beats the bench verdict.
        s.live_captions_by_machine.get_mut(&name).unwrap().choice = LiveCaptionsChoice::Off;
        let r = resolve_live_captions(&s);
        assert_eq!((r.source, r.enabled), ("manual", false));

        // Another machine's entry does not apply here.
        let mut other = Settings::defaults();
        other.live_captions_by_machine.insert(format!("{name}-other"), MachineLiveCaptions {
            choice: LiveCaptionsChoice::Off,
            bench_xrt: None,
            benched_at_unix_seconds: None,
        });
        assert_eq!(resolve_live_captions(&other).source, "hardware");
    }

    #[test]
    fn only_gpu_less_battery_laptops_lack_live_captions() {
        assert!(live_captions_enabled(true, true, false));
        assert!(live_captions_enabled(true, false, false));
        assert!(live_captions_enabled(false, false, false));
        assert!(live_captions_enabled(false, true, true));
        assert!(!live_captions_enabled(false, true, false));
    }

    fn write_supply(dir: &std::path::Path, name: &str, typ: &str, scope: Option<&str>) {
        let d = dir.join(name);
        syncsafe::create_dir_all(&d).unwrap();
        syncsafe::write(d.join("type"), format!("{typ}\n")).unwrap();
        if let Some(sc) = scope {
            syncsafe::write(d.join("scope"), format!("{sc}\n")).unwrap();
        }
    }

    #[test]
    fn peripheral_batteries_do_not_make_a_laptop() {
        let tmp = tempfile::tempdir().unwrap();
        // Desktop with AC + wireless mouse and keyboard batteries.
        write_supply(tmp.path(), "AC", "Mains", None);
        write_supply(tmp.path(), "hidpp_battery_0", "Battery", Some("Device"));
        write_supply(tmp.path(), "hidpp_battery_1", "Battery", Some("Device"));
        assert!(!has_system_battery_in(tmp.path()));
    }

    #[test]
    fn system_battery_detected() {
        let tmp = tempfile::tempdir().unwrap();
        write_supply(tmp.path(), "AC", "Mains", None);
        // Real laptop batteries have no scope file (or scope=System).
        write_supply(tmp.path(), "BAT0", "Battery", None);
        assert!(has_system_battery_in(tmp.path()));
        let tmp2 = tempfile::tempdir().unwrap();
        write_supply(tmp2.path(), "BAT0", "Battery", Some("System"));
        assert!(has_system_battery_in(tmp2.path()));
    }

    #[test]
    fn empty_or_missing_dir_is_desktop() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(!has_system_battery_in(tmp.path()));
        assert!(!has_system_battery_in(&tmp.path().join("nope")));
    }
}
