// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Capture-packet continuity from the device's own stream position.
//!
//! WASAPI's `IAudioCaptureClient::GetBuffer` reports, per packet, the device
//! position of its first frame (frames since stream start, at the stream's
//! rate). [`PacketGap`] returns how far a packet starts past the end of the
//! previous one: device frames that never reached the client. A packet
//! repeating the previous position continues the previous packet; a backward
//! jump re-anchors and reports nothing.

/// Longest device-position gap, in seconds, that the capture loop fills with
/// silence.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub(crate) const MAX_FILL_SECS: u64 = 30;

/// What [`PacketGap::observe`] found before a packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Gap {
    None,
    /// Device frames lost before the packet.
    Lost(u64),
    /// A position jump of this many frames that the packets' QPC stamps do
    /// not account for; nothing is known lost.
    Uncorroborated(u64),
}

/// Device-position bookkeeping for one capture stream.
#[derive(Debug)]
pub(crate) struct PacketGap {
    /// Device frames per second.
    rate: u64,
    /// Position and QPC ns of the last packet that started a new position.
    last: Option<(u64, u64)>,
    /// Position the next packet starts at when nothing was lost.
    expected: u64,
}

impl PacketGap {
    pub(crate) fn new(rate: u32) -> Self {
        Self { rate: rate.max(1) as u64, last: None, expected: 0 }
    }

    /// Records a packet of `frames` frames starting at `position`, captured
    /// at `qpc_ns` (0 when unknown). A jump past the previous packet's end is
    /// [`Gap::Lost`] when the QPC time elapsed between the two packets, less
    /// the duration of the frames delivered in between, covers at least
    /// three quarters of the jump, or when either stamp is unknown;
    /// otherwise it is [`Gap::Uncorroborated`]. The first packet of a stream
    /// is [`Gap::None`].
    pub(crate) fn observe(&mut self, position: u64, frames: u32, qpc_ns: u64) -> Gap {
        let frames = frames as u64;
        let Some((last_pos, last_qpc)) = self.last else {
            self.last = Some((position, qpc_ns));
            self.expected = position.saturating_add(frames);
            return Gap::None;
        };
        if position == last_pos {
            self.expected = self.expected.saturating_add(frames);
            return Gap::None;
        }
        let delivered = self.expected - last_pos;
        let lost = position.saturating_sub(self.expected);
        self.last = Some((position, qpc_ns));
        self.expected = position.saturating_add(frames);
        if lost == 0 {
            return Gap::None;
        }
        if qpc_ns != 0 && last_qpc != 0 {
            let ns = |f: u64| f as u128 * 1_000_000_000 / self.rate as u128;
            let unaccounted_ns = (qpc_ns.saturating_sub(last_qpc) as u128).saturating_sub(ns(delivered));
            if unaccounted_ns * 4 < ns(lost) * 3 {
                return Gap::Uncorroborated(lost);
            }
        }
        Gap::Lost(lost)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;

    /// QPC ns of device frame `pos` on a stream whose clock matches QPC.
    fn qpc(pos: u64) -> u64 {
        7_000_000_000_000 + pos * 1_000_000_000 / RATE as u64
    }

    fn packet(g: &mut PacketGap, pos: u64, frames: u32) -> Gap {
        g.observe(pos, frames, qpc(pos))
    }

    #[test]
    fn contiguous_packets_lose_nothing() {
        let mut g = PacketGap::new(RATE);
        assert_eq!(packet(&mut g, 0, 480), Gap::None);
        assert_eq!(packet(&mut g, 480, 480), Gap::None);
        assert_eq!(packet(&mut g, 960, 441), Gap::None);
        assert_eq!(packet(&mut g, 1_401, 480), Gap::None);
    }

    #[test]
    fn first_packet_mid_stream_is_the_anchor() {
        // A stream's first packet can start past zero; it reports no loss.
        let mut g = PacketGap::new(RATE);
        assert_eq!(packet(&mut g, 96_000, 480), Gap::None);
        assert_eq!(packet(&mut g, 96_480, 480), Gap::None);
    }

    #[test]
    fn position_jump_reports_the_lost_frames() {
        // 10 ms packets, then 3.76 s the client never received.
        let mut g = PacketGap::new(RATE);
        packet(&mut g, 0, 480);
        packet(&mut g, 480, 480);
        let lost = 180_480;
        assert_eq!(packet(&mut g, 960 + lost, 480), Gap::Lost(lost));
        assert_eq!(packet(&mut g, 960 + lost + 480, 480), Gap::None, "re-anchored after the gap");
    }

    #[test]
    fn repeated_position_continues_the_previous_packet() {
        // Packets sharing one position are parts of the same device period.
        let mut g = PacketGap::new(RATE);
        packet(&mut g, 0, 480);
        assert_eq!(packet(&mut g, 480, 240), Gap::None);
        assert_eq!(packet(&mut g, 480, 240), Gap::None);
        assert_eq!(packet(&mut g, 960, 480), Gap::None);
        assert_eq!(packet(&mut g, 1_920, 480), Gap::Lost(480));
    }

    #[test]
    fn backward_jump_reanchors_without_a_fill() {
        let mut g = PacketGap::new(RATE);
        g.observe(48_000, 480, qpc(0));
        assert_eq!(g.observe(0, 480, qpc(480)), Gap::None);
        assert_eq!(g.observe(480, 480, qpc(960)), Gap::None);
        assert_eq!(g.observe(1_440, 480, qpc(1_920)), Gap::Lost(480));
    }

    #[test]
    fn jump_the_qpc_clock_does_not_cover_is_uncorroborated() {
        // Positions advancing twice as fast as the packets' QPC stamps (a
        // position counter in bytes of a mono 16-bit stream).
        let mut g = PacketGap::new(RATE);
        g.observe(0, 480, qpc(0));
        assert_eq!(g.observe(960, 480, qpc(480)), Gap::Uncorroborated(480));
        assert_eq!(g.observe(1_920, 480, qpc(960)), Gap::Uncorroborated(480));
    }

    #[test]
    fn position_counter_slightly_fast_is_uncorroborated() {
        // Positions advancing 10 % faster than the delivered frames while
        // the QPC stamps follow the frames: no packet lost anything.
        let mut g = PacketGap::new(RATE);
        g.observe(0, 480, qpc(0));
        assert_eq!(g.observe(528, 480, qpc(480)), Gap::Uncorroborated(48));
        assert_eq!(g.observe(1_056, 480, qpc(960)), Gap::Uncorroborated(48));
    }

    #[test]
    fn single_lost_packet_fills() {
        let mut g = PacketGap::new(RATE);
        packet(&mut g, 0, 480);
        packet(&mut g, 480, 480);
        assert_eq!(packet(&mut g, 1_440, 480), Gap::Lost(480));
    }

    #[test]
    fn position_near_u64_max_does_not_overflow() {
        let mut g = PacketGap::new(RATE);
        assert_eq!(g.observe(u64::MAX - 100, 480, 0), Gap::None);
        assert_eq!(g.observe(u64::MAX - 100, 480, 0), Gap::None);
        assert_eq!(g.observe(0, 480, 0), Gap::None);
    }

    #[test]
    fn unknown_qpc_trusts_the_position() {
        let mut g = PacketGap::new(RATE);
        g.observe(0, 480, 0);
        assert_eq!(g.observe(4_800, 480, 0), Gap::Lost(4_320));
        assert_eq!(g.observe(9_600, 480, qpc(9_600)), Gap::Lost(4_320));
    }

    #[test]
    fn qpc_jitter_within_a_quarter_still_fills() {
        // 100 ms gap; the stamp lands 20 ms early.
        let mut g = PacketGap::new(RATE);
        g.observe(0, 480, qpc(0));
        assert_eq!(g.observe(5_280, 480, qpc(5_280) - 20_000_000), Gap::Lost(4_800));
    }
}
