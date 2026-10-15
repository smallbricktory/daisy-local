// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! One live-names instance: audio in, per-slot turns and names out. Holds no
//! threads and no models of its own; the diarizer and the embedder come in
//! through two small traits.

use std::collections::BTreeMap;

use anyhow::Result;
use providers_local::nemotron3_diar::{LiveDiarizer, FRAME_SAMPLES, MIN_TURN_FRAMES};

use super::naming::{self, Claim, SlotAudio, Voices};
use super::overlay::{Overlay, OverlayName, OverlayTurn};
use super::{LiveSpeakers, NameOut, SlotDelta, TurnOut};

/// Audio between two emitted updates.
const EMIT_EVERY_S: f64 = 1.0;
/// Every this many updates, one carries the whole history (`full`).
const FULL_EVERY: u32 = 30;

pub trait Diarize {
    fn push(&mut self, samples: &[f32]) -> Result<usize>;
    fn finish(&mut self) -> Result<usize>;
    fn frames(&self) -> &[u8];
    fn audio_seconds(&self) -> f64;
}

impl Diarize for LiveDiarizer {
    fn push(&mut self, samples: &[f32]) -> Result<usize> {
        LiveDiarizer::push(self, samples)
    }
    fn finish(&mut self) -> Result<usize> {
        LiveDiarizer::finish(self)
    }
    fn frames(&self) -> &[u8] {
        LiveDiarizer::frames(self)
    }
    fn audio_seconds(&self) -> f64 {
        LiveDiarizer::audio_seconds(self)
    }
}

/// Embeds a window of mono 16 kHz audio into a unit speaker print.
pub trait Embed {
    fn embed(&mut self, samples: &[f32]) -> Result<Vec<f32>>;
}

impl Embed for voiceprints::Encoder {
    fn embed(&mut self, samples: &[f32]) -> Result<Vec<f32>> {
        let pcm: Vec<i16> = samples.iter().map(|s| (s * 32768.0).clamp(-32768.0, 32767.0) as i16).collect();
        Ok(self.encode_pcm(&pcm)?)
    }
}

pub struct Engine<D: Diarize, E: Embed> {
    diar: D,
    embedder: Option<E>,
    voices: Voices,
    /// `(stream sample, session seconds)` at the start of each continuous
    /// stretch of audio fed.
    timeline: Vec<(u64, f64)>,
    expected_session_s: f64,
    fed: u64,
    /// Audio from stream sample `ring_start` on, kept while frames still
    /// need it for naming.
    ring: Vec<f32>,
    ring_start: u64,
    counted_frames: usize,
    slot_audio: SlotAudio,
    slot_embeddings: BTreeMap<u8, Vec<Vec<f32>>>,
    shown: BTreeMap<u8, Claim>,
    ordinals: BTreeMap<u8, u32>,
    windows: u32,
    updates: u32,
    last_emit_s: f64,
    /// Turns read from the frames so far.
    runs: Runs,
    /// Closed turns the updates sent so far carried.
    sent_closed: usize,
    /// Open turns and names as the last update sent them.
    sent_open: Vec<(u8, f64, f64)>,
    sent_names: BTreeMap<u8, NameOut>,
    track: &'static str,
    load_seconds: Option<f64>,
    first: FirstLabels,
}

/// The turns in the diarizer's frames, each frame read once: a run that
/// ended is a closed turn kept on the session clock; a run still going is
/// open.
#[derive(Default)]
struct Runs {
    scanned: usize,
    run_start: [Option<usize>; 8],
    /// `(slot, session start, session end)` per closed turn piece, in the
    /// order they closed.
    closed: Vec<(u8, f64, f64)>,
}

/// Each slot's first turn as first emitted, on the stream's clock, and how far
/// the audio had run past each labelled frame when a turn first covered it.
#[derive(Default)]
struct FirstLabels {
    scanned: usize,
    run_start: [Option<usize>; 8],
    /// Slot to `(start frame, end frame)` of its first turn.
    turns: BTreeMap<u8, (usize, usize)>,
    /// Frames some emitted turn has covered, by frame index.
    covered: Vec<bool>,
    /// Sum of `audio fed at first coverage - frame time` over covered frames.
    delay_s: f64,
    labelled: u64,
}

impl FirstLabels {
    /// Reads the frames added since the last call; `audio_s` is the audio
    /// the stream has taken in as they are emitted.
    fn scan(&mut self, frames: &[u8], audio_s: f64) {
        let from = self.scanned.min(frames.len());
        self.covered.resize(frames.len(), false);
        for slot in 0..8u8 {
            let bit = 1u8 << slot;
            let mut start = self.run_start[slot as usize];
            for (i, &f) in frames.iter().enumerate().skip(from) {
                match (f & bit != 0, start) {
                    (true, None) => start = Some(i),
                    (false, Some(s)) => {
                        start = None;
                        if i - s >= MIN_TURN_FRAMES {
                            self.cover(slot, s, i, audio_s);
                        }
                    }
                    _ => {}
                }
            }
            if let Some(s) = start {
                if frames.len() - s >= MIN_TURN_FRAMES {
                    self.cover(slot, s, frames.len(), audio_s);
                }
            }
            self.run_start[slot as usize] = start;
        }
        self.scanned = frames.len();
    }

    fn cover(&mut self, slot: u8, start: usize, end: usize, audio_s: f64) {
        self.turns.entry(slot).or_insert((start, end));
        for i in start..end {
            if !self.covered[i] {
                self.covered[i] = true;
                self.delay_s += audio_s - i as f64 / 100.0;
                self.labelled += 1;
            }
        }
    }

    /// Mean over labelled 10 ms frames of the delay between a frame's audio
    /// time and the first emission of a turn that covers it.
    fn mean_latency(&self) -> Option<f64> {
        (self.labelled > 0).then(|| self.delay_s / self.labelled as f64)
    }
}

impl<D: Diarize, E: Embed> Engine<D, E> {
    /// `embedder` is `None` when there are no saved voices to name against.
    pub fn new(track: &'static str, diar: D, embedder: Option<E>, voices: Voices, load_seconds: Option<f64>) -> Self {
        let embedder = if voices.lock().unwrap_or_else(|e| e.into_inner()).is_empty() { None } else { embedder };
        Self {
            diar,
            embedder,
            voices,
            timeline: Vec::new(),
            expected_session_s: 0.0,
            fed: 0,
            ring: Vec::new(),
            ring_start: 0,
            counted_frames: 0,
            slot_audio: SlotAudio::default(),
            slot_embeddings: BTreeMap::new(),
            shown: BTreeMap::new(),
            ordinals: BTreeMap::new(),
            windows: 0,
            updates: 0,
            last_emit_s: 0.0,
            runs: Runs::default(),
            sent_closed: 0,
            sent_open: Vec::new(),
            sent_names: BTreeMap::new(),
            track,
            load_seconds,
            first: FirstLabels::default(),
        }
    }

    /// Feeds one stretch of audio that starts `start_sample` samples into the
    /// chunk beginning `offset_ms` on the session clock. Returns an update
    /// when a second of audio has passed since the last and the tracks
    /// changed.
    pub fn push(&mut self, offset_ms: u32, start_sample: u64, samples: &[f32]) -> Result<Option<LiveSpeakers>> {
        if samples.is_empty() {
            return Ok(None);
        }
        let session_s = offset_ms as f64 / 1000.0 + start_sample as f64 / 16_000.0;
        if self.timeline.is_empty() || (session_s - self.expected_session_s).abs() > 0.1 {
            self.timeline.push((self.fed, session_s));
        }
        self.expected_session_s = session_s + samples.len() as f64 / 16_000.0;
        self.fed += samples.len() as u64;
        if self.embedder.is_some() {
            self.ring.extend_from_slice(samples);
        }
        self.forget_names_without_voices();
        self.diar.push(samples)?;
        self.first.scan(self.diar.frames(), self.diar.audio_seconds());
        self.count_new_frames();
        self.read_runs();
        if self.diar.audio_seconds() - self.last_emit_s >= EMIT_EVERY_S {
            self.last_emit_s = self.diar.audio_seconds();
            return Ok(self.changed());
        }
        Ok(None)
    }

    /// Ends the stream; returns a last update when anything changed.
    pub fn finish(&mut self) -> Result<Option<LiveSpeakers>> {
        self.forget_names_without_voices();
        self.diar.finish()?;
        self.first.scan(self.diar.frames(), self.diar.audio_seconds());
        self.count_new_frames();
        self.read_runs();
        Ok(self.changed())
    }

    /// Once the shared voices are emptied (the vault locked), drops the
    /// speaker encoder, the names and the slot prints, so every slot reads
    /// by its ordinal. Returns an update when a shown name changed.
    pub fn forget_names_without_voices(&mut self) -> Option<LiveSpeakers> {
        if self.embedder.is_none() && self.shown.is_empty() {
            return None;
        }
        if !self.voices.lock().unwrap_or_else(|e| e.into_inner()).is_empty() {
            return None;
        }
        self.embedder = None;
        self.shown.clear();
        self.slot_embeddings.clear();
        self.slot_audio = SlotAudio::default();
        self.ring = Vec::new();
        self.ring_start = (self.diar.frames().len() * FRAME_SAMPLES) as u64;
        self.changed()
    }

    /// The speaker encoder is loaded.
    pub fn names_voices(&self) -> bool {
        self.embedder.is_some()
    }

    /// An update when the turns or names changed since the last one sent;
    /// the first one is always sent.
    fn changed(&mut self) -> Option<LiveSpeakers> {
        let open = self.open_turns();
        self.assign_ordinals(&open);
        let names: BTreeMap<u8, NameOut> = self.ordinals.keys().map(|&s| (s, self.name_of(s))).collect();
        let first = self.updates == 0;
        let new_closed = self.runs.closed.len() > self.sent_closed;
        if !first && !new_closed && open == self.sent_open && names == self.sent_names {
            return None;
        }
        let full = self.updates % FULL_EVERY == 0;
        let from = if full { 0 } else { self.sent_closed };
        let mut tracks: BTreeMap<u8, SlotDelta> = names
            .iter()
            .map(|(&slot, name)| {
                (slot, SlotDelta { slot: slot as u32, turns: Vec::new(), open: Vec::new(), name: name.clone() })
            })
            .collect();
        for &(slot, start_s, end_s) in &self.runs.closed[from..] {
            if let Some(t) = tracks.get_mut(&slot) {
                t.turns.push(TurnOut { start_s, end_s });
            }
        }
        for &(slot, start_s, end_s) in &open {
            if let Some(t) = tracks.get_mut(&slot) {
                t.open.push(TurnOut { start_s, end_s });
            }
        }
        self.updates += 1;
        self.sent_closed = self.runs.closed.len();
        self.sent_open = open;
        self.sent_names = names;
        Some(LiveSpeakers {
            session_id: String::new(),
            track: self.track.into(),
            full,
            tracks: tracks.into_values().collect(),
        })
    }

    fn count_new_frames(&mut self) {
        if self.embedder.is_none() {
            self.counted_frames = self.diar.frames().len();
            return;
        }
        let mut renamed = false;
        let upto = self.diar.frames().len();
        for f in self.counted_frames..upto {
            let Some(slot) = naming::counted_slot(self.diar.frames()[f]) else { continue };
            let from = (f * FRAME_SAMPLES) as u64;
            let Some(lo) = from.checked_sub(self.ring_start).map(|v| v as usize) else { continue };
            if lo + FRAME_SAMPLES > self.ring.len() {
                continue;
            }
            let windows = self.slot_audio.push(slot, &self.ring[lo..lo + FRAME_SAMPLES]);
            for w in windows {
                let Some(enc) = self.embedder.as_mut() else { break };
                match enc.embed(&w) {
                    Ok(print) => {
                        self.slot_embeddings.entry(slot).or_default().push(print);
                        self.windows += 1;
                        renamed = true;
                    }
                    Err(e) => log::warn!("live speakers: embed window for slot {slot}: {e}"),
                }
            }
        }
        self.counted_frames = upto;
        let keep_from = (upto * FRAME_SAMPLES) as u64;
        if keep_from > self.ring_start {
            let drop = ((keep_from - self.ring_start) as usize).min(self.ring.len());
            self.ring.drain(..drop);
            self.ring_start += drop as u64;
        }
        if renamed {
            self.rename();
        }
    }

    fn rename(&mut self) {
        let slots: BTreeMap<u8, Vec<f32>> =
            self.slot_embeddings.iter().filter_map(|(&s, e)| naming::mean_print(e).map(|p| (s, p))).collect();
        let voices = self.voices.lock().unwrap_or_else(|e| e.into_inner());
        self.shown = naming::name_slots(&slots, &voices, &self.shown);
    }

    fn session_time(&self, stream_s: f64) -> f64 {
        let at = (stream_s * 16_000.0).round().max(0.0) as u64;
        let seg = self.timeline.iter().rev().find(|(s, _)| *s <= at).or(self.timeline.first());
        match seg {
            Some(&(s, t)) => t + (at as f64 - s as f64) / 16_000.0,
            None => stream_s,
        }
    }

    /// The stream-clock run `[start_s, end_s)` as pieces on the session
    /// clock, appended to `out`.
    fn pieces(&self, slot: u8, start_s: f64, end_s: f64, out: &mut Vec<(u8, f64, f64)>) {
        let mut cuts: Vec<f64> = vec![start_s];
        cuts.extend(
            self.timeline.iter().map(|&(s, _)| s as f64 / 16_000.0).filter(|&b| b > start_s && b < end_s),
        );
        cuts.push(end_s);
        for w in cuts.windows(2) {
            let start = self.session_time(w[0]);
            let end = self.session_time(w[1] - 1.0 / 16_000.0) + 1.0 / 16_000.0;
            out.push((slot, start, end));
        }
    }

    /// Reads the frames added since the last call; the runs that ended in
    /// them become closed turns.
    fn read_runs(&mut self) {
        let frames = self.diar.frames();
        let from = self.runs.scanned.min(frames.len());
        let mut closed: Vec<(u8, usize, usize)> = Vec::new();
        for (i, &m) in frames.iter().enumerate().skip(from) {
            for slot in 0..8u8 {
                let start = &mut self.runs.run_start[slot as usize];
                match (m & (1u8 << slot) != 0, *start) {
                    (true, None) => *start = Some(i),
                    (false, Some(s)) => {
                        if i - s >= MIN_TURN_FRAMES {
                            closed.push((slot, s, i));
                        }
                        *start = None;
                    }
                    _ => {}
                }
            }
        }
        self.runs.scanned = frames.len();
        closed.sort_by_key(|&(slot, s, _)| (s, slot));
        let mut pieces = Vec::new();
        for (slot, s, e) in closed {
            self.pieces(slot, s as f64 / 100.0, e as f64 / 100.0, &mut pieces);
        }
        self.assign_ordinals(&pieces);
        self.runs.closed.extend(pieces);
    }

    /// The runs still going that are long enough to be turns, as pieces on
    /// the session clock, by start.
    fn open_turns(&self) -> Vec<(u8, f64, f64)> {
        let len = self.runs.scanned;
        let mut runs: Vec<(usize, u8)> = (0..8u8)
            .filter_map(|slot| self.runs.run_start[slot as usize].map(|s| (s, slot)))
            .filter(|&(s, _)| len - s >= MIN_TURN_FRAMES)
            .collect();
        runs.sort_unstable();
        let mut out = Vec::new();
        for (s, slot) in runs {
            self.pieces(slot, s as f64 / 100.0, len as f64 / 100.0, &mut out);
        }
        out
    }

    /// Numbers each slot not yet numbered, in the order of `turns`.
    fn assign_ordinals(&mut self, turns: &[(u8, f64, f64)]) {
        for &(slot, _, _) in turns {
            let next = self.ordinals.len() as u32 + 1;
            self.ordinals.entry(slot).or_insert(next);
        }
    }

    /// Every turn so far, closed and open, by start then slot.
    fn turns(&mut self) -> Vec<(u8, f64, f64)> {
        let open = self.open_turns();
        self.assign_ordinals(&open);
        let mut out = self.runs.closed.clone();
        out.extend(open);
        out.sort_by(|a, b| a.1.total_cmp(&b.1).then(a.0.cmp(&b.0)));
        out
    }

    fn name_of(&self, slot: u8) -> NameOut {
        let voices = self.voices.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(v) = self.shown.get(&slot).and_then(|c| voices.get(c.voice)) {
            return NameOut::Voice {
                voice_id: v.id.clone(),
                display_name: if v.is_me { "Me".into() } else { v.display_name.clone() },
            };
        }
        NameOut::Anonymous { ordinal: self.ordinals.get(&slot).copied().unwrap_or(0) }
    }

    /// Seconds of audio the stream has taken in.
    pub fn overlay_audio_seconds(&self) -> f64 {
        self.diar.audio_seconds()
    }

    fn first_turns(&self) -> Vec<OverlayTurn> {
        let mut out: Vec<OverlayTurn> = self
            .first
            .turns
            .iter()
            .map(|(&slot, &(a, b))| OverlayTurn {
                speaker: slot as u32,
                start_s: self.session_time(a as f64 / 100.0),
                end_s: self.session_time(b as f64 / 100.0 - 1.0 / 16_000.0) + 1.0 / 16_000.0,
            })
            .collect();
        out.sort_by(|x, y| x.start_s.total_cmp(&y.start_s));
        out
    }

    /// The overlay file for the stream so far.
    pub fn overlay(&mut self, model: &str, failure: Option<&str>) -> Overlay {
        let turns = self.turns();
        let speakers = {
            let mut s: Vec<u8> = turns.iter().map(|t| t.0).collect();
            s.sort_unstable();
            s.dedup();
            s.len() as u32
        };
        let voices = self.voices.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let mut names: Vec<OverlayName> = self
            .ordinals
            .iter()
            .map(|(&slot, &ordinal)| OverlayName {
                slot: slot as u32,
                ordinal,
                voice_id: self.shown.get(&slot).and_then(|c| voices.get(c.voice)).map(|v| v.id.clone()),
            })
            .collect();
        names.sort_by_key(|n| n.ordinal);
        Overlay {
            diarizer: "nemotron".into(),
            model: model.into(),
            mode: "streaming".into(),
            load_seconds: self.load_seconds,
            audio_seconds: self.diar.audio_seconds(),
            origin_s: 0.0,
            updates: self.updates,
            speakers,
            first_label_latency_mean_s: self.first.mean_latency(),
            failure: failure.map(str::to_string),
            turns: turns
                .into_iter()
                .map(|(slot, start_s, end_s)| OverlayTurn { speaker: slot as u32, start_s, end_s })
                .collect(),
            first_turns: self.first_turns(),
            names,
            embeddings: self.embedder.is_some().then_some(self.windows),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::live_names::naming::Voice;
    use providers_local::nemotron3_diar::turns_from_frames;

    fn shared(v: Vec<Voice>) -> Voices {
        std::sync::Arc::new(std::sync::Mutex::new(v))
    }

    /// Scripted diarizer: reports the masks it was told to, one frame per 160
    /// samples pushed.
    struct Script {
        masks: Vec<u8>,
        frames: Vec<u8>,
        fed: usize,
    }

    impl Script {
        fn new(masks: Vec<u8>) -> Self {
            Self { masks, frames: vec![], fed: 0 }
        }
    }

    impl Diarize for Script {
        fn push(&mut self, samples: &[f32]) -> Result<usize> {
            self.fed += samples.len();
            let want = self.fed / FRAME_SAMPLES;
            let before = self.frames.len();
            while self.frames.len() < want {
                let i = self.frames.len();
                self.frames.push(self.masks.get(i).copied().unwrap_or(0));
            }
            Ok(self.frames.len() - before)
        }
        fn finish(&mut self) -> Result<usize> {
            Ok(0)
        }
        fn frames(&self) -> &[u8] {
            &self.frames
        }
        fn audio_seconds(&self) -> f64 {
            self.fed as f64 / 16_000.0
        }
    }

    /// Embeds a window as the unit vector whose hot index is its first sample.
    struct ByFirstSample;

    impl Embed for ByFirstSample {
        fn embed(&mut self, samples: &[f32]) -> Result<Vec<f32>> {
            let mut v = vec![0.0; 8];
            v[samples[0] as usize] = 1.0;
            Ok(v)
        }
    }

    fn voice(id: &str, name: &str, hot: usize, is_me: bool) -> Voice {
        let mut print = vec![0.0; 8];
        print[hot] = 1.0;
        Voice { id: id.into(), display_name: name.into(), is_me, print }
    }

    #[test]
    fn slots_without_saved_voices_read_speaker_n_by_first_appearance() {
        let mut masks = vec![0b1000u8; 100];
        masks.extend(vec![0b10u8; 100]);
        let mut e: Engine<Script, ByFirstSample> = Engine::new("system", Script::new(masks), None, shared(vec![]), None);
        let up = e.push(0, 0, &vec![0.0; 32_000]).unwrap().unwrap();
        assert_eq!(up.track, "system");
        assert_eq!(up.tracks.len(), 2);
        assert_eq!(up.tracks[0].slot, 1);
        assert_eq!(up.tracks[0].name, NameOut::Anonymous { ordinal: 2 });
        assert_eq!(up.tracks[1].name, NameOut::Anonymous { ordinal: 1 });
        assert!((up.tracks[1].turns[0].start_s - 0.0).abs() < 1e-9);
    }

    #[test]
    fn a_ten_second_window_names_the_slot_after_a_saved_voice() {
        let masks = vec![0b100u8; 1_000]; // slot 2 alone for 10 s
        let voices = vec![voice("v-ana", "Ana Okafor", 5, false), voice("v-me", "Dev", 4, true)];
        let mut e = Engine::new("system", Script::new(masks), Some(ByFirstSample), shared(voices), Some(0.3));
        // The window's first sample is 5: the embedder reads it as voice 5.
        let mut audio = vec![0.0f32; 160_000];
        audio[0] = 5.0;
        let mut last = None;
        for part in audio.chunks(16_000) {
            if let Some(u) = e.push(0, 0, part).unwrap() {
                last = Some(u);
            }
        }
        let last = last.unwrap();
        assert_eq!(last.tracks[0].name, NameOut::Voice { voice_id: "v-ana".into(), display_name: "Ana Okafor".into() });
        let o = e.overlay("nemotron3/fast32", None);
        assert_eq!(o.embeddings, Some(1));
        assert_eq!(o.names[0].voice_id.as_deref(), Some("v-ana"));
    }

    #[test]
    fn the_owner_voice_reads_me() {
        let masks = vec![0b1u8; 1_000];
        let voices = vec![voice("v-me", "Dana Whitfield", 4, true)];
        let mut e = Engine::new("system", Script::new(masks), Some(ByFirstSample), shared(voices), None);
        let mut audio = vec![0.0f32; 160_000];
        audio[0] = 4.0;
        let mut last = None;
        for part in audio.chunks(16_000) {
            last = e.push(0, 0, part).unwrap().or(last);
        }
        assert_eq!(
            last.unwrap().tracks[0].name,
            NameOut::Voice { voice_id: "v-me".into(), display_name: "Me".into() }
        );
    }

    #[test]
    fn a_pause_shifts_later_turns_on_the_session_clock() {
        let masks = vec![1u8; 200];
        let mut e: Engine<Script, ByFirstSample> = Engine::new("system", Script::new(masks), None, shared(vec![]), None);
        e.push(0, 0, &vec![0.0; 16_000]).unwrap();
        let up = e.push(30_000, 0, &vec![0.0; 16_000]).unwrap().unwrap();
        let t = &up.tracks[0].open;
        assert_eq!(t.len(), 2);
        assert!((t[0].end_s - 1.0).abs() < 1e-3 && (t[1].start_s - 30.0).abs() < 1e-3, "{t:?}");
    }

    #[test]
    fn first_labels_give_each_slots_first_turn_as_emitted_and_the_per_frame_mean_delay() {
        let mut masks = vec![0u8; 100];
        masks.extend(vec![0b1u8; 200]);
        masks.extend(vec![0u8; 100]);
        masks.extend(vec![0b100u8; 200]);
        let mut e: Engine<Script, ByFirstSample> = Engine::new("system", Script::new(masks), None, shared(vec![]), None);
        for i in 0..7 {
            e.push(i * 1000, 0, &vec![0.0; 16_000]).unwrap();
        }
        let o = e.overlay("nemotron3/fast32", None);
        assert_eq!(o.first_turns.len(), 2);
        assert_eq!((o.first_turns[0].speaker, o.first_turns[1].speaker), (0, 2));
        assert!((o.first_turns[0].start_s - 1.0).abs() < 1e-6 && (o.first_turns[0].end_s - 2.0).abs() < 1e-6);
        assert!((o.first_turns[1].start_s - 4.0).abs() < 1e-6 && (o.first_turns[1].end_s - 5.0).abs() < 1e-6);
        assert!((o.first_label_latency_mean_s.unwrap() - 0.505).abs() < 1e-6);
        assert!((o.turns[0].end_s - 3.0).abs() < 1e-6);
    }

    #[test]
    fn first_label_latency_averages_frames_not_slots() {
        let mut masks = vec![0b1u8; 1_000];
        masks.extend(vec![0b10u8; 30]);
        let mut e: Engine<Script, ByFirstSample> = Engine::new("system", Script::new(masks), None, shared(vec![]), None);
        for i in 0..11 {
            e.push(i * 1000, 0, &vec![0.0; 16_000]).unwrap();
        }
        let o = e.overlay("nemotron3/fast32", None);
        assert_eq!(o.first_turns.len(), 2);
        let want = (1_000.0 * 0.505 + 30.0 * 0.855) / 1_030.0;
        let got = o.first_label_latency_mean_s.unwrap();
        assert!((got - want).abs() < 1e-6, "{got} vs {want}");
        assert!((got - 1.0).abs() > 0.4, "a mean over slots' first turns would read 1.0");
    }

    #[test]
    fn no_labels_leave_the_latency_absent() {
        let mut e: Engine<Script, ByFirstSample> =
            Engine::new("system", Script::new(vec![0u8; 200]), None, shared(vec![]), None);
        e.push(0, 0, &vec![0.0; 32_000]).unwrap();
        let o = e.overlay("nemotron3/fast32", None);
        assert!(o.first_label_latency_mean_s.is_none() && o.first_turns.is_empty());
    }

    #[test]
    fn no_change_emits_nothing() {
        let masks = vec![1u8; 400];
        let mut e: Engine<Script, ByFirstSample> = Engine::new("system", Script::new(masks), None, shared(vec![]), None);
        assert!(e.push(0, 0, &vec![0.0; 16_000]).unwrap().is_some());
        assert!(e.push(1_000, 0, &vec![0.0; 16_000]).unwrap().is_some());
        let mut quiet: Engine<Script, ByFirstSample> =
            Engine::new("system", Script::new(vec![0u8; 400]), None, shared(vec![]), None);
        quiet.push(0, 0, &vec![0.0; 32_000]).unwrap();
        assert!(quiet.push(2_000, 0, &vec![0.0; 16_000]).unwrap().is_none());
    }

    /// Masks in which slots 0..3 take turns of varying length, with gaps
    /// shorter and longer than a turn.
    fn busy_masks() -> Vec<u8> {
        let mut m = Vec::new();
        for i in 0..200usize {
            let slot = (i * 7 % 4) as u8;
            let len = 5 + (i * 13 % 60);
            m.extend(std::iter::repeat_n(1u8 << slot, len));
            m.extend(std::iter::repeat_n(0u8, i % 3 * 10));
        }
        m
    }

    #[test]
    fn updates_carry_new_closed_turns_and_the_open_one_and_add_up_to_the_batch_turns() {
        let masks = busy_masks();
        let frames = masks.len();
        let mut e: Engine<Script, ByFirstSample> = Engine::new("system", Script::new(masks.clone()), None, shared(vec![]), None);
        let mut closed: BTreeMap<u32, Vec<(f64, f64)>> = BTreeMap::new();
        let mut open: BTreeMap<u32, Vec<(f64, f64)>> = BTreeMap::new();
        let mut biggest = 0;
        let mut updates = 0;
        let mut apply = |u: LiveSpeakers| {
            updates += 1;
            if u.full {
                closed.clear();
            }
            for t in u.tracks {
                biggest = biggest.max(t.turns.len());
                closed.entry(t.slot).or_default().extend(t.turns.iter().map(|x| (x.start_s, x.end_s)));
                open.insert(t.slot, t.open.iter().map(|x| (x.start_s, x.end_s)).collect());
            }
        };
        for i in 0..(frames * FRAME_SAMPLES).div_ceil(16_000) {
            if let Some(u) = e.push(i as u32 * 1000, 0, &vec![0.0; 16_000]).unwrap() {
                apply(u);
            }
        }
        if let Some(u) = e.finish().unwrap() {
            apply(u);
        }
        let mut got: Vec<(u8, f64, f64)> = closed
            .iter()
            .chain(open.iter())
            .flat_map(|(&slot, ts)| ts.iter().map(move |&(a, b)| (slot as u8, a, b)))
            .collect();
        got.sort_by(|a, b| a.1.total_cmp(&b.1).then(a.0.cmp(&b.0)));
        let mut padded = masks.clone();
        padded.resize(e.diar.frames().len(), 0);
        let want: Vec<(u8, f64, f64)> =
            turns_from_frames(&padded).iter().map(|t| (t.slot, t.start_s, t.end_s)).collect();
        assert_eq!(got.len(), want.len());
        for (g, w) in got.iter().zip(&want) {
            assert!(g.0 == w.0 && (g.1 - w.1).abs() < 1e-6 && (g.2 - w.2).abs() < 1e-6, "{g:?} vs {w:?}");
        }
        assert!(updates > FULL_EVERY as usize, "{updates}");
        assert!(want.len() > 40 && biggest < want.len() / 2, "{biggest} of {}", want.len());
    }

    #[test]
    fn a_locked_vault_drops_the_names_and_the_encoder() {
        let masks = vec![0b100u8; 1_500];
        let voices = shared(vec![voice("v-ana", "Ana Okafor", 5, false)]);
        let mut e = Engine::new("system", Script::new(masks), Some(ByFirstSample), voices.clone(), None);
        let mut audio = vec![0.0f32; 160_000];
        audio[0] = 5.0;
        let mut last = None;
        for part in audio.chunks(16_000) {
            last = e.push(0, 0, part).unwrap().or(last);
        }
        assert!(matches!(last.unwrap().tracks[0].name, NameOut::Voice { .. }));
        voices.lock().unwrap().clear();
        let up = e.forget_names_without_voices().expect("the name change is an update");
        assert_eq!(up.tracks[0].name, NameOut::Anonymous { ordinal: 1 });
        assert!(!e.names_voices());
        let o = e.overlay("nemotron3/fast32", None);
        assert_eq!(o.names[0].voice_id, None);
        assert_eq!(o.embeddings, None);
        let more = vec![5.0f32; 16_000];
        for _ in 0..4 {
            if let Some(u) = e.push(0, 0, &more).unwrap() {
                assert_eq!(u.tracks[0].name, NameOut::Anonymous { ordinal: 1 });
            }
        }
    }
}
