//! Machine identity + form-factor detection.

/// This machine's name, the key for its per-machine settings block and
/// `settings.<MACHINE>.json` file.
///
/// Must not change when the network does. `sysinfo`'s host name is the
/// resolver's idea of the host, and on macOS the static HostName is unset by
/// default, so it reports whatever DHCP/DNS supplied — `mymac-wifi.lan`
/// on wifi, `mymac-lan.lan` on ethernet. Two names for one machine means
/// two settings blocks, and docking silently reverts every per-machine
/// choice. macOS therefore reads its machine-local LocalHostName
/// (`mymac`), which is also already filename-safe. Linux (`/etc/hostname`)
/// and Windows (`%COMPUTERNAME%`) are machine-local as they stand.
pub fn machine_name() -> String {
    #[cfg(target_os = "macos")]
    if let Some(n) = macos_local_host_name() {
        return n;
    }
    sysinfo::System::host_name().unwrap_or_else(|| "unknown-machine".into())
}

/// `SCDynamicStoreCopyLocalHostName` — the name set in Sharing preferences,
/// independent of any network. `None` when unset or unreadable.
#[cfg(target_os = "macos")]
fn macos_local_host_name() -> Option<String> {
    use core_foundation::base::TCFType;
    use core_foundation::string::{CFString, CFStringRef};

    #[link(name = "SystemConfiguration", kind = "framework")]
    extern "C" {
        fn SCDynamicStoreCopyLocalHostName(store: *const std::ffi::c_void) -> CFStringRef;
    }

    // A null store is documented as "use a transient session", which is what
    // a single read at startup wants.
    let raw = unsafe { SCDynamicStoreCopyLocalHostName(std::ptr::null()) };
    if raw.is_null() {
        return None;
    }
    // Copy-rule function: we own the reference.
    let name = unsafe { CFString::wrap_under_create_rule(raw) }.to_string();
    (!name.trim().is_empty()).then_some(name)
}

/// The machine name reduced to filename-safe characters, for per-machine
/// files in the (synced) profile dir (`settings.<MACHINE>.json` etc.).
pub fn sanitized_machine_name() -> String {
    machine_name()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect()
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

    #[test]
    fn machine_name_is_not_a_resolver_name() {
        let n = machine_name();
        assert!(!n.trim().is_empty(), "machine name must never be empty");
        // The settings key must not carry a DNS suffix: `mymac-wifi.lan`
        // and `mymac-lan.lan` are the same machine on two networks, and
        // keying settings by either splits them. LocalHostName (macOS) and
        // /etc/hostname (linux) are single labels; a dot means we fell back
        // to the resolver's idea of the host.
        #[cfg(target_os = "macos")]
        assert!(
            !n.contains('.'),
            "macOS machine name '{n}' looks resolver-supplied, not LocalHostName"
        );
    }
}
