// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! The DNS-SD advertisement a phone with a typed pairing key browses for:
//! `_daisy-pair._tcp`, carrying this machine's id, the key's routing tag and
//! the host and port to dial. Up only while a key is live, and answered only
//! on the interfaces that host is reached at and the LAN's.
use std::net::{IpAddr, ToSocketAddrs};

use crate::error::{AppError, Result};

/// The multicast DNS port a phone browses on.
pub const MDNS_PORT: u16 = 5353;

const SERVICE: &str = "_daisy-pair._tcp.local.";

/// How long a host name has to resolve to this machine's addresses.
const RESOLVE_WAIT: std::time::Duration = std::time::Duration::from_secs(2);

/// The addresses `host` names: itself when it is one, else what it resolves
/// to within `RESOLVE_WAIT`.
fn addresses_of(host: &str) -> Vec<IpAddr> {
    if let Ok(ip) = host.parse::<IpAddr>() {
        return vec![ip];
    }
    let host = host.to_string();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let found: Vec<IpAddr> = (host.as_str(), 0).to_socket_addrs().map(|a| a.map(|s| s.ip()).collect()).unwrap_or_default();
        let _ = tx.send(found);
    });
    rx.recv_timeout(RESOLVE_WAIT).unwrap_or_default()
}

/// The names of this machine's interfaces that are up, not loopback and not
/// point-to-point, and hold an address `host` names. A name also selects the
/// interface of this machine's LAN IPv4.
fn interfaces_for(host: &str) -> Vec<String> {
    let held: Vec<(String, IpAddr)> = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter(|i| !i.is_loopback() && i.is_oper_up() && !i.is_p2p())
        .map(|i| (i.name.clone(), i.ip()))
        .collect();
    let lan = match host.parse::<IpAddr>() {
        Ok(_) => None,
        Err(_) => crate::commands::sync::lan_ipv4().and_then(|ip| ip.parse::<IpAddr>().ok()),
    };
    choose(&held, &addresses_of(host), lan)
}

/// The names in `held` of the interfaces holding an address in `named` or `lan`.
fn choose(held: &[(String, IpAddr)], named: &[IpAddr], lan: Option<IpAddr>) -> Vec<String> {
    let mut names: Vec<String> = held.iter().filter(|(_, ip)| named.contains(ip) || lan == Some(*ip)).map(|(n, _)| n.clone()).collect();
    names.sort();
    names.dedup();
    names
}

/// A registered advertisement. Dropping it withdraws it.
pub struct Advert {
    daemon: mdns_sd::ServiceDaemon,
    fullname: String,
}

impl Advert {
    pub fn start(device_id: &str, tag: &str, host: &str, port: u16) -> Result<Self> {
        let interfaces = interfaces_for(host);
        if interfaces.is_empty() {
            return Err(AppError::Io(format!("mDNS: no interface holds an address for {host}")));
        }
        let daemon = mdns_sd::ServiceDaemon::new().map_err(|e| AppError::Io(format!("mDNS: {e}")))?;
        daemon.disable_interface(mdns_sd::IfKind::All).map_err(|e| AppError::Io(format!("mDNS: {e}")))?;
        for name in &interfaces {
            daemon.enable_interface(mdns_sd::IfKind::Name(name.clone())).map_err(|e| AppError::Io(format!("mDNS: {e}")))?;
        }
        let port_text = port.to_string();
        let txt = [("v", "2"), ("id", device_id), ("tag", tag), ("host", host), ("port", port_text.as_str())];
        // The SRV target is a name of its own.
        let target = format!("daisy-{}.local.", &device_id[..device_id.len().min(12)]);
        let info = mdns_sd::ServiceInfo::new(SERVICE, &format!("daisy-{device_id}"), &target, "", port, &txt[..])
            .map_err(|e| AppError::Io(format!("mDNS: {e}")))?
            .enable_addr_auto();
        let fullname = info.get_fullname().to_string();
        daemon.register(info).map_err(|e| AppError::Io(format!("mDNS: {e}")))?;
        log::info!("sync: advertising the pairing key's tag on {}", interfaces.join(", "));
        Ok(Self { daemon, fullname })
    }
}

impl Drop for Advert {
    /// Sends the goodbye off the caller's thread, then stops the responder.
    fn drop(&mut self) {
        let daemon = self.daemon.clone();
        let goodbye = daemon.unregister(&self.fullname);
        std::thread::spawn(move || {
            if let Ok(rx) = goodbye {
                let _ = rx.recv_timeout(std::time::Duration::from_secs(2));
            }
            if let Err(e) = daemon.shutdown() {
                log::debug!("sync: mDNS shutdown: {e}");
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_address_selects_the_interface_that_holds_it_and_no_other() {
        let ifaces = if_addrs::get_if_addrs().unwrap();
        let holds_only_loopback = |name: &str| ifaces.iter().filter(|i| i.name == name).all(|i| i.is_loopback());
        assert!(interfaces_for("127.0.0.1").is_empty(), "loopback is never advertised on");
        for name in interfaces_for("localhost") {
            assert!(!holds_only_loopback(&name), "{name} holds only loopback addresses");
        }
        if let Some(lan) = ifaces.iter().find(|i| !i.is_loopback() && i.ip().is_ipv4()) {
            let chosen = interfaces_for(&lan.ip().to_string());
            let holders: Vec<String> = {
                let mut v: Vec<String> = ifaces.iter().filter(|i| !i.is_loopback() && i.ip() == lan.ip()).map(|i| i.name.clone()).collect();
                v.sort();
                v.dedup();
                v
            };
            assert_eq!(chosen, holders);
        }
        assert!(interfaces_for("203.0.113.9").is_empty(), "an address this machine does not hold selects nothing");
    }

    fn held() -> Vec<(String, IpAddr)> {
        [("wlp0s20f3", "172.16.0.152"), ("wlp0s20f3", "fd6b::1"), ("docker0", "172.17.0.1"), ("tun0", "10.8.0.2")]
            .iter()
            .map(|(n, ip)| (n.to_string(), ip.parse().unwrap()))
            .collect()
    }

    #[test]
    fn a_name_that_resolves_to_a_bridge_alone_also_selects_the_lan_interface() {
        let lan = Some("172.16.0.152".parse().unwrap());
        assert_eq!(choose(&held(), &["172.17.0.1".parse().unwrap()], lan), ["docker0", "wlp0s20f3"]);
        assert_eq!(choose(&held(), &[], lan), ["wlp0s20f3"], "a name that resolves to nothing selects the LAN interface");
        assert_eq!(choose(&held(), &["fd6b::1".parse().unwrap()], lan), ["wlp0s20f3"], "one interface is named once");
    }

    #[test]
    fn a_tunnel_is_selected_only_when_the_host_is_its_address() {
        let lan = Some("172.16.0.152".parse().unwrap());
        assert!(!choose(&held(), &["172.17.0.1".parse().unwrap()], lan).contains(&"tun0".to_string()));
        assert_eq!(choose(&held(), &["10.8.0.2".parse().unwrap()], None), ["tun0"], "an address host selects its holder alone");
    }
}
