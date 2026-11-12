// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! The host-firewall notice on Linux: the command that opens the sync port
//! and the mDNS port to this machine's LAN, IPv4 and IPv6. Reading the rules needs root, so
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

/// The command that opens the sync port and the mDNS port to each of `nets`
/// (IPv4 and IPv6 CIDR text).
pub fn command_for(firewall: Firewall, nets: &[String]) -> String {
    match firewall {
        Firewall::Ufw => nets
            .iter()
            .map(|net| {
                format!(
                    "sudo ufw allow from {net} to any port {port} proto tcp && sudo ufw allow from {net} to any port {MDNS_PORT} proto udp",
                    port = sync_port()
                )
            })
            .collect::<Vec<_>>()
            .join(" && "),
        Firewall::Firewalld => {
            let rule = |net: &str, port: u16, proto: &str| {
                let family = if net.contains(':') { "ipv6" } else { "ipv4" };
                format!("sudo firewall-cmd --permanent --add-rich-rule='rule family=\"{family}\" source address=\"{net}\" port port=\"{port}\" protocol=\"{proto}\" accept'")
            };
            let mut parts: Vec<String> = nets.iter().flat_map(|net| [rule(net, sync_port(), "tcp"), rule(net, MDNS_PORT, "udp")]).collect();
            parts.push("sudo firewall-cmd --reload".into());
            parts.join(" && ")
        }
    }
}

/// Ufw wins when both run. `None` with no firewall running or no network to open.
pub fn hint_for(ufw: bool, firewalld: bool, nets: &[String]) -> Option<FirewallHint> {
    let firewall = if ufw {
        Firewall::Ufw
    } else if firewalld {
        Firewall::Firewalld
    } else {
        return None;
    };
    if nets.is_empty() {
        return None;
    }
    Some(FirewallHint { firewall, command: command_for(firewall, nets) })
}

/// The IPv6 networks a phone on the LAN interface dials from: link-local
/// (fe80::/10) when the interface has IPv6 at all, and each of its other
/// IPv6 networks.
pub fn v6_nets(addrs: &[(std::net::Ipv6Addr, u8)]) -> Vec<String> {
    if addrs.is_empty() {
        return Vec::new();
    }
    let mut nets = vec!["fe80::/10".to_string()];
    for &(ip, len) in addrs {
        if ip.is_unicast_link_local() || ip.is_loopback() || len < crate::sync::lan::MIN_LINK_PREFIX {
            continue;
        }
        let net = format!("{}/{len}", crate::sync::lan::v6_net(ip, len));
        if !nets.contains(&net) {
            nets.push(net);
        }
    }
    nets
}

/// The networks of the interface holding the address the pairing screen
/// tells a phone: its IPv4 subnet and its IPv6 networks.
#[cfg(target_os = "linux")]
fn lan_nets() -> Vec<String> {
    let Some(ip) = crate::commands::sync::lan_ipv4().and_then(|ip| ip.parse::<Ipv4Addr>().ok()) else {
        return Vec::new();
    };
    let ifaces = if_addrs::get_if_addrs().unwrap_or_default();
    let Some(lan) = ifaces.iter().find(|i| i.ip() == std::net::IpAddr::V4(ip)) else {
        return Vec::new();
    };
    let mut nets: Vec<String> = match &lan.addr {
        if_addrs::IfAddr::V4(v4) => subnet_of(ip, v4.netmask).into_iter().collect(),
        _ => Vec::new(),
    };
    let v6: Vec<(std::net::Ipv6Addr, u8)> = ifaces
        .iter()
        .filter(|i| i.name == lan.name)
        .filter_map(|i| match &i.addr {
            if_addrs::IfAddr::V6(a) => Some((a.ip, a.prefixlen)),
            _ => None,
        })
        .collect();
    nets.extend(v6_nets(&v6));
    nets
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
    hint_for(ufw, firewalld, &lan_nets())
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
        let v4 = ["10.20.0.0/16".to_string()];
        assert_eq!(
            command_for(Firewall::Ufw, &v4),
            "sudo ufw allow from 10.20.0.0/16 to any port 7845 proto tcp && sudo ufw allow from 10.20.0.0/16 to any port 5353 proto udp"
        );
        assert_eq!(
            command_for(Firewall::Firewalld, &v4),
            "sudo firewall-cmd --permanent --add-rich-rule='rule family=\"ipv4\" source address=\"10.20.0.0/16\" port port=\"7845\" protocol=\"tcp\" accept' && sudo firewall-cmd --permanent --add-rich-rule='rule family=\"ipv4\" source address=\"10.20.0.0/16\" port port=\"5353\" protocol=\"udp\" accept' && sudo firewall-cmd --reload"
        );
    }

    #[test]
    fn ipv6_networks_get_rules_of_their_own_family() {
        let nets = ["172.16.0.0/19".to_string(), "fe80::/10".to_string()];
        let ufw = command_for(Firewall::Ufw, &nets);
        assert!(ufw.contains("sudo ufw allow from fe80::/10 to any port 7845 proto tcp"), "{ufw}");
        assert!(ufw.contains("sudo ufw allow from 172.16.0.0/19 to any port 5353 proto udp"), "{ufw}");
        let fw = command_for(Firewall::Firewalld, &nets);
        assert!(fw.contains("rule family=\"ipv6\" source address=\"fe80::/10\" port port=\"7845\""), "{fw}");
        assert!(fw.contains("rule family=\"ipv4\" source address=\"172.16.0.0/19\" port port=\"7845\""), "{fw}");
        assert_eq!(fw.matches("--reload").count(), 1);
        assert!(fw.ends_with("sudo firewall-cmd --reload"));
    }

    #[test]
    fn the_lan_interface_opens_link_local_and_its_own_ipv6_networks() {
        let addrs = [
            ("fe80::eca1:50b8:32b3:7910".parse().unwrap(), 64),
            ("fd6b:da8a:b11a:5e02:50bd:ce6c:9243:a78".parse().unwrap(), 64),
            ("fd6b:da8a:b11a:5e02:b434:6e86:6d23:d628".parse().unwrap(), 64),
            ("2001:db8:1:2::5".parse().unwrap(), 64),
            ("2002:cb00:7109::1".parse().unwrap(), 16),
        ];
        assert_eq!(v6_nets(&addrs), ["fe80::/10", "fd6b:da8a:b11a:5e02::/64", "2001:db8:1:2::/64"]);
        assert!(v6_nets(&[]).is_empty(), "an interface with no IPv6 opens no IPv6 network");
    }

    #[test]
    fn hint_prefers_ufw_and_needs_a_firewall_and_a_network() {
        let nets = ["10.20.0.0/16".to_string()];
        let both = hint_for(true, true, &nets).unwrap();
        assert_eq!(both.firewall, Firewall::Ufw);
        assert_eq!(hint_for(false, true, &nets).unwrap().firewall, Firewall::Firewalld);
        assert_eq!(hint_for(false, false, &nets), None);
        assert_eq!(hint_for(true, false, &[]), None);
    }
}
