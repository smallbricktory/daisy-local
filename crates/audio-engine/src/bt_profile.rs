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
    /// PulseAudio/PipeWire card name, e.g. `bluez_card.AC_BC_32_C1_7D_36`.
    pub name: String,
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
            None => self.best_mic_profile().is_some(),
        }
    }
}

/// Is this a BlueZ card? (Only BT cards have the A2DP↔HFP profile trap.)
pub fn is_bluetooth_card(name: &str) -> bool {
    name.starts_with("bluez_card.")
}

/// Extract the BlueZ address stem shared by a card name and its input node.
///
///   `bluez_input.AC_BC_32_C1_7D_36.0` → `AC_BC_32_C1_7D_36`
///   `bluez_card.AC_BC_32_C1_7D_36`    → `AC_BC_32_C1_7D_36`
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
        Some(stem.to_string())
    }
}

impl Card {
    /// Pick the best profile that provides a microphone, or `None` if the card
    /// has no mic-capable profile available. Preference order:
    ///   1. available, ≥1 source, name contains "headset" or "handsfree"
    ///   2. available, ≥1 source, highest `priority`
    pub fn best_mic_profile(&self) -> Option<String> {
        let candidates: Vec<&CardProfile> = self
            .profiles
            .iter()
            .filter(|p| p.available && p.sources >= 1)
            .collect();
        if candidates.is_empty() {
            return None;
        }
        // Prefer a telephony profile by name.
        let telephony = candidates.iter().find(|p| {
            let n = p.name.to_ascii_lowercase();
            n.contains("headset") || n.contains("handsfree") || n.contains("head-unit")
        });
        if let Some(p) = telephony {
            return Some(p.name.clone());
        }
        // Otherwise highest priority.
        candidates
            .iter()
            .max_by_key(|p| p.priority)
            .map(|p| p.name.clone())
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

/// Ensure the BT card backing `mic_node_name` is on a profile that exposes a
/// microphone. No-op (returns `Ok(None)`) for non-BT mics or cards already on a
/// mic profile. Returns `Ok(Some(flip))` when a profile flip was issued; the
/// caller must then wait briefly and re-resolve the source id (BlueZ
/// renumbers the input node on profile change), and restore the flip on stop.
#[cfg(target_os = "linux")]
pub fn ensure_capture_profile(mic_node_name: &str) -> crate::error::Result<Option<BtProfileFlip>> {
    let Some(addr) = bluez_address(mic_node_name) else {
        return Ok(None); // not a BT mic
    };
    let cards = current_cards()?;
    let Some(card) = cards
        .iter()
        .find(|c| is_bluetooth_card(&c.name) && c.name.contains(&addr))
    else {
        return Ok(None);
    };
    if !card.active_profile_has_no_mic() {
        return Ok(None); // already capturable
    }
    let Some(profile) = card.best_mic_profile() else {
        return Err(crate::error::Error::Subprocess(format!(
            "Bluetooth mic '{}' has no microphone profile available — \
             reconnect the headset or pick a different mic",
            card.name
        )));
    };
    log::info!(
        "[bt-profile] {} on '{}' (no mic) → flipping to '{}'",
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
) -> crate::error::Result<Option<BtProfileFlip>> {
    // WASAPI manages A2DP↔HFP in the driver layer; no profile coercion needed.
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
            "[bt-profile] restored {} to '{}'",
            flip.card_name,
            flip.previous_profile
        ),
        Err(e) => log::warn!(
            "[bt-profile] could not restore {} to '{}': {e}",
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
	Name: bluez_card.AC_BC_32_C1_7D_36
	Driver: module-bluez5-device.c
	Owner Module: 30
	Properties:
		device.description = "Danny's AirPods Pro"
		api.bluez5.address = "AC:BC:32:C1:7D:36"
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
        assert!(is_bluetooth_card("bluez_card.AC_BC_32_C1_7D_36"));
        assert!(!is_bluetooth_card("alsa_card.pci-0000_00_1f.3"));
    }

    #[test]
    fn bluez_address_extracts_mac_stem() {
        assert_eq!(
            bluez_address("bluez_input.AC_BC_32_C1_7D_36.0").as_deref(),
            Some("AC_BC_32_C1_7D_36")
        );
        assert_eq!(
            bluez_address("bluez_card.AC_BC_32_C1_7D_36").as_deref(),
            Some("AC_BC_32_C1_7D_36")
        );
        assert_eq!(bluez_address("alsa_input.pci-0000_00_1f.3"), None);
    }

    #[test]
    fn parses_airpods_card_with_profiles() {
        let cards = parse_cards(AIRPODS_A2DP);
        assert_eq!(cards.len(), 1);
        let c = &cards[0];
        assert_eq!(c.name, "bluez_card.AC_BC_32_C1_7D_36");
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
        assert_eq!(card.best_mic_profile().as_deref(), Some("headset-head-unit"));
    }

    #[test]
    fn builtin_duplex_card_needs_no_flip() {
        let card = &parse_cards(BUILTIN_CARD)[0];
        assert!(!card.active_profile_has_no_mic());
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
        assert_eq!(card.best_mic_profile(), None);
    }
}
