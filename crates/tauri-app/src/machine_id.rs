//! Best-effort machine identifier; the machine-mode vault passphrase is
//! derived from it. Linux reads `/etc/machine-id`; Windows reads `MachineGuid`
//! from the registry; macOS reads the `IOPlatformUUID` via `ioreg`. Returns
//! an empty string when nothing can be resolved.

#[cfg(target_os = "linux")]
pub fn read_machine_id() -> String {
    syncsafe::read_to_string("/etc/machine-id")
        .or_else(|_| syncsafe::read_to_string("/var/lib/dbus/machine-id"))
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

#[cfg(target_os = "windows")]
pub fn read_machine_id() -> String {
    if let Some(id) = read_machine_guid_winreg() {
        if !id.is_empty() {
            return id;
        }
    }
    // Fallback when the direct registry read fails.
    let out = std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "(Get-ItemProperty -Path 'HKLM:\\SOFTWARE\\Microsoft\\Cryptography' -Name MachineGuid).MachineGuid",
        ])
        .output();
    match out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        _ => String::new(),
    }
}

#[cfg(target_os = "windows")]
fn read_machine_guid_winreg() -> Option<String> {
    use winreg::enums::HKEY_LOCAL_MACHINE;
    use winreg::RegKey;
    // HKLM\SOFTWARE\Microsoft\Cryptography is a shared key, not WOW64-redirected.
    let crypto = RegKey::predef(HKEY_LOCAL_MACHINE)
        .open_subkey("SOFTWARE\\Microsoft\\Cryptography")
        .ok()?;
    let guid: String = crypto.get_value("MachineGuid").ok()?;
    Some(guid.trim().to_string())
}

#[cfg(target_os = "macos")]
pub fn read_machine_id() -> String {
    // `ioreg` prints a line like: "IOPlatformUUID" = "564D1234-ABCD-..."
    let out = std::process::Command::new("ioreg")
        .args(["-rd1", "-c", "IOPlatformExpertDevice"])
        .output();
    match out {
        Ok(o) if o.status.success() => {
            let s = String::from_utf8_lossy(&o.stdout);
            s.lines()
                .find(|l| l.contains("IOPlatformUUID"))
                .and_then(|l| l.split('"').nth(3))
                .map(|v| v.trim().to_string())
                .unwrap_or_default()
        }
        _ => String::new(),
    }
}

#[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
pub fn read_machine_id() -> String {
    String::new()
}

/// Derives the machine-mode vault passphrase. Returns an empty string when
/// the machine id cannot be read.
pub fn machine_passphrase() -> String {
    let id = read_machine_id();
    if id.is_empty() {
        return String::new();
    }
    format!("daisy.v1.machine:{id}")
}
