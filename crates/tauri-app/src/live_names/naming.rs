// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! The live naming rule as pure functions: counted-sample windows, slot and
//! voice prints, greedy pairing over the whole cosine table, and `settle`.

use std::collections::BTreeMap;

/// Counted samples of one slot that form one embedded window (10 s).
pub const WINDOW_SAMPLES: usize = 160_000;
/// Cosine a pair needs to enter the greedy table.
pub const PAIR_FLOOR: f32 = 0.50;
/// Cosine a claim needs to name a slot: `cos >= NAME_FLOOR` in float32.
pub const NAME_FLOOR: f32 = 0.70;

/// A saved voice as the namer sees it.
#[derive(Debug, Clone, PartialEq)]
pub struct Voice {
    pub id: String,
    pub display_name: String,
    /// The voice under the contact named "Me".
    pub is_me: bool,
    /// Mean of the voice's samples in the app's embedding space, L2-normalised.
    pub print: Vec<f32>,
}

/// The saved voices a running instance names against. Emptied when the
/// vault locks; the instance then reads every slot by its ordinal.
pub type Voices = std::sync::Arc<std::sync::Mutex<Vec<Voice>>>;

/// A slot's claim on a voice (an index into the voice table) and its strength.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Claim {
    pub voice: usize,
    pub cosine: f32,
}

/// Dot product accumulated in index order; NaN when the vectors cannot be
/// compared. Prints are unit length, so this is their cosine.
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return f32::NAN;
    }
    let mut acc = 0.0f32;
    for (x, y) in a.iter().zip(b) {
        acc += x * y;
    }
    acc
}

/// Mean of the vectors, L2-normalised. Vectors whose length differs from the
/// first are left out; `None` when nothing is left or the mean is zero.
pub fn mean_print(vectors: &[Vec<f32>]) -> Option<Vec<f32>> {
    let first = vectors.first()?;
    let mut mean = vec![0.0f32; first.len()];
    let mut n = 0usize;
    for v in vectors.iter().filter(|v| v.len() == first.len()) {
        for (m, x) in mean.iter_mut().zip(v) {
            *m += x;
        }
        n += 1;
    }
    let norm = mean.iter().map(|x| x * x).sum::<f32>().sqrt();
    if n == 0 || norm <= 0.0 || !norm.is_finite() {
        return None;
    }
    for m in mean.iter_mut() {
        *m /= norm;
    }
    Some(mean)
}

/// The one slot above 0.5 in a frame's mask, when exactly one is.
pub fn counted_slot(mask: u8) -> Option<u8> {
    (mask.count_ones() == 1).then(|| mask.trailing_zeros() as u8)
}

/// Collects each slot's counted samples into whole windows.
#[derive(Debug, Default)]
pub struct SlotAudio {
    pending: BTreeMap<u8, Vec<f32>>,
}

impl SlotAudio {
    /// Adds counted samples to a slot; returns every window they complete.
    pub fn push(&mut self, slot: u8, samples: &[f32]) -> Vec<Vec<f32>> {
        let buf = self.pending.entry(slot).or_default();
        buf.extend_from_slice(samples);
        let mut out = Vec::new();
        while buf.len() >= WINDOW_SAMPLES {
            out.push(buf.drain(..WINDOW_SAMPLES).collect());
        }
        out
    }
}

/// The best voice for each slot, one voice to one slot: pairs at or above
/// [`PAIR_FLOOR`] ranked by cosine over the whole table, each slot and voice
/// taken once, then claims below [`NAME_FLOOR`] dropped.
pub fn greedy(slots: &BTreeMap<u8, Vec<f32>>, voices: &[Voice]) -> BTreeMap<u8, Claim> {
    let mut pairs: Vec<(u8, usize, f32)> = Vec::new();
    for (&slot, print) in slots {
        for (i, v) in voices.iter().enumerate() {
            let c = cosine(print, &v.print);
            if !c.is_nan() && c >= PAIR_FLOOR {
                pairs.push((slot, i, c));
            }
        }
    }
    pairs.sort_by(|a, b| b.2.total_cmp(&a.2).then(a.0.cmp(&b.0)).then(a.1.cmp(&b.1)));
    let mut taken = vec![false; voices.len()];
    let mut out = BTreeMap::new();
    for (slot, voice, c) in pairs {
        if out.contains_key(&slot) || taken[voice] {
            continue;
        }
        taken[voice] = true;
        out.insert(slot, Claim { voice, cosine: c });
    }
    out.retain(|_, c| c.cosine >= NAME_FLOOR);
    out
}

/// The names to show after a fresh greedy pass. A name already shown stays,
/// at its fresh cosine, unless an unnamed slot claims the same voice more
/// strongly; then the voice moves to that slot. Every other greedy claim
/// lands on an unnamed slot whose voice nobody holds, the stronger claims
/// first, one voice per slot.
pub fn settle(
    shown: &BTreeMap<u8, Claim>,
    greedy: &BTreeMap<u8, Claim>,
    cosine_of: impl Fn(u8, usize) -> f32,
) -> BTreeMap<u8, Claim> {
    let mut result = BTreeMap::new();
    for (&slot, claim) in shown {
        let current = cosine_of(slot, claim.voice);
        let beaten = greedy.iter().any(|(&other, rival)| {
            other != slot && !shown.contains_key(&other) && rival.voice == claim.voice && rival.cosine > current
        });
        if !beaten {
            result.insert(slot, Claim { voice: claim.voice, cosine: current });
        }
    }
    let mut held: Vec<usize> = result.values().map(|c| c.voice).collect();
    let mut rest: Vec<(&u8, &Claim)> = greedy.iter().collect();
    rest.sort_by(|a, b| b.1.cosine.total_cmp(&a.1.cosine).then(a.0.cmp(b.0)));
    for (&slot, claim) in rest {
        if !result.contains_key(&slot) && !shown.contains_key(&slot) && !held.contains(&claim.voice) {
            result.insert(slot, *claim);
            held.push(claim.voice);
        }
    }
    result
}

/// One naming pass: greedy over the slot prints, settled against what is shown.
pub fn name_slots(
    slots: &BTreeMap<u8, Vec<f32>>,
    voices: &[Voice],
    shown: &BTreeMap<u8, Claim>,
) -> BTreeMap<u8, Claim> {
    let fresh = greedy(slots, voices);
    settle(shown, &fresh, |slot, voice| {
        slots.get(&slot).map(|p| cosine(p, &voices[voice].print)).unwrap_or(f32::NAN)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(dim: usize, hot: usize) -> Vec<f32> {
        let mut v = vec![0.0; dim];
        v[hot] = 1.0;
        v
    }

    fn voice(id: &str, name: &str, print: Vec<f32>) -> Voice {
        Voice { id: id.into(), display_name: name.into(), is_me: false, print }
    }

    /// Slot print `[c, sqrt(1 - c*c), 0, ...]` against the voice `e0`.
    fn at_cosine(c: f32) -> Vec<f32> {
        let s = (1.0f64 - (c as f64) * (c as f64)).sqrt() as f32;
        let mut slot = vec![0.0f32; 256];
        slot[0] = c;
        slot[1] = s;
        slot
    }

    fn named(bits: u32) -> bool {
        let slots = BTreeMap::from([(0u8, at_cosine(f32::from_bits(bits)))]);
        let voices = [voice("v", "Ana Okafor", unit(256, 0))];
        !greedy(&slots, &voices).is_empty()
    }

    #[test]
    fn the_line_is_the_float32_nearest_zero_point_seven() {
        assert_eq!(NAME_FLOOR.to_bits(), 0x3f33_3333);
    }

    #[test]
    fn a_claim_at_the_line_names_and_the_float_below_is_dropped() {
        assert!(named(0x3f33_3333));
        assert!(!named(0x3f33_3332));
    }

    #[test]
    fn pairs_below_the_pair_floor_never_enter_the_table() {
        let slots = BTreeMap::from([(0u8, at_cosine(0.45))]);
        assert!(greedy(&slots, &[voice("v", "Ana Okafor", unit(256, 0))]).is_empty());
    }

    #[test]
    fn a_dropped_pairing_still_holds_its_voice_against_a_weaker_slot() {
        let slots = BTreeMap::from([(0u8, at_cosine(0.65)), (1u8, at_cosine(0.55))]);
        assert!(greedy(&slots, &[voice("v", "Ana Okafor", unit(256, 0))]).is_empty());
    }

    #[test]
    fn the_stronger_slot_keeps_a_shared_voice() {
        let slots = BTreeMap::from([(0u8, at_cosine(0.8)), (1u8, at_cosine(0.9))]);
        let g = greedy(&slots, &[voice("v", "Ana Okafor", unit(256, 0))]);
        assert_eq!(g.keys().copied().collect::<Vec<_>>(), vec![1]);
    }

    #[test]
    fn a_shown_name_stays_unless_an_unnamed_slot_claims_it_more_strongly() {
        let voices = [voice("v", "Ana Okafor", unit(256, 0))];
        let shown = BTreeMap::from([(0u8, Claim { voice: 0, cosine: 0.8 })]);

        let slots = BTreeMap::from([(0u8, at_cosine(0.75)), (1u8, at_cosine(0.72))]);
        let kept = name_slots(&slots, &voices, &shown);
        assert_eq!(kept.keys().copied().collect::<Vec<_>>(), vec![0]);
        assert!((kept[&0].cosine - 0.75).abs() < 1e-6, "stays at its fresh cosine");

        let slots = BTreeMap::from([(0u8, at_cosine(0.75)), (1u8, at_cosine(0.9))]);
        let moved = name_slots(&slots, &voices, &shown);
        assert_eq!(moved.keys().copied().collect::<Vec<_>>(), vec![1]);
    }

    #[test]
    fn remaining_claims_land_on_unnamed_slots_with_free_voices() {
        let voices = [voice("a", "Ana Okafor", unit(256, 0)), voice("b", "Bram Ilunga", unit(256, 1))];
        let slots = BTreeMap::from([(0u8, unit(256, 0)), (1u8, unit(256, 1))]);
        let shown = BTreeMap::from([(0u8, Claim { voice: 0, cosine: 1.0 })]);
        let out = name_slots(&slots, &voices, &shown);
        assert_eq!(out[&0].voice, 0);
        assert_eq!(out[&1].voice, 1);
    }

    #[test]
    fn windows_form_at_whole_counted_ten_seconds() {
        let mut a = SlotAudio::default();
        assert!(a.push(2, &vec![0.1; WINDOW_SAMPLES - 1]).is_empty());
        let w = a.push(2, &[0.2; 5]);
        assert_eq!(w.len(), 1);
        assert_eq!(w[0].len(), WINDOW_SAMPLES);
        assert!(a.push(3, &[0.0; 10]).is_empty(), "slots count apart");
        let w = a.push(2, &vec![0.3; WINDOW_SAMPLES]);
        assert_eq!(w.len(), 1);
    }

    #[test]
    fn only_a_frame_with_one_speaking_slot_counts() {
        assert_eq!(counted_slot(0b0000_0100), Some(2));
        assert_eq!(counted_slot(0b0000_0110), None);
        assert_eq!(counted_slot(0), None);
    }

    #[test]
    fn a_print_is_the_normalised_mean() {
        let p = mean_print(&[vec![3.0, 0.0], vec![3.0, 4.0]]).unwrap();
        let n = (p[0] * p[0] + p[1] * p[1]).sqrt();
        assert!((n - 1.0).abs() < 1e-6);
        assert!(mean_print(&[]).is_none());
    }
}
