// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! The host-firewall notice on Linux: the command that opens the sync port
//! and the mDNS port to this machine's LAN. Reading the rules needs root, so
//! the notice follows whether a firewall is running, not whether the ports
//! are already open.
use std::net::Ipv4Addr;

use serde::Serialize;

use crate::commands::sync::sync_port;
use crate::sync::advert::MDNS_PORT;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Firewall {
    Ufw,
    Firewalld,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FirewallHint {
    pub firewall: Firewall,
    pub command: String,
}

/// True when `ufw.conf` text carries an uncommented `ENABLED=yes`.
pub fn ufw_enabled(conf: &str) -> bool {
    conf.lines()
        .map(str::trim)
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| l.split_once('='))
        .any(|(k, v)| k.trim() == "ENABLED" && v.trim().trim_matches(|c| c == '"' || c == '\'').eq_ignore_ascii_case("yes"))
}

/// `ip`/`netmask` as network CIDR text: 192.168.1.23 + 255.255.255.0 gives
/// `192.168.1.0/24`. `None` for a non-contiguous mask, a host route, and a
/// loopback, link-local or unspecified address.
pub fn subnet_of(ip: Ipv4Addr, netmask: Ipv4Addr) -> Option<String> {
    if ip.is_loopback() || ip.is_link_local() || ip.is_unspecified() {
        return None;
    }
    let mask = u32::from(netmask);
    let prefix = mask.leading_ones();
    if prefix == 0 || prefix == 32 || mask.count_ones() != prefix {
        return None;
    }
    Some(format!("{}/{}", Ipv4Addr::from(u32::from(ip) & mask), prefix))
}

pub fn command_for(firewall: Firewall, net: &str) -> String {
    match firewall {
        Firewall::Ufw => format!(
            "sudo ufw allow from {net} to any port {port} proto tcp && sudo ufw allow from {net} to any port {MDNS_PORT} proto udp",
            port = sync_port()
        ),
        Firewall::Firewalld => {
            let rule = |port: u16, proto: &str| {
                format!("sudo firewall-cmd --permanent --add-rich-rule='rule family=\"ipv4\" source address=\"{net}\" port port=\"{port}\" protocol=\"{proto}\" accept'")
            };
            format!("{} && {} && sudo firewall-cmd --reload", rule(sync_port(), "tcp"), rule(MDNS_PORT, "udp"))
        }
    }
}

/// Ufw wins when both run.
pub fn hint_for(ufw: bool, firewalld: bool, net: Option<&str>) -> Option<FirewallHint> {
    let firewall = if ufw {
        Firewall::Ufw
    } else if firewalld {
        Firewall::Firewalld
    } else {
        return None;
    };
    Some(FirewallHint { firewall, command: command_for(firewall, net?) })
}

/// The LAN subnet of the address the pairing screen tells a phone.
#[cfg(target_os = "linux")]
fn lan_subnet() -> Option<String> {
    let ip: Ipv4Addr = crate::commands::sync::lan_ipv4()?.parse().ok()?;
    if_addrs::get_if_addrs().unwrap_or_default().into_iter().find_map(|i| match i.addr {
        if_addrs::IfAddr::V4(v4) if v4.ip == ip => subnet_of(ip, v4.netmask),
        _ => None,
    })
}

#[cfg(target_os = "linux")]
pub fn linux_firewall_hint() -> Option<FirewallHint> {
    let ufw = std::fs::read_to_string("/etc/ufw/ufw.conf").map(|c| ufw_enabled(&c)).unwrap_or(false);
    let firewalld = !ufw
        && std::process::Command::new("systemctl")
            .args(["is-active", "--quiet", "firewalld"])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
    if !ufw && !firewalld {
        return None;
    }
    hint_for(ufw, firewalld, lan_subnet().as_deref())
}

#[cfg(not(target_os = "linux"))]
pub fn linux_firewall_hint() -> Option<FirewallHint> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ufw_conf_reads_the_enabled_line_and_skips_comments() {
        assert!(ufw_enabled("# comment\nENABLED=yes\nLOGLEVEL=low\n"));
        assert!(ufw_enabled("  ENABLED = Yes  \n"));
        assert!(!ufw_enabled("ENABLED=no\n"));
        assert!(!ufw_enabled("#ENABLED=yes\n"));
        assert!(!ufw_enabled("NOT_ENABLED=yes\n"));
        assert!(!ufw_enabled(""));
    }

    #[test]
    fn subnet_masks_the_host_bits() {
        let m24 = Ipv4Addr::new(255, 255, 255, 0);
        assert_eq!(subnet_of(Ipv4Addr::new(192, 168, 1, 23), m24).as_deref(), Some("192.168.1.0/24"));
        assert_eq!(subnet_of(Ipv4Addr::new(172, 16, 5, 9), Ipv4Addr::new(255, 255, 224, 0)).as_deref(), Some("172.16.0.0/19"));
    }

    #[test]
    fn subnet_skips_addresses_a_phone_cannot_use() {
        let m = Ipv4Addr::new(255, 255, 255, 0);
        assert_eq!(subnet_of(Ipv4Addr::new(127, 0, 0, 1), m), None);
        assert_eq!(subnet_of(Ipv4Addr::new(169, 254, 3, 4), m), None);
        assert_eq!(subnet_of(Ipv4Addr::new(10, 0, 0, 2), Ipv4Addr::new(255, 255, 255, 255)), None);
        assert_eq!(subnet_of(Ipv4Addr::new(10, 0, 0, 2), Ipv4Addr::new(255, 0, 255, 0)), None);
    }

    #[test]
    fn commands_carry_both_ports() {
        assert_eq!(
            command_for(Firewall::Ufw, "10.20.0.0/16"),
            "sudo ufw allow from 10.20.0.0/16 to any port 7845 proto tcp && sudo ufw allow from 10.20.0.0/16 to any port 5353 proto udp"
        );
        assert_eq!(
            command_for(Firewall::Firewalld, "10.20.0.0/16"),
            "sudo firewall-cmd --permanent --add-rich-rule='rule family=\"ipv4\" source address=\"10.20.0.0/16\" port port=\"7845\" protocol=\"tcp\" accept' && sudo firewall-cmd --permanent --add-rich-rule='rule family=\"ipv4\" source address=\"10.20.0.0/16\" port port=\"5353\" protocol=\"udp\" accept' && sudo firewall-cmd --reload"
        );
    }

    #[test]
    fn hint_prefers_ufw_and_needs_a_firewall_and_a_subnet() {
        let both = hint_for(true, true, Some("10.20.0.0/16")).unwrap();
        assert_eq!(both.firewall, Firewall::Ufw);
        assert_eq!(hint_for(false, true, Some("10.20.0.0/16")).unwrap().firewall, Firewall::Firewalld);
        assert_eq!(hint_for(false, false, Some("10.20.0.0/16")), None);
        assert_eq!(hint_for(true, false, None), None);
    }
}
