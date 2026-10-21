// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Phone-to-desktop sync: this machine's identity and its pairings.
pub mod advert;
pub mod exchange;
pub mod firewall;
pub mod gate;
pub mod hellos;
pub mod identity;
pub mod import;
pub mod inbox;
pub mod pairing;
pub mod peripheral;
pub mod pipeline;
pub mod records;
pub mod server;
pub mod session_meta;
pub mod signing;
pub mod voices;
pub mod wake;
#[cfg(test)]
mod records_tests;

/// A name a peer sent, as a log line shows it: its first 32 characters,
/// quoted and escaped.
pub(crate) fn shown(peer_text: &str) -> String {
    let head: String = peer_text.chars().take(32).collect();
    format!("{head:?}")
}

#[cfg(test)]
mod shown_tests {
    #[test]
    fn a_peer_name_is_logged_quoted_escaped_and_cut_short() {
        assert_eq!(super::shown("daisy-1"), "\"daisy-1\"");
        assert_eq!(super::shown("a\nb"), "\"a\\nb\"");
        assert_eq!(super::shown(&"x".repeat(100)), format!("\"{}\"", "x".repeat(32)));
    }
}
