// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Two synthetic people, shared by the tests that need the speaker encoder to
//! tell voices apart. One copy, so the pair stays honest in one place;
//! `the_two_fixture_voices_are_two_people` in `voiceprint_enrollment.rs` is
//! the assertion that keeps it so.
#![allow(dead_code)]

pub const RATE: u32 = 16_000;


/// Source-filter speech: a glottal pulse train at `f0` driven through three
/// two-pole resonators standing in for the vocal tract, with a little breath
/// noise and a syllable envelope that never dips to silence.
///
/// The formants are what make two of these two people. Pitch alone does not:
/// a first pass varying only `f0` and a spectral tilt put the pair at cosine
/// 0.733, over the 0.62 at which a match is awarded, so the encoder read them
/// as one person and every test resting on telling them apart passed or failed
/// for the wrong reason. `the_two_fixture_voices_are_two_people` is what keeps
/// that from going unnoticed again.
pub fn voice(samples: usize, f0: f32, vowels: &[[f32; 3]], seed: u64) -> Vec<i16> {
    const VOWEL_SAMPLES: usize = RATE as usize / 4; // a vowel every 250 ms
    let period = (RATE as f32 / f0).round().max(2.0) as usize;
    let mut state = seed | 1;
    let r = 0.985f32; // pole radius: the formants' bandwidth
    let mut memory = [[0.0f32; 2]; 3];
    let mut coefficients = [(0.0f32, 0.0f32); 3];
    let mut raw = Vec::with_capacity(samples);
    for n in 0..samples {
        // The tract glides between this speaker's vowels rather than sitting
        // still: two voices holding one shape apart differ only in where their
        // peaks are, and the encoder reads the movement too.
        if n % 64 == 0 {
            let position = n as f32 / VOWEL_SAMPLES as f32;
            let from = (position as usize) % vowels.len();
            let to = (from + 1) % vowels.len();
            let blend = position.fract();
            for k in 0..3 {
                let f = vowels[from][k] * (1.0 - blend) + vowels[to][k] * blend;
                let theta = std::f32::consts::TAU * f / RATE as f32;
                coefficients[k] = (2.0 * r * theta.cos(), -r * r);
            }
        }
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let breath = ((state >> 48) as i16 as f32) / 32_768.0 * 0.05;
        let x = if n % period == 0 { 1.0 } else { 0.0 } + breath;
        let mut sum = 0.0;
        for (k, (a1, a2)) in coefficients.iter().enumerate() {
            let y = x + a1 * memory[k][0] + a2 * memory[k][1];
            memory[k][1] = memory[k][0];
            memory[k][0] = y;
            sum += y;
        }
        let env = 0.70 + 0.30 * (n as f32 * 4.0 * std::f32::consts::TAU / RATE as f32).sin();
        raw.push(sum * env);
    }
    let peak = raw.iter().fold(0.0f32, |m, v| m.max(v.abs())).max(1e-6);
    raw.iter().map(|v| (v / peak * 20_000.0) as i16).collect()
}

/// The two people in these meetings. Rowan is low, with a tract that stays
/// back — /u/, /o/, /a/, second formant under 1.2 kHz throughout. Zoe is high
/// and front — /i/, /e/, /æ/, second formant never under 1.9 kHz.
pub fn rowan(samples: usize) -> Vec<i16> {
    voice(
        samples,
        105.0,
        &[[320.0, 800.0, 2_300.0], [450.0, 900.0, 2_400.0], [700.0, 1_150.0, 2_500.0]],
        0x2545_F491,
    )
}
pub fn zoe(samples: usize) -> Vec<i16> {
    voice(
        samples,
        215.0,
        &[[300.0, 2_700.0, 3_400.0], [500.0, 2_300.0, 3_100.0], [820.0, 1_950.0, 2_900.0]],
        0x9E37_79B9,
    )
}

/// Below `SAMPLE_SILENCE_FLOOR`: the gaps between turns read as silence and
/// are never gathered into anybody's sample.
pub fn room_tone(samples: usize) -> Vec<i16> {
    let mut state = 0xDEAD_BEEFu64;
    (0..samples)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            ((state >> 48) as i16) / 400
        })
        .collect()
}
