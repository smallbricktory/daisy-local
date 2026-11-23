// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Which IPv6 peers count as this machine's LAN, and the IPv6 networks a
//! phone on the LAN dials from.
use std::net::{IpAddr, Ipv6Addr};

/// An IPv6 network: its address with the host bits cleared, and its prefix length.
pub type V6Net = (Ipv6Addr, u8);

/// `ip` with every bit past the first `len` cleared.
pub fn v6_net(ip: Ipv6Addr, len: u8) -> Ipv6Addr {
    let len = u32::from(len.min(128));
    let mask = if len == 0 { 0 } else { u128::MAX << (128 - len) };
    Ipv6Addr::from(u128::from(ip) & mask)
}

/// The shortest prefix that counts as a link. A shorter one is a routed
/// block, such as 6to4's 2002::/16, and is left out.
pub const MIN_LINK_PREFIX: u8 = 48;

/// How long a reading of the interfaces is reused.
const ONLINK_TTL: std::time::Duration = std::time::Duration::from_secs(5);

/// The IPv6 networks of this machine's interfaces that are up and not
/// point-to-point, loopback included, with a prefix of at least
/// `MIN_LINK_PREFIX`. A reading is reused for `ONLINK_TTL`.
pub fn onlink_v6() -> Vec<V6Net> {
    static LAST: std::sync::Mutex<Option<(std::time::Instant, Vec<V6Net>)>> = std::sync::Mutex::new(None);
    let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((at, nets)) = last.as_ref() {
        if at.elapsed() < ONLINK_TTL {
            return nets.clone();
        }
    }
    let mut nets: Vec<V6Net> = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter(|i| i.is_oper_up() && !i.is_p2p())
        .filter_map(|i| match i.addr {
            if_addrs::IfAddr::V6(v6) if v6.prefixlen >= MIN_LINK_PREFIX => Some((v6_net(v6.ip, v6.prefixlen), v6.prefixlen)),
            _ => None,
        })
        .collect();
    nets.sort();
    nets.dedup();
    *last = Some((std::time::Instant::now(), nets.clone()));
    nets
}

/// True for a peer the sync listener serves. Every IPv4 peer, an IPv4-mapped
/// IPv6 address as its IPv4 address, and an IPv6 peer that is loopback,
/// link-local (fe80::/10) or inside one of `onlink` of at least
/// `MIN_LINK_PREFIX`. A unique-local peer is served only from an `onlink`
/// network.
pub fn lan_peer(ip: IpAddr, onlink: impl FnOnce() -> Vec<V6Net>) -> bool {
    let IpAddr::V6(v6) = ip.to_canonical() else {
        return true;
    };
    if v6.is_loopback() || v6.is_unicast_link_local() {
        return true;
    }
    onlink().iter().any(|&(net, len)| len >= MIN_LINK_PREFIX && v6_net(v6, len) == net)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nets() -> Vec<V6Net> {
        vec![("2001:db8:1:2::".parse().unwrap(), 64), ("fd6b:da8a:b11a:5e02::".parse().unwrap(), 64)]
    }

    #[test]
    fn a_network_clears_the_host_bits() {
        assert_eq!(v6_net("2001:db8:1:2:aaaa:bbbb:cccc:dddd".parse().unwrap(), 64), "2001:db8:1:2::".parse::<Ipv6Addr>().unwrap());
        assert_eq!(v6_net("fd6b:da8a:b11a:5e02::1".parse().unwrap(), 48), "fd6b:da8a:b11a::".parse::<Ipv6Addr>().unwrap());
        assert_eq!(v6_net("::1".parse().unwrap(), 128), "::1".parse::<Ipv6Addr>().unwrap());
        assert_eq!(v6_net("2001:db8::1".parse().unwrap(), 0), Ipv6Addr::UNSPECIFIED);
    }

    #[test]
    fn link_peers_are_served() {
        for ip in ["127.0.0.1", "203.0.113.9", "::1", "fe80::eca1:50b8:32b3:7910", "fd6b:da8a:b11a:5e02::a78", "::ffff:172.16.0.144", "2001:db8:1:2::77"] {
            assert!(lan_peer(ip.parse().unwrap(), nets), "{ip}");
        }
    }

    #[test]
    fn an_ipv6_peer_off_the_link_is_refused() {
        for ip in ["2001:db8:9::1", "2606:4700::1111", "2001:db8:1:3::77", "::", "::203.0.113.9", "64:ff9b::cb00:7109"] {
            assert!(!lan_peer(ip.parse().unwrap(), nets), "{ip}");
        }
    }

    #[test]
    fn a_unique_local_peer_is_served_only_from_an_interface_network() {
        assert!(!lan_peer("fd7a:115c:a1e0::5".parse().unwrap(), nets), "an overlay's ULA");
        assert!(!lan_peer("fd6b:da8a:b11a:5e03::5".parse().unwrap(), nets), "the LAN site's other /64");
    }

    #[test]
    fn a_routed_block_is_never_a_link() {
        let sixto4 = || vec![("2002::".parse().unwrap(), 16)];
        assert!(!lan_peer("2002:cb00:7109::1".parse().unwrap(), sixto4));
        assert!(!lan_peer("2001:db8:9::1".parse().unwrap(), || vec![(Ipv6Addr::UNSPECIFIED, 0)]));
    }

    #[test]
    fn an_ipv4_peer_never_reads_the_interfaces() {
        assert!(lan_peer("203.0.113.9".parse().unwrap(), || panic!("read for IPv4")));
        assert!(lan_peer("::ffff:203.0.113.9".parse().unwrap(), || panic!("read for mapped IPv4")));
    }
}
