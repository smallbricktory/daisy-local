// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! What the session screen shows after Stop while the transcript has no
//! speakers yet: the overlay files' turns and names, plus the transcript's
//! lines on the session clock to label them against. Finalize's speakers
//! replace it.

use serde::Serialize;

use super::naming::Voice;
use super::overlay::{self, Overlay, Track};
use super::{NameOut, SlotOut, TurnOut};
use crate::state::AppState;

/// One transcript line on the session clock.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LineOut {
    pub track: &'static str,
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
}

/// The overlay of an undiarized session. Voice prints and embeddings stay in
/// the files.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct UndiarizedNames {
    pub system: Vec<SlotOut>,
    pub mic: Vec<SlotOut>,
    /// The mic carries one voice, the local user's.
    pub single_local_speaker: bool,
    pub lines: Vec<LineOut>,
}

/// Slots of one overlay with their turns and shown names. A slot whose saved
/// voice is unknown to this unlock (locked vault, deleted voice) reads by its
/// ordinal.
pub fn slots(o: &Overlay, voices: &[Voice]) -> Vec<SlotOut> {
    let mut slots: Vec<u32> = o.turns.iter().map(|t| t.speaker).collect();
    slots.sort_unstable();
    slots.dedup();
    slots
        .into_iter()
        .map(|slot| {
            let name = o.names.iter().find(|n| n.slot == slot);
            let voice = name.and_then(|n| n.voice_id.as_deref()).and_then(|id| voices.iter().find(|v| v.id == id));
            let name = match (voice, name) {
                (Some(v), _) => NameOut::Voice {
                    voice_id: v.id.clone(),
                    display_name: if v.is_me { "Me".into() } else { v.display_name.clone() },
                },
                (None, Some(n)) => NameOut::Anonymous { ordinal: n.ordinal },
                (None, None) => NameOut::Anonymous { ordinal: 0 },
            };
            SlotOut {
                slot,
                turns: o
                    .turns
                    .iter()
                    .filter(|t| t.speaker == slot)
                    .map(|t| TurnOut { start_s: t.start_s, end_s: t.end_s })
                    .collect(),
                name,
            }
        })
        .collect()
}

/// The overlay files of `session_id` (`voices` loads the saved voices once an overlay is found), only while its transcript has speech
/// and no speakers, or while it has no transcript yet (`lines` is then empty).
/// `None` when the session is diarized, or holds no overlay turns.
pub fn read(app: &AppState, session_id: &str, voices: &dyn Fn() -> Vec<Voice>) -> Option<UndiarizedNames> {
    if !crate::commands::session_store::is_safe_session_id(session_id) {
        return None;
    }
    let root = app.profile.session_path(session_id);
    let no_transcript = crate::commands::session_store::preferred_transcript_path(&root).is_none();
    if !no_transcript && !crate::commands::voiceprints::transcript_undiarized(app, session_id) {
        return None;
    }
    let (system, mic) = (overlay::read(&root, Track::System), overlay::read(&root, Track::Mic));
    if system.is_none() && mic.is_none() {
        return None;
    }
    let voices = voices();
    let system = system.map(|o| slots(&o, &voices)).unwrap_or_default();
    let mic = mic.map(|o| slots(&o, &voices)).unwrap_or_default();
    if system.is_empty() && mic.is_empty() {
        return None;
    }
    let single_local_speaker =
        crate::commands::session_store::read_manifest_opt(&root).map(|m| m.single_local_speaker).unwrap_or(true);
    Some(UndiarizedNames { system, mic, single_local_speaker, lines: lines(&root) })
}

/// The transcript's lines with chunk offsets applied, in start order.
fn lines(root: &std::path::Path) -> Vec<LineOut> {
    let Some(path) = crate::commands::session_store::preferred_transcript_path(root) else { return vec![] };
    let Some(st) = syncsafe::read(&path).ok().and_then(|b| serde_json::from_slice::<transcript::SessionTranscript>(&b).ok())
    else {
        return vec![];
    };
    let offsets = crate::commands::pipeline::chunk_offsets_ms(root);
    let mut out = Vec::new();
    for ch in &st.chunks {
        let off = offsets.get(&ch.chunk_index).copied().unwrap_or(0) as u64;
        for tr in &ch.tracks {
            let track = if tr.track.is_mic() { "mic" } else { "system" };
            for seg in &tr.segments {
                if seg.text.trim().is_empty() {
                    continue;
                }
                out.push(LineOut {
                    track,
                    start_ms: off + seg.start_ms as u64,
                    end_ms: off + seg.end_ms as u64,
                    text: seg.text.trim().to_string(),
                });
            }
        }
    }
    out.sort_by_key(|l| l.start_ms);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::live_names::overlay::{OverlayName, OverlayTurn};

    fn overlay_with(names: Vec<OverlayName>) -> Overlay {
        let mut o = Overlay::failed("nemotron3/fast32", "x");
        o.failure = None;
        o.turns = vec![
            OverlayTurn { speaker: 2, start_s: 0.0, end_s: 4.0 },
            OverlayTurn { speaker: 0, start_s: 4.0, end_s: 9.0 },
            OverlayTurn { speaker: 2, start_s: 9.0, end_s: 12.0 },
        ];
        o.names = names;
        o
    }

    fn voice(id: &str, name: &str, is_me: bool) -> Voice {
        Voice { id: id.into(), display_name: name.into(), is_me, print: vec![] }
    }

    #[test]
    fn slots_carry_turns_and_names_without_prints() {
        let o = overlay_with(vec![
            OverlayName { slot: 2, ordinal: 1, voice_id: Some("v-ana".into()) },
            OverlayName { slot: 0, ordinal: 2, voice_id: None },
        ]);
        let s = slots(&o, &[voice("v-ana", "Ana Okafor", false)]);
        assert_eq!(s.iter().map(|x| x.slot).collect::<Vec<_>>(), vec![0, 2]);
        assert_eq!(s[0].name, NameOut::Anonymous { ordinal: 2 });
        assert_eq!(s[1].name, NameOut::Voice { voice_id: "v-ana".into(), display_name: "Ana Okafor".into() });
        assert_eq!(s[1].turns.len(), 2);
    }

    #[test]
    fn the_owner_voice_reads_me_and_an_unknown_voice_reads_by_ordinal() {
        let o = overlay_with(vec![
            OverlayName { slot: 2, ordinal: 1, voice_id: Some("v-me".into()) },
            OverlayName { slot: 0, ordinal: 2, voice_id: Some("v-gone".into()) },
        ]);
        let s = slots(&o, &[voice("v-me", "Bram Ilunga", true)]);
        assert_eq!(s[1].name, NameOut::Voice { voice_id: "v-me".into(), display_name: "Me".into() });
        assert_eq!(s[0].name, NameOut::Anonymous { ordinal: 2 });
    }

    fn app() -> (AppState, tempfile::TempDir) {
        let tmp = tempfile::tempdir().unwrap();
        let profile = crate::profile::ProfileDir::at(tmp.path()).unwrap();
        (AppState::new(profile), tmp)
    }

    fn write_transcript(root: &std::path::Path, speaker_id: Option<u32>) {
        let seg = |start_ms, end_ms, text: &str, speaker_id| transcript::Segment {
            start_ms,
            end_ms,
            text: text.into(),
            confidence: None,
            speaker_id,
            words: Vec::new(),
            split_group: None,
        };
        let st = transcript::SessionTranscript {
            schema_version: transcript::SessionTranscript::SCHEMA,
            session_id: "s".into(),
            provider: "local-whisper".into(),
            model: "base.en".into(),
            transcribed_at_unix_seconds: 0,
            provisional: false,
            edited_at_unix_seconds: None,
            chunks: vec![transcript::ChunkTranscript {
                chunk_index: 1,
                tracks: vec![transcript::TrackTranscript {
                    track: transcript::Track::System,
                    source_wav_relative: "chunks/0001/system.wav".into(),
                    segments: vec![seg(1000, 3000, "Welcome, everyone.", speaker_id)],
                }],
            }],
        };
        syncsafe::write(root.join("transcript.dedup.json"), serde_json::to_vec(&st).unwrap()).unwrap();
    }

    #[test]
    fn the_overlay_is_read_only_while_the_transcript_is_undiarized() {
        let (app, _tmp) = app();
        let sid = "daisy-test-names";
        let root = app.profile.session_path(sid);
        syncsafe::create_dir_all(&root).unwrap();
        overlay::write(&root, Track::System, &overlay_with(vec![OverlayName { slot: 2, ordinal: 1, voice_id: None }])).unwrap();
        let none = || -> Vec<Voice> { vec![] };

        let early = read(&app, sid, &none).expect("no transcript yet: the overlay alone");
        assert_eq!(early.system.len(), 2);
        assert!(early.lines.is_empty());

        write_transcript(&root, None);
        let got = read(&app, sid, &none).expect("undiarized with an overlay");
        assert_eq!(got.system.len(), 2);
        assert!(got.mic.is_empty());
        assert_eq!(got.lines.len(), 1);
        assert_eq!((got.lines[0].track, got.lines[0].start_ms), ("system", 1000));

        write_transcript(&root, Some(0));
        assert!(read(&app, sid, &none).is_none(), "speakers have landed");

        assert!(read(&app, "../escape", &none).is_none());
    }
}
