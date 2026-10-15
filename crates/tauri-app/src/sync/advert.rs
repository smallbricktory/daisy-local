// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! The DNS-SD advertisement a phone with a typed pairing key browses for:
//! `_daisy-pair._tcp`, carrying this machine's id, the key's routing tag and
//! the host and port to dial. Up only while a key is live, and answered only
//! on the interfaces that host is reached at.
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

/// The names of this machine's non-loopback interfaces that hold an address
/// `host` names.
/// A name that resolves to none of them falls back to the interface of this
/// machine's LAN IPv4.
fn interfaces_for(host: &str) -> Vec<String> {
    let ifaces = if_addrs::get_if_addrs().unwrap_or_default();
    let pick = |addrs: &[IpAddr]| {
        let mut names: Vec<String> = ifaces.iter().filter(|i| !i.is_loopback() && addrs.contains(&i.ip())).map(|i| i.name.clone()).collect();
        names.sort();
        names.dedup();
        names
    };
    let names = pick(&addresses_of(host));
    if !names.is_empty() || host.parse::<IpAddr>().is_ok() {
        return names;
    }
    match crate::commands::sync::lan_ipv4().and_then(|ip| ip.parse::<IpAddr>().ok()) {
        Some(ip) => pick(&[ip]),
        None => Vec::new(),
    }
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
}
