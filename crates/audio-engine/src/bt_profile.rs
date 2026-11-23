// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Bluetooth capture-profile coercion.
//!
//! BlueZ exposes a BT headset (AirPods, etc.) as a single PipeWire *card* with
//! several mutually-exclusive *profiles*:
//!
//!   * `a2dp-sink`            — high-fidelity stereo OUT, **no microphone**
//!   * `headset-head-unit`    — telephony: mic IN + lossy OUT (HSP/HFP)
//!   * `off`                  — nothing
//!
//! On A2DP the `bluez_input.*` node exists but reads zero samples. Before
//! opening the capture, if the mic's card is a BT card whose active profile
//! has no sources (no mic), it is forced onto a profile that does. The card
//! stays on that profile for the lifetime of the recording; WirePlumber does
//! not revert an explicitly set profile while a capture is attached.
//!
//! This module is pure-parse + a thin `pactl` shell-out. All decision logic
//! lives in unit-tested free functions.

/// One selectable profile on a card, as printed by `pactl list cards`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CardProfile {
    pub name: String,
    /// How many capture sources this profile exposes (a mic profile has ≥1).
    pub sources: u32,
    pub priority: u32,
    pub available: bool,
}

/// A single card block from `pactl list cards`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Card {
    /// PulseAudio/PipeWire card name, e.g. `bluez_card.0A_1B_2C_3D_4E_5F`.
    pub name: String,
    /// Canonical MAC from the `api.bluez5.address` property, normalized to
    /// uppercase underscores. Naming-convention-independent match key.
    pub bluez_mac: Option<String>,
    pub active_profile: String,
    pub profiles: Vec<CardProfile>,
}

impl Card {
    fn active(&self) -> Option<&CardProfile> {
        self.profiles.iter().find(|p| p.name == self.active_profile)
    }

    /// True when the active profile currently exposes no microphone.
    pub fn active_profile_has_no_mic(&self) -> bool {
        match self.active() {
            Some(p) => p.sources == 0,
            // Unknown active profile: be conservative and assume a flip is
            // needed only if some mic profile exists to flip to.
            None => self.best_mic_profile(false).is_some(),
        }
    }
}

/// Is this a BlueZ card? (Only BT cards have the A2DP↔HFP profile trap.)
pub fn is_bluetooth_card(name: &str) -> bool {
    name.starts_with("bluez_card.")
}

/// Extract the BlueZ address stem shared by a card name and its input node,
/// normalized to underscores. PipeWire node names carry the MAC with colons
/// (`bluez_input.1A:2B:3C:4D:5E:6F`) while card names use underscores
/// (`bluez_card.1A_2B_3C_4D_5E_6F`); both must resolve to the same stem.
///
///   `bluez_input.0A_1B_2C_3D_4E_5F.0` → `0A_1B_2C_3D_4E_5F`
///   `bluez_input.1A:2B:3C:4D:5E:6F`   → `1A_2B_3C_4D_5E_6F`
///   `bluez_card.0A_1B_2C_3D_4E_5F`    → `0A_1B_2C_3D_4E_5F`
///
/// Returns `None` for non-BlueZ names.
pub fn bluez_address(name: &str) -> Option<String> {
    let rest = name
        .strip_prefix("bluez_input.")
        .or_else(|| name.strip_prefix("bluez_output."))
        .or_else(|| name.strip_prefix("bluez_card."))?;
    // Trim a trailing `.N` profile/port index if present (`...36.0` → `...36`).
    let stem = match rest.rfind('.') {
        // Only strip if the tail is all digits (a port index), not part of the MAC.
        Some(i) if rest[i + 1..].chars().all(|c| c.is_ascii_digit()) && !rest[i + 1..].is_empty() => {
            &rest[..i]
        }
        _ => rest,
    };
    if stem.is_empty() {
        None
    } else {
        Some(normalize_mac(stem))
    }
}

/// One comparable MAC form: uppercase, underscore-separated.
fn normalize_mac(s: &str) -> String {
    s.replace(':', "_").to_ascii_uppercase()
}

impl Card {
    /// Pick the best mic-capable profile, or `None` when the card has none.
    /// Telephony profiles win; among them, `prefer_cvsd` decides the codec:
    /// false → wideband (non-CVSD) first for transcription quality, true →
    /// CVSD first for devices that already proved their wideband path broken.
    pub fn best_mic_profile(&self, prefer_cvsd: bool) -> Option<String> {
        let candidates: Vec<&CardProfile> = self
            .profiles
            .iter()
            .filter(|p| p.available && p.sources >= 1)
            .collect();
        if candidates.is_empty() {
            return None;
        }
        let is_telephony = |p: &CardProfile| {
            let n = p.name.to_ascii_lowercase();
            n.contains("headset") || n.contains("handsfree") || n.contains("head-unit")
        };
        let is_cvsd = |p: &CardProfile| p.name.to_ascii_lowercase().contains("cvsd");
        let pick = |want_cvsd: bool| {
            candidates
                .iter()
                .find(|p| is_telephony(p) && is_cvsd(p) == want_cvsd)
                .map(|p| p.name.clone())
        };
        pick(prefer_cvsd)
            .or_else(|| pick(!prefer_cvsd))
            .or_else(|| {
                candidates
                    .iter()
                    .max_by_key(|p| p.priority)
                    .map(|p| p.name.clone())
            })
    }
}

/// Parse the full output of `pactl list cards` into card structs. Resilient to
/// unknown lines; only `Card #`, `Name:`, `Active Profile:` and the indented
/// profile lines inside the `Profiles:` block are interpreted.
pub fn parse_cards(text: &str) -> Vec<Card> {
    let mut cards: Vec<Card> = Vec::new();
    let mut cur: Option<Card> = None;
    let mut in_profiles = false;

    for raw in text.lines() {
        let trimmed = raw.trim();

        if trimmed.starts_with("Card #") {
            if let Some(c) = cur.take() {
                cards.push(c);
            }
            cur = Some(Card::default());
            in_profiles = false;
            continue;
        }
        let Some(card) = cur.as_mut() else { continue };

        if let Some(rest) = trimmed.strip_prefix("Name:") {
            card.name = rest.trim().to_string();
            in_profiles = false;
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("api.bluez5.address = ") {
            card.bluez_mac = Some(normalize_mac(rest.trim().trim_matches('"')));
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("Active Profile:") {
            card.active_profile = rest.trim().to_string();
            in_profiles = false;
            continue;
        }
        if trimmed.starts_with("Profiles:") {
            in_profiles = true;
            continue;
        }
        // A new top-level section (e.g. "Ports:", "Properties:") ends Profiles.
        if in_profiles && trimmed.ends_with(':') && !trimmed.contains('(') {
            in_profiles = false;
            continue;
        }
        if in_profiles {
            if let Some(p) = parse_profile_line(trimmed) {
                card.profiles.push(p);
            }
        }
    }
    if let Some(c) = cur.take() {
        cards.push(c);
    }
    cards
}

/// Parse one profile line, e.g.
/// `headset-head-unit: Headset Head Unit (HSP/HFP) (sinks: 1, sources: 1, priority: 30, available: yes)`
fn parse_profile_line(line: &str) -> Option<CardProfile> {
    // The name/description separator is colon-space. Profile names can
    // contain bare colons (e.g. `output:analog-stereo+input:analog-stereo`);
    // the split is on the first `": "`, not the first `:`.
    let (name, rest) = match line.find(": ") {
        Some(i) => (line[..i].trim().to_string(), &line[i + 2..]),
        None => return None,
    };
    if name.is_empty() {
        return None;
    }
    // The numeric attributes live in the final parenthesised group.
    let sources = extract_kv_u32(rest, "sources:").unwrap_or(0);
    let priority = extract_kv_u32(rest, "priority:").unwrap_or(0);
    let available = extract_available(rest);
    // Heuristic guard: a real profile line has the attribute group. Lines like
    // "Headset Head Unit" descriptions without "(sources:" are not profiles.
    if !rest.contains("sources:") {
        return None;
    }
    Some(CardProfile {
        name,
        sources,
        priority,
        available,
    })
}

fn extract_kv_u32(haystack: &str, key: &str) -> Option<u32> {
    let i = haystack.find(key)? + key.len();
    let tail = haystack[i..].trim_start();
    let digits: String = tail.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

fn extract_available(haystack: &str) -> bool {
    match haystack.find("available:") {
        Some(i) => {
            let tail = haystack[i + "available:".len()..].trim_start();
            // "yes" / "no" / "unknown" — treat anything not "no" as usable.
            !tail.starts_with("no")
        }
        // Older pactl without an availability field: assume usable.
        None => true,
    }
}

// ── Linux shell-out (thin; logic above is pure) ────────────────────────────

/// Read the current card list from `pactl list cards`.
#[cfg(target_os = "linux")]
pub fn current_cards() -> crate::error::Result<Vec<Card>> {
    use crate::error::Error;
    let out = std::process::Command::new("pactl")
        .env("LC_ALL", "C")
        .args(["list", "cards"])
        .output()
        .map_err(Error::Io)?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(Error::Subprocess(format!("pactl list cards: {stderr}")));
    }
    Ok(parse_cards(&String::from_utf8_lossy(&out.stdout)))
}

/// Set a card's profile via `pactl set-card-profile`.
#[cfg(target_os = "linux")]
pub fn set_card_profile(card_name: &str, profile: &str) -> crate::error::Result<()> {
    use crate::error::Error;
    let out = std::process::Command::new("pactl")
        .env("LC_ALL", "C")
        .args(["set-card-profile", card_name, profile])
        .output()
        .map_err(Error::Io)?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(Error::Subprocess(format!(
            "pactl set-card-profile {card_name} {profile}: {stderr}"
        )));
    }
    Ok(())
}

/// Records that a BT card was forced off its prior profile; the profile is
/// restored when the recording stops.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BtProfileFlip {
    pub card_name: String,
    pub previous_profile: String,
}

/// The Bluetooth card matching a normalized MAC stem, by canonical
/// `api.bluez5.address` property first, card-name substring second.
#[cfg(target_os = "linux")]
fn card_for<'a>(cards: &'a [Card], addr: &str) -> Option<&'a Card> {
    cards.iter().find(|c| {
        is_bluetooth_card(&c.name)
            && (c.bluez_mac.as_deref() == Some(addr)
                || c.name.to_ascii_uppercase().contains(addr))
    })
}

/// Ensure the BT card backing `mic_node_name` is on a profile that exposes a
/// microphone. No-op (returns `Ok(None)`) for non-BT mics or cards already on
/// a mic profile. Returns `Ok(Some(flip))` when a profile flip was issued; the
/// caller must then wait briefly and re-resolve the source id (BlueZ
/// renumbers the input node on profile change), and restore the flip on stop.
#[cfg(target_os = "linux")]
pub fn ensure_capture_profile(
    mic_node_name: &str,
    prefer_cvsd: bool,
) -> crate::error::Result<Option<BtProfileFlip>> {
    let Some(addr) = bluez_address(mic_node_name) else {
        return Ok(None); // not a BT mic
    };
    // No pactl (pipewire-pulse not installed) = no flip tooling; the session
    // manager's autoswitch is then the only profile authority. Proceed.
    let cards = match current_cards() {
        Ok(c) => c,
        Err(e) => {
            log::warn!("bt profile: cannot list cards ({e}); proceeding without a profile flip");
            return Ok(None);
        }
    };
    let Some(card) = card_for(&cards, &addr) else {
        let seen: Vec<&str> = cards
            .iter()
            .filter(|c| is_bluetooth_card(&c.name))
            .map(|c| c.name.as_str())
            .collect();
        log::warn!(
            "bt profile: no card matched mic '{mic_node_name}' (stem {addr}); BT cards seen: {seen:?}"
        );
        return Ok(None);
    };
    if !card.active_profile_has_no_mic() {
        log::info!(
            "bt profile: {} already on mic-capable profile '{}' — no flip",
            card.name,
            card.active_profile
        );
        return Ok(None);
    }
    let Some(profile) = card.best_mic_profile(prefer_cvsd) else {
        return Err(crate::error::Error::Subprocess(format!(
            "Bluetooth mic '{}' has no microphone profile available — \
             reconnect the headset or pick a different mic",
            card.name
        )));
    };
    log::info!(
        "bt profile: {} on '{}' (no mic) → flipping to '{}'",
        card.name,
        card.active_profile,
        profile
    );
    set_card_profile(&card.name, &profile)?;
    Ok(Some(BtProfileFlip {
        card_name: card.name.clone(),
        previous_profile: card.active_profile.clone(),
    }))
}

#[cfg(not(target_os = "linux"))]
pub fn ensure_capture_profile(
    _mic_node_name: &str,
    _prefer_cvsd: bool,
) -> crate::error::Result<Option<BtProfileFlip>> {
    // WASAPI manages A2DP↔HFP in the driver layer; no profile coercion needed.
    Ok(None)
}

/// The CVSD (narrowband) mic profile, if this card exposes one. The mSBC
/// wideband path is broken on many BlueZ/controller/headset combinations
/// (AirPods prominently): the SCO stream dies seconds after engagement while
/// the profile stays active. CVSD is the compatibility fallback.
pub fn cvsd_profile(card: &Card) -> Option<String> {
    card.profiles
        .iter()
        .find(|p| {
            p.available && p.sources >= 1 && p.name.to_ascii_lowercase().contains("cvsd")
        })
        .map(|p| p.name.clone())
}

/// Downshift a BT mic's card to its CVSD profile (silent-mic recovery on a
/// dead mSBC link). Returns true when a flip was issued.
#[cfg(target_os = "linux")]
pub fn downshift_codec(mic_node_name: &str) -> crate::error::Result<bool> {
    let Some(addr) = bluez_address(mic_node_name) else {
        return Ok(false);
    };
    let cards = match current_cards() {
        Ok(c) => c,
        Err(e) => {
            log::warn!("bt profile: cannot list cards for codec downshift: {e}");
            return Ok(false);
        }
    };
    let Some(card) = card_for(&cards, &addr) else {
        return Ok(false);
    };
    let Some(cvsd) = cvsd_profile(card) else {
        log::info!("bt profile: {} exposes no CVSD profile — no downshift", card.name);
        return Ok(false);
    };
    if card.active_profile == cvsd {
        return Ok(false);
    }
    log::warn!(
        "bt profile: {} mic dead on '{}' — downshifting to '{}' (narrowband)",
        card.name,
        card.active_profile,
        cvsd
    );
    set_card_profile(&card.name, &cvsd)?;
    Ok(true)
}

#[cfg(not(target_os = "linux"))]
pub fn downshift_codec(_mic_node_name: &str) -> crate::error::Result<bool> {
    Ok(false)
}

/// Flip + re-resolve, for every path that opens a BT mic (recording start,
/// mid-call switch, level meter, calibration). Returns `None` for non-BT mics
/// or cards already capturable; otherwise the flip to restore later, the
/// re-resolved source (BlueZ renumbers the input node on profile change),
/// and the post-flip source list — callers that need the refreshed world
/// reuse it instead of enumerating again.
#[cfg(target_os = "linux")]
pub fn prepare_capture(
    mic_node_name: &str,
    prefer_cvsd: bool,
) -> crate::error::Result<Option<(BtProfileFlip, crate::source::Source, Vec<crate::source::Source>)>>
{
    let Some(flip) = ensure_capture_profile(mic_node_name, prefer_cvsd)? else {
        return Ok(None);
    };
    let addr = bluez_address(mic_node_name);
    // BlueZ profile transition latency before the input node appears.
    std::thread::sleep(std::time::Duration::from_millis(250));
    let sources = crate::source::list_sources()?;
    let resolved = sources
        .iter()
        .find(|s| {
            s.kind == crate::source::SourceKind::Mic && bluez_address(&s.node_name) == addr
        })
        .cloned()
        .ok_or_else(|| {
            crate::error::Error::Subprocess(
                "Bluetooth mic did not expose an input after switching to its headset \
                 profile — reconnect the headset and try again"
                    .to_string(),
            )
        })?;
    Ok(Some((flip, resolved, sources)))
}

#[cfg(not(target_os = "linux"))]
pub fn prepare_capture(
    _mic_node_name: &str,
    _prefer_cvsd: bool,
) -> crate::error::Result<Option<(BtProfileFlip, crate::source::Source, Vec<crate::source::Source>)>>
{
    Ok(None)
}

/// Restore a card to the profile it was on before [`ensure_capture_profile`]
/// flipped it. Best-effort: logs and swallows errors (the recording already
/// succeeded; failing to restore music-quality audio must not surface as an
/// error to the user).
#[cfg(target_os = "linux")]
pub fn restore_profile(flip: &BtProfileFlip) {
    match set_card_profile(&flip.card_name, &flip.previous_profile) {
        Ok(()) => log::info!(
            "bt profile: restored {} to '{}'",
            flip.card_name,
            flip.previous_profile
        ),
        Err(e) => log::warn!(
            "bt profile: could not restore {} to '{}': {e}",
            flip.card_name,
            flip.previous_profile
        ),
    }
}

#[cfg(not(target_os = "linux"))]
pub fn restore_profile(_flip: &BtProfileFlip) {}

#[cfg(test)]
mod tests {
    use super::*;

    const AIRPODS_A2DP: &str = r#"
Card #42
	Name: bluez_card.0A_1B_2C_3D_4E_5F
	Driver: module-bluez5-device.c
	Owner Module: 30
	Properties:
		device.description = "Rowan's AirPods Pro"
		api.bluez5.address = "0A:1B:2C:3D:4E:5F"
	Profiles:
		off: Off (sinks: 0, sources: 0, priority: 0, available: yes)
		a2dp-sink: High Fidelity Playback (A2DP Sink) (sinks: 1, sources: 0, priority: 40, available: yes)
		a2dp-sink-aac: High Fidelity Playback (A2DP Sink, codec AAC) (sinks: 1, sources: 0, priority: 38, available: yes)
		headset-head-unit: Headset Head Unit (HSP/HFP) (sinks: 1, sources: 1, priority: 30, available: yes)
		headset-head-unit-msbc: Headset Head Unit (HSP/HFP, codec mSBC) (sinks: 1, sources: 1, priority: 29, available: yes)
	Active Profile: a2dp-sink
	Ports:
		headphone-output: Headphone (type: Headphones, priority: 0)
"#;

    const BUILTIN_CARD: &str = r#"
Card #0
	Name: alsa_card.pci-0000_00_1f.3
	Driver: module-alsa-card.c
	Profiles:
		output:analog-stereo+input:analog-stereo: Analog Stereo Duplex (sinks: 1, sources: 1, priority: 6565, available: yes)
		off: Off (sinks: 0, sources: 0, priority: 0, available: yes)
	Active Profile: output:analog-stereo+input:analog-stereo
"#;

    #[test]
    fn is_bluetooth_card_detects_bluez() {
        assert!(is_bluetooth_card("bluez_card.0A_1B_2C_3D_4E_5F"));
        assert!(!is_bluetooth_card("alsa_card.pci-0000_00_1f.3"));
    }

    #[test]
    fn bluez_address_extracts_mac_stem() {
        assert_eq!(
            bluez_address("bluez_input.0A_1B_2C_3D_4E_5F.0").as_deref(),
            Some("0A_1B_2C_3D_4E_5F")
        );
        assert_eq!(
            bluez_address("bluez_card.0A_1B_2C_3D_4E_5F").as_deref(),
            Some("0A_1B_2C_3D_4E_5F")
        );
        assert_eq!(bluez_address("alsa_input.pci-0000_00_1f.3"), None);
    }

    #[test]
    fn card_matching_uses_the_address_property_over_name_conventions() {
        let cards = parse_cards(AIRPODS_A2DP);
        assert_eq!(cards[0].bluez_mac.as_deref(), Some("0A_1B_2C_3D_4E_5F"));
        // A node name in any format resolves to the same normalized stem.
        assert_eq!(
            bluez_address("bluez_input.0a:1b:2c:3d:4e:5f").as_deref(),
            Some("0A_1B_2C_3D_4E_5F")
        );
    }

    #[test]
    fn colon_node_names_match_underscore_card_names() {
        // PipeWire-native node name vs pactl card name for the same headset.
        let node_stem = bluez_address("bluez_input.1A:2B:3C:4D:5E:6F").unwrap();
        let card_stem = bluez_address("bluez_card.1A_2B_3C_4D_5E_6F").unwrap();
        assert_eq!(node_stem, card_stem);
        assert!("bluez_card.1A_2B_3C_4D_5E_6F".contains(&node_stem));
    }

    #[test]
    fn parses_airpods_card_with_profiles() {
        let cards = parse_cards(AIRPODS_A2DP);
        assert_eq!(cards.len(), 1);
        let c = &cards[0];
        assert_eq!(c.name, "bluez_card.0A_1B_2C_3D_4E_5F");
        assert_eq!(c.active_profile, "a2dp-sink");
        assert_eq!(c.profiles.len(), 5);
        let a2dp = c.profiles.iter().find(|p| p.name == "a2dp-sink").unwrap();
        assert_eq!(a2dp.sources, 0);
        let hsp = c.profiles.iter().find(|p| p.name == "headset-head-unit").unwrap();
        assert_eq!(hsp.sources, 1);
        assert!(hsp.available);
    }

    #[test]
    fn airpods_on_a2dp_needs_flip_to_headset() {
        let card = &parse_cards(AIRPODS_A2DP)[0];
        assert!(card.active_profile_has_no_mic());
        assert_eq!(card.best_mic_profile(false).as_deref(), Some("headset-head-unit"));
    }

    #[test]
    fn codec_preference_orders_telephony_profiles() {
        let card = &parse_cards(AIRPODS_PIPEWIRE_NATIVE)[0];
        // Wideband first by default (transcription quality)…
        assert_eq!(card.best_mic_profile(false).as_deref(), Some("headset-head-unit"));
        // …CVSD first for devices that proved wideband broken.
        assert_eq!(
            card.best_mic_profile(true).as_deref(),
            Some("headset-head-unit-cvsd")
        );
        // A card with only one telephony profile serves it either way.
        let plain = &parse_cards(AIRPODS_A2DP)[0];
        assert_eq!(plain.best_mic_profile(true).as_deref(), Some("headset-head-unit"));
    }

    #[test]
    fn builtin_duplex_card_needs_no_flip() {
        let card = &parse_cards(BUILTIN_CARD)[0];
        assert!(!card.active_profile_has_no_mic());
    }

    const AIRPODS_PIPEWIRE_NATIVE: &str = r#"
Card #5059
	Name: bluez_card.1A_2B_3C_4D_5E_6F
	Driver: module-bluez5-device.c
	Properties:
		api.bluez5.address = "1A:2B:3C:4D:5E:6F"
	Profiles:
		off: Off (sinks: 0, sources: 0, priority: 0, available: yes)
		a2dp-sink: High Fidelity Playback (A2DP Sink, codec SBC) (sinks: 1, sources: 0, priority: 18, available: yes)
		a2dp-sink-sbc_xq: High Fidelity Playback (A2DP Sink, codec SBC-XQ) (sinks: 1, sources: 0, priority: 17, available: yes)
		headset-head-unit-cvsd: Headset Head Unit (HSP/HFP, codec CVSD) (sinks: 1, sources: 1, priority: 2, available: yes)
		headset-head-unit: Headset Head Unit (HSP/HFP, codec mSBC) (sinks: 1, sources: 1, priority: 3, available: yes)
	Active Profile: a2dp-sink
"#;

    #[test]
    fn cvsd_profile_found_on_the_real_airpods_card() {
        let cards = parse_cards(AIRPODS_PIPEWIRE_NATIVE);
        let card = &cards[0];
        assert_eq!(card.bluez_mac.as_deref(), Some("1A_2B_3C_4D_5E_6F"));
        assert_eq!(cvsd_profile(card).as_deref(), Some("headset-head-unit-cvsd"));
        // No CVSD variant → no downshift target.
        let plain = &parse_cards(AIRPODS_A2DP)[0];
        assert_eq!(cvsd_profile(plain), None);
    }

    #[test]
    fn unavailable_mic_profile_is_not_chosen() {
        let txt = r#"
Card #1
	Name: bluez_card.DE_AD_BE_EF_00_01
	Profiles:
		a2dp-sink: A2DP (sinks: 1, sources: 0, priority: 40, available: yes)
		headset-head-unit: HSP/HFP (sinks: 1, sources: 1, priority: 30, available: no)
	Active Profile: a2dp-sink
"#;
        let card = &parse_cards(txt)[0];
        assert!(card.active_profile_has_no_mic());
        // The only mic profile is unavailable → nothing to flip to.
        assert_eq!(card.best_mic_profile(false), None);
    }
}
