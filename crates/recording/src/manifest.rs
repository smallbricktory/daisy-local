// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Session manifest: on-disk source of truth for a recording's structure.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase", from = "String")]
pub enum AecMode {
    #[default]
    Disabled,
    Always,
    /// Echo cancellation happened during capture (macOS VoiceProcessingIO),
    /// so no software pass runs at finalize.
    Capture,
    /// A mode written by a newer build than this one. Never written at
    /// record time. Reads as "processing state unidentifiable": no software
    /// AEC pass runs over the audio.
    Unknown,
}

impl From<String> for AecMode {
    fn from(s: String) -> Self {
        match s.as_str() {
            "disabled" => AecMode::Disabled,
            "always" => AecMode::Always,
            "capture" => AecMode::Capture,
            _ => AecMode::Unknown,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AttendeeRole {
    /// The owner of this Daisy install (the "Me" track).
    #[serde(rename = "self")]
    Self_,
    /// Everyone else (the "Them" track / system audio).
    #[serde(rename = "other")]
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attendee {
    pub display_name: String,
    pub role: AttendeeRole,
}

/// Link to a calendar event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CalendarLink {
    pub provider: String,
    pub event_id: String,
    pub recurrence_id: Option<String>,
    pub planned_start_unix_seconds: i64,
    pub planned_end_unix_seconds: i64,
}

/// [`RecordingSegment::reason`] of a segment opened when the computer woke
/// from a sleep that paused the recording.
pub const SEGMENT_REASON_SYSTEM_SLEEP: &str = "system_sleep";

/// One contiguous capture window. A new segment opens on Resume.
/// Chunk indices are monotonic across segments (segment N may start at chunk 7).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordingSegment {
    pub started_at_unix_seconds: i64,
    pub stopped_at_unix_seconds: Option<i64>,
    pub first_chunk_index: u32,
    pub last_chunk_index: Option<u32>,
    /// Why the recording broke ahead of this segment: [`SEGMENT_REASON_SYSTEM_SLEEP`]
    /// from this build; the phone writes `user`, `call`, `route_change`,
    /// `media_reset` and `resumed`. Absent on a session's first segment and
    /// on a desktop pause the user resumed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Keys another Daisy client wrote that this build does not declare;
    /// carried through a rewrite untouched.
    #[serde(flatten, default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// Whole seconds from a JSON number: a phone writes exact float durations,
/// which read as their ceiling.
pub fn whole_seconds<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Option<u64>, D::Error> {
    let v: Option<f64> = Option::deserialize(d)?;
    Ok(v.map(|f| f.ceil() as u64))
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChunkManifest {
    pub index: u32,
    pub started_at_unix_seconds: i64,
    pub ended_at_unix_seconds: Option<i64>,
    /// Whole seconds. A phone writes the exact float; it reads as its
    /// ceiling, the same rounding `ended_at_unix_seconds` carries.
    #[serde(default, deserialize_with = "whole_seconds")]
    pub duration_seconds: Option<u64>,
    pub mic_wav_relative: PathBuf,
    /// Empty on a session with no system track (a phone recording).
    #[serde(default)]
    pub system_wav_relative: PathBuf,
    pub mic_aec_wav_relative: Option<PathBuf>,
    /// DFN3-denoised mic sidecar (feeds meeting.opus + mic diarization; never ASR).
    #[serde(default)]
    pub mic_dn_wav_relative: Option<PathBuf>,

    // ---- schema v3 ----
    /// Monotonic nanoseconds for the first sample written into this chunk,
    /// per track. `None` for chunks written before v3 or recovered from a
    /// partial capture, which pair by sample count instead.
    #[serde(default)]
    pub mic_host_ns: Option<u64>,
    #[serde(default)]
    pub system_host_ns: Option<u64>,
    /// Samples written into this chunk, per track. Not cumulative across the
    /// session.
    #[serde(default)]
    pub mic_samples: Option<u64>,
    #[serde(default)]
    pub system_samples: Option<u64>,

    /// True when the OS voice-processed this chunk's mic capture (macOS
    /// VoiceProcessingIO), so it arrived echo-cancelled and software AEC is
    /// skipped for it.
    #[serde(default)]
    pub mic_voice_processed: bool,

    /// Exactly-zero mic samples in this chunk, and the longest unbroken run
    /// of them.
    #[serde(default)]
    pub mic_zero_samples: Option<u64>,
    #[serde(default)]
    pub mic_longest_zero_run: Option<u64>,

    /// Samples the chunk holds on the session timeline: the longer of its
    /// mic and system WAVs, silence fills included. It is the span the chunk
    /// takes in `meeting.opus`. Read from the WAVs when the chunk closes and
    /// again at finalize.
    #[serde(default)]
    pub audio_samples: Option<u64>,

    /// Keys another Daisy client wrote that this build does not declare;
    /// carried through a rewrite untouched.
    #[serde(flatten, default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(remote = "Self")]
pub struct SessionManifest {
    pub schema_version: u32,
    pub session_id: String,
    pub created_at_unix_seconds: i64,
    pub sample_rate: u32,
    pub channels: u32,
    /// Source fields are absent on a phone recording (one microphone, no
    /// system track); 0 and "" mean no such source.
    #[serde(default)]
    pub mic_source_id: u32,
    #[serde(default)]
    pub mic_source_node_name: String,
    #[serde(default)]
    pub mic_source_description: String,
    #[serde(default)]
    pub system_source_id: u32,
    #[serde(default)]
    pub system_source_node_name: String,
    #[serde(default)]
    pub system_source_description: String,
    #[serde(default)]
    pub aec_mode: AecMode,
    pub chunks: Vec<ChunkManifest>,
    pub finalized_at_unix_seconds: Option<i64>,
    #[serde(default)]
    pub title: Option<String>,

    // ---- schema v2 ----
    #[serde(default = "default_meeting_id")]
    pub meeting_id: String,
    #[serde(default)]
    pub tag_ids: Vec<String>,
    #[serde(default)]
    pub notes_md_relative: Option<PathBuf>,
    #[serde(default)]
    pub attendees: Vec<Attendee>,
    #[serde(default)]
    pub calendar: Option<CalendarLink>,
    #[serde(default)]
    pub recording_segments: Vec<RecordingSegment>,
    /// Per-recording transcription language override (ISO code, e.g. "fr").
    /// `None` = use the app default. Applied at transcribe/regen time.
    #[serde(default)]
    pub language: Option<String>,

    /// True when the recording ended without a clean Stop (force-quit /
    /// crash): set when orphan recovery detects the session, cleared by a
    /// successful finalize. Drives the "interrupted" badge.
    #[serde(default)]
    pub interrupted: bool,

    /// Whether the DFN3 denoise stage ran at finalize (None = pre-denoise
    /// build or stage skipped via settings).
    #[serde(default)]
    pub denoise_applied: Option<bool>,

    /// Per-session mapping from a diarized speaker cluster (the integer
    /// `Segment::speaker_id`) to a human label and optional cross-session
    /// voiceprint id. Populated by the UI when the user labels a speaker;
    /// the same labels survive transcript regen.
    #[serde(default)]
    pub speaker_map: Vec<SpeakerLabel>,

    /// Diarization was attempted but the on-device WeSpeaker model was
    /// missing. Surfaces a user-facing banner on the session view.
    #[serde(default)]
    pub diarization_unavailable: bool,

    /// False = there is a group on the local (mic) end and the mic track is
    /// diarized like the system track. True (default) treats the mic as the
    /// local user, one known speaker. Set from the record-screen checkbox;
    /// read only at diarize time.
    #[serde(default = "default_true")]
    pub single_local_speaker: bool,

    /// Known speaker count for diarization. `None` = auto-estimate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_speakers: Option<u32>,

    /// Integrations this session has been successfully pushed to.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sent_integration_ids: Vec<String>,

    /// Per-cluster side tags (Room = mic, Remote = system) written by
    /// diarization. Empty for legacy/solo sessions, where every cluster is
    /// treated as Remote.
    #[serde(default)]
    pub cluster_sides: Vec<ClusterSide>,

    /// Clusters whose enrollment sample still reads as two voices after
    /// diarization's split pass. Written by diarization; enrollment refuses
    /// these and the UI warns before the user tries. Empty on sessions
    /// diarized by earlier builds.
    #[serde(default)]
    pub mixed_clusters: Vec<u32>,
    /// Per-cluster voice-sample windows written by diarization: the stretch of
    /// each of that cluster's transcript segments that its own speaker turn
    /// covers, session-absolute. Voice sampling reads these in place of whole
    /// segments. Absent on sessions diarized by earlier builds, which sample
    /// the segment span.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cluster_sample_spans: Vec<ClusterSampleSpans>,
    /// True when `cluster_sample_spans` sit on the timeline
    /// [`Self::chunk_offsets_ms`] describes. Diarization sets it with the
    /// windows it writes.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub sample_spans_on_audio_timeline: bool,
    /// How far apart two different people sat in this session, as the largest
    /// mean window cosine between any two clusters.
    #[serde(default)]
    pub speaker_separation: Option<f32>,
    /// How the last diarization run settled its speaker count. Written by
    /// every run that grouped voices; absent on sessions diarized by earlier
    /// builds and on solo-mic sessions that took one speaker from the setup.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diarize_count: Option<DiarizeCount>,
    /// The diarization run that numbered the clusters of a session with no
    /// `speakers.json` (a recording made here); a session with one reads the
    /// run that file names. Names in `meta.json` are read from the map under
    /// the current run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diarization_id: Option<String>,

    // ---- schema v4 ----
    /// Tag ids the person dismissed from the suggestion surfaces for this
    /// session. Undo removes an id.
    #[serde(default)]
    pub dismissed_suggested_tag_ids: Vec<String>,

    // ---- sessions from another Daisy client ----
    /// `"phone_room"` for a recording made on a phone: one microphone in a
    /// room, no system track, no AEC. Absent on desktop recordings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recording_kind: Option<String>,
    /// Opaque id of the paired device that recorded the session; the device
    /// its artifacts go back to. Absent on desktop recordings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_device: Option<String>,
    /// Keys another Daisy client wrote that this build does not declare;
    /// carried through a rewrite untouched.
    #[serde(flatten, default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// Which physical capture track a diarized cluster came from. `Room` = the
/// local microphone (multiple in-person speakers), `Remote` = the system
/// loopback (the far end of a call). Used only for presentation + diarize;
/// never for voiceprint matching.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum SpeakerSide {
    Room,
    #[default]
    Remote,
}

/// Per-session record of which side a diarized cluster belongs to. Populated
/// by `diarize_session_impl` for every cluster it creates, including
/// unlabeled ones.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClusterSide {
    pub cluster_id: u32,
    pub side: SpeakerSide,
}

/// Where a diarization run's speaker count came from, in the order the
/// sources are consulted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CountSource {
    /// The number typed in the re-diarize dialog, or `DAISY_DIARIZE_SPEAKERS`.
    Typed,
    /// Enrolled voiceprints heard across the speech.
    Voiceprints,
    /// The session's `expected_speakers`.
    Expected,
    /// The calendar event's attendees.
    Attendees,
    /// Estimated from the voices themselves.
    Auto,
}

/// The speaker count a diarization run settled on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiarizeCount {
    /// What set the count.
    pub source: CountSource,
    /// The number a person stated (`Typed` or `Expected`), as they stated it.
    /// `None` for the other sources.
    #[serde(default)]
    pub requested: Option<u32>,
    /// Speakers the run grouped on the track the count applied to.
    pub found: u32,
}

/// One cluster's voice-sample windows, session-absolute and chronological.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClusterSampleSpans {
    pub cluster_id: u32,
    /// `(start_ms, end_ms)` pairs.
    pub windows: Vec<(u32, u32)>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpeakerLabel {
    pub cluster_id: u32,
    pub display_name: String,
    #[serde(default)]
    pub email: Option<String>,
    /// Set when the user enrolls a voiceprint for this speaker.
    #[serde(default)]
    pub voiceprint_id: Option<String>,
    /// Cosine score from the auto-match that produced this label, if any.
    /// Used by the UI to decide whether a labeled cluster should still be
    /// surfaced for review (low-confidence matches are treated as unknown
    /// for the initial-tab routing). None means manually set or pre-existing.
    #[serde(default)]
    pub match_confidence: Option<f32>,
    /// Keys another Daisy client wrote that this build does not declare;
    /// carried through a rewrite untouched.
    #[serde(flatten, default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub extra: serde_json::Map<String, serde_json::Value>,
    /// The Contact this labeled cluster belongs to (set when a speaker is
    /// identified/enrolled). Optional + serde-default; old manifests load
    /// without it.
    #[serde(default)]
    pub contact_id: Option<String>,
}

fn default_meeting_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

fn default_true() -> bool {
    true
}

impl<'de> Deserialize<'de> for SessionManifest {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let mut m = SessionManifest::deserialize(d)?;
        m.normalize_chunks();
        Ok(m)
    }
}

impl Serialize for SessionManifest {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        if self.chunks_normalized() {
            return SessionManifest::serialize(self, s);
        }
        let mut m = self.clone();
        m.normalize_chunks();
        SessionManifest::serialize(&m, s)
    }
}

impl SessionManifest {
    pub const SCHEMA: u32 = 4;

    /// Puts `chunks` in index order and keeps one chunk per index: the first
    /// in manifest order. Returns the indices of the chunks dropped.
    pub fn normalize_chunks(&mut self) -> Vec<u32> {
        if self.chunks_normalized() {
            return Vec::new();
        }
        self.chunks.sort_by_key(|c| c.index);
        let mut dropped = Vec::new();
        self.chunks.dedup_by(|later, kept| {
            let same = later.index == kept.index;
            if same {
                dropped.push(later.index);
            }
            same
        });
        for index in &dropped {
            log::warn!("manifest {}: a second chunk {index} dropped", self.session_id);
        }
        dropped
    }

    /// True when `chunks` run in strictly increasing index order.
    fn chunks_normalized(&self) -> bool {
        self.chunks.windows(2).all(|w| w[0].index < w[1].index)
    }

    /// Every version from v2 up reads.
    pub fn schema_is_supported(&self) -> bool {
        self.schema_version >= 2
    }

    /// False for a recording with no system track (`recording_kind` =
    /// `"phone_room"`): no AEC pass, no system-track arms.
    pub fn has_system_track(&self) -> bool {
        self.recording_kind.as_deref() != Some("phone_room")
    }

    /// Diarized cluster_id → display name lookup for rendering. Empty when
    /// no speakers are labelled.
    pub fn speaker_display_map(&self) -> std::collections::HashMap<u32, String> {
        self.speaker_map
            .iter()
            .map(|s| (s.cluster_id, s.display_name.clone()))
            .collect()
    }

    /// `chunk_index -> ms from the meeting start to that chunk's start`:
    /// the audio of the chunks before it, back to back, as `meeting.opus`
    /// joins them. Paused spans and capture gaps hold no audio and take no
    /// time. Offsets clamp to 7 days.
    pub fn chunk_offsets_ms(&self) -> std::collections::HashMap<u32, u32> {
        self.chunk_spans_ms()
            .into_iter()
            .map(|(index, start, _)| (index, start))
            .collect()
    }

    /// `(chunk_index, start_ms, audio_ms)` per chunk in index order, on the
    /// timeline [`Self::chunk_offsets_ms`] describes. A chunk whose length
    /// is unknown ends where the next one started on the wall clock. No chunk
    /// starts or ends past 7 days.
    pub fn chunk_spans_ms(&self) -> Vec<(u32, u32, u64)> {
        const MAX_OFFSET_MS: u64 = 7 * 86_400 * 1000;
        let mut chunks: Vec<&ChunkManifest> = self.chunks.iter().collect();
        chunks.sort_by_key(|c| c.index);
        let mut cursor: u64 = 0;
        let mut out = Vec::with_capacity(chunks.len());
        for (i, c) in chunks.iter().enumerate() {
            let len = c.audio_ms(self.sample_rate).unwrap_or_else(|| {
                chunks.get(i + 1).map_or(0, |n| {
                    let secs = n.started_at_unix_seconds.saturating_sub(c.started_at_unix_seconds);
                    (secs.max(0) as u64).saturating_mul(1000)
                })
            });
            let start = cursor.min(MAX_OFFSET_MS);
            out.push((c.index, start as u32, len.min(MAX_OFFSET_MS - start)));
            cursor = cursor.saturating_add(len);
        }
        out
    }

    /// `chunk_index -> ms from the meeting start to that chunk's start` on
    /// the live transcript's timeline: the mic samples captured into the
    /// chunks before it, back to back. The live decode counts captured
    /// samples, so silence the WAV writer fills in for a capture stall is not
    /// on it; [`Self::chunk_offsets_ms`] is.
    pub fn capture_offsets_ms(&self) -> std::collections::HashMap<u32, u32> {
        self.capture_spans_ms().into_iter().map(|(i, start, _)| (i, start)).collect()
    }

    /// `(chunk_index, start_ms, mic_ms)` per chunk in index order, on the
    /// timeline [`Self::capture_offsets_ms`] describes. A chunk with no mic
    /// count takes its audio length. No chunk starts or ends past 7 days.
    pub fn capture_spans_ms(&self) -> Vec<(u32, u32, u64)> {
        const MAX_OFFSET_MS: u64 = 7 * 86_400 * 1000;
        let rate = u64::from(self.sample_rate.max(1));
        let lengths = self.chunk_spans_ms();
        let mut chunks: Vec<&ChunkManifest> = self.chunks.iter().collect();
        chunks.sort_by_key(|c| c.index);
        let mut cursor: u64 = 0;
        let mut out = Vec::with_capacity(chunks.len());
        for c in chunks {
            let len = match c.mic_samples {
                Some(n) => n.saturating_mul(1000) / rate,
                None => lengths.iter().find(|(i, _, _)| *i == c.index).map_or(0, |(_, _, l)| *l),
            };
            let start = cursor.min(MAX_OFFSET_MS);
            out.push((c.index, start as u32, len.min(MAX_OFFSET_MS - start)));
            cursor = cursor.saturating_add(len);
        }
        out
    }

    /// Milliseconds of mic audio the live decode heard: every chunk's
    /// captured mic samples back to back.
    pub fn capture_ms(&self) -> u64 {
        self.capture_spans_ms().iter().map(|(_, _, len)| len).sum()
    }

    /// Milliseconds of audio the session holds: every chunk back to back.
    pub fn audio_ms(&self) -> u64 {
        self.chunk_spans_ms().iter().map(|(_, _, len)| len).sum()
    }
}

impl ChunkManifest {
    /// Milliseconds of audio in this chunk, from what was written to disk:
    /// `audio_samples`, else the larger capture count, else
    /// `duration_seconds`. `None` when the chunk records none of them.
    pub fn audio_ms(&self, sample_rate: u32) -> Option<u64> {
        let rate = u64::from(sample_rate.max(1));
        self.audio_samples
            .or_else(|| self.mic_samples.into_iter().chain(self.system_samples).max())
            .map(|n| n.saturating_mul(1000) / rate)
            .or_else(|| self.duration_seconds.map(|s| s.saturating_mul(1000)))
    }
}

/// Energy-gate-relevant manifest fields, tolerant of partial/older
/// manifests: the mic device description and whether the mic carries a
/// single local speaker (absent = true, matching the manifest default).
pub struct ManifestGateProbe {
    pub mic_description: Option<String>,
    pub single_local_speaker: bool,
    /// The capture backend behind every chunk, when they agree. A manifest
    /// with no chunks reads as raw capture. `None` only for a session whose
    /// chunks mix backends.
    pub mic_voice_processed: Option<bool>,
}

/// Reads the gate fields from `<session_dir>/manifest.json`; a missing or
/// unparseable manifest yields the defaults (no device, single speaker, raw
/// capture).
pub fn manifest_gate_probe(session_dir: &std::path::Path) -> ManifestGateProbe {
    #[derive(Deserialize)]
    struct Probe {
        mic_source_description: Option<String>,
        single_local_speaker: Option<bool>,
        #[serde(default)]
        chunks: Vec<ProbeChunk>,
    }
    #[derive(Deserialize)]
    struct ProbeChunk {
        #[serde(default)]
        mic_voice_processed: bool,
    }
    let p = syncsafe::read(session_dir.join("manifest.json"))
        .ok()
        .and_then(|b| serde_json::from_slice::<Probe>(&b).ok());
    let mic_voice_processed = p.as_ref().and_then(|p| {
        // No chunks (pre-migration or chunkless) reads as raw capture.
        let first = p.chunks.first().is_some_and(|c| c.mic_voice_processed);
        p.chunks
            .iter()
            .all(|c| c.mic_voice_processed == first)
            .then_some(first)
    });
    ManifestGateProbe {
        mic_description: p.as_ref().and_then(|p| p.mic_source_description.clone()),
        single_local_speaker: p.and_then(|p| p.single_local_speaker).unwrap_or(true),
        mic_voice_processed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aec_mode_tolerates_variants_from_newer_builds() {
        // An unrecognized mode parses as Unknown.
        let m: AecMode = serde_json::from_str("\"reverb_cancel_2030\"").unwrap();
        assert_eq!(m, AecMode::Unknown);
        for (s, want) in [
            ("\"disabled\"", AecMode::Disabled),
            ("\"always\"", AecMode::Always),
            ("\"capture\"", AecMode::Capture),
        ] {
            assert_eq!(serde_json::from_str::<AecMode>(s).unwrap(), want);
        }
        // Unknown re-serializes as its own tag, which parses back as Unknown.
        let json = serde_json::to_string(&AecMode::Unknown).unwrap();
        assert_eq!(serde_json::from_str::<AecMode>(&json).unwrap(), AecMode::Unknown);
    }

    #[test]
    fn manifest_with_unknown_aec_mode_parses() {
        let mut m = sample_v2();
        m.aec_mode = AecMode::Capture;
        let mut v: serde_json::Value = serde_json::to_value(&m).unwrap();
        v["aec_mode"] = serde_json::json!("some_future_mode");
        let back: SessionManifest = serde_json::from_value(v).unwrap();
        assert_eq!(back.aec_mode, AecMode::Unknown);
    }

    fn sample_v2() -> SessionManifest {
        SessionManifest {
            recording_kind: None,
            origin_device: None,
            extra: Default::default(),
            schema_version: 2, session_id: "s".into(), created_at_unix_seconds: 0,
            sample_rate: 16000, channels: 1, mic_source_id: 1,
            mic_source_node_name: "m".into(), mic_source_description: "m".into(),
            system_source_id: 2, system_source_node_name: "s".into(),
            system_source_description: "s".into(), aec_mode: AecMode::Disabled,
            chunks: vec![], finalized_at_unix_seconds: None, title: None,
            meeting_id: "id".into(), tag_ids: vec![], notes_md_relative: None,
            attendees: vec![], calendar: None, recording_segments: vec![],
            speaker_map: vec![],
            language: None,
            diarization_unavailable: false,
            single_local_speaker: true,
            expected_speakers: None,
            sent_integration_ids: vec![],
            cluster_sides: vec![],
            mixed_clusters: vec![], cluster_sample_spans: vec![], sample_spans_on_audio_timeline: false,
            speaker_separation: None, diarize_count: None, diarization_id: None,
            interrupted: false,
            denoise_applied: None,
            dismissed_suggested_tag_ids: vec![],
        }
    }

    #[test]
    fn a_manifest_without_a_diarize_count_loads_and_one_with_it_round_trips() {
        let mut json = serde_json::to_value(sample_v2()).unwrap();
        assert!(json.get("diarize_count").is_none());
        assert_eq!(serde_json::from_value::<SessionManifest>(json.clone()).unwrap().diarize_count, None);
        json["diarize_count"] = serde_json::json!({ "source": "typed", "requested": 9, "found": 11 });
        let m: SessionManifest = serde_json::from_value(json).unwrap();
        let want = DiarizeCount { source: CountSource::Typed, requested: Some(9), found: 11 };
        assert_eq!(m.diarize_count, Some(want));
        let back: SessionManifest = serde_json::from_value(serde_json::to_value(&m).unwrap()).unwrap();
        assert_eq!(back.diarize_count, Some(want));
    }

    #[test]
    fn legacy_manifest_defaults_single_local_and_empty_sides() {
        // A v2 manifest JSON written before this feature has neither field.
        let m = sample_v2();
        let mut json = serde_json::to_value(&m).unwrap();
        json.as_object_mut().unwrap().remove("single_local_speaker");
        json.as_object_mut().unwrap().remove("cluster_sides");
        let back: SessionManifest = serde_json::from_value(json).unwrap();
        assert!(back.single_local_speaker, "legacy manifests default to solo (mic=you)");
        assert!(back.cluster_sides.is_empty());
    }

    #[test]
    fn speaker_side_defaults_to_remote() {
        assert_eq!(SpeakerSide::default(), SpeakerSide::Remote);
        assert_eq!(serde_json::to_string(&SpeakerSide::Room).unwrap(), "\"room\"");
        assert_eq!(serde_json::to_string(&SpeakerSide::Remote).unwrap(), "\"remote\"");
    }

    #[test]
    fn v2_manifest_roundtrips_with_new_fields() {
        let mut m = sample_v2();
        m.title = Some("T".into());
        m.tag_ids = vec!["tagid".into()];
        m.notes_md_relative = Some("notes.md".into());
        m.attendees = vec![
            Attendee { display_name: "Rowan".into(), role: AttendeeRole::Self_ },
            Attendee { display_name: "Sable".into(), role: AttendeeRole::Other },
        ];
        m.recording_segments = vec![RecordingSegment {
            reason: None,
            extra: Default::default(),
            started_at_unix_seconds: 1000, stopped_at_unix_seconds: Some(2000),
            first_chunk_index: 1, last_chunk_index: Some(6),
        }];
        let json = serde_json::to_string(&m).unwrap();
        let back: SessionManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(m, back);
        assert!(json.contains("\"role\":\"self\""));
        assert!(json.contains("\"role\":\"other\""));
    }

    #[test]
    fn schema_v1_is_not_supported() {
        let mut m = sample_v2();
        m.schema_version = 1;
        assert!(!m.schema_is_supported());
        assert!(sample_v2().schema_is_supported());
    }

    #[test]
    fn v2_v3_and_v4_are_both_supported() {
        let mut m = sample_v2();
        assert!(m.schema_is_supported());
        m.schema_version = 3;
        assert!(m.schema_is_supported());
        m.schema_version = 4;
        assert!(m.schema_is_supported());
    }

    /// The manifest a phone writes: schema 4, one mic track, no system
    /// source, no AEC, and keys this build does not declare.
    const PHONE_MANIFEST: &str = r#"{
      "audio_purged": false,
      "channels": 1,
      "chunks": [{
        "duration_seconds": 9.3995625,
        "ended_at_unix_seconds": 1700000010,
        "index": 1,
        "mic_host_ns": 327071631296500,
        "mic_samples": 150393,
        "mic_wav_relative": "chunks/0001/mic.wav",
        "started_at_unix_seconds": 1700000000,
        "phone_only_chunk_key": 7
      }],
      "created_at_unix_seconds": 1700000000,
      "finalized_at_unix_seconds": 1700000010,
      "interrupted": false,
      "recording_segments": [{
        "first_chunk_index": 1,
        "last_chunk_index": 1,
        "started_at_unix_seconds": 1700000000,
        "stopped_at_unix_seconds": 1700000010,
        "reason": "user"
      }],
      "dismissed_suggested_tag_ids": ["t1"],
      "sample_rate": 16000,
      "schema_version": 4,
      "session_id": "daisy-1700000000",
      "recording_kind": "phone_room",
      "origin_device": "dev-7f3a"
    }"#;

    #[test]
    fn phone_manifest_parses_without_desktop_sources() {
        let m: SessionManifest = serde_json::from_str(PHONE_MANIFEST).unwrap();
        assert!(m.schema_is_supported());
        assert_eq!(m.mic_source_id, 0);
        assert!(m.mic_source_node_name.is_empty());
        assert_eq!(m.system_source_id, 0);
        assert!(m.system_source_node_name.is_empty());
        assert!(m.system_source_description.is_empty());
        assert_eq!(m.aec_mode, AecMode::Disabled);
        assert_eq!(m.chunks.len(), 1);
        assert_eq!(m.chunks[0].system_wav_relative, PathBuf::new());
        assert_eq!(m.chunks[0].mic_wav_relative, PathBuf::from("chunks/0001/mic.wav"));
        assert_eq!(m.chunks[0].duration_seconds, Some(10), "9.3995625 s reads as its ceiling");
        assert_eq!(m.recording_kind.as_deref(), Some("phone_room"));
        assert_eq!(m.origin_device.as_deref(), Some("dev-7f3a"));
        assert!(!m.has_system_track());
    }

    #[test]
    fn phone_keys_survive_a_desktop_rewrite() {
        let m: SessionManifest = serde_json::from_str(PHONE_MANIFEST).unwrap();
        let v: serde_json::Value = serde_json::to_value(&m).unwrap();
        assert_eq!(v["audio_purged"], serde_json::json!(false));
        assert_eq!(v["dismissed_suggested_tag_ids"], serde_json::json!(["t1"]));
        assert_eq!(v["chunks"][0]["phone_only_chunk_key"], serde_json::json!(7));
        assert_eq!(v["recording_segments"][0]["reason"], serde_json::json!("user"));
        assert_eq!(v["schema_version"], serde_json::json!(4));
    }

    #[test]
    fn desktop_manifest_carries_no_origin_fields() {
        let m = sample_v2();
        assert!(m.has_system_track());
        let v: serde_json::Value = serde_json::to_value(&m).unwrap();
        assert!(v.get("recording_kind").is_none());
        assert!(v.get("origin_device").is_none());
    }

    #[test]
    fn schema_gate_reads_any_version_from_two_up() {
        let mut m = sample_v2();
        for v in [1u32] {
            m.schema_version = v;
            assert!(!m.schema_is_supported(), "v{v}");
        }
        for v in [2u32, 3, 4, 5, 99] {
            m.schema_version = v;
            assert!(m.schema_is_supported(), "v{v}");
        }
    }

    #[test]
    fn speaker_label_keeps_keys_it_does_not_declare() {
        let json = r#"{"cluster_id":3,"display_name":"Rowan","phone_clip_ms":1200}"#;
        let l: SpeakerLabel = serde_json::from_str(json).unwrap();
        let v: serde_json::Value = serde_json::to_value(&l).unwrap();
        assert_eq!(v["phone_clip_ms"], serde_json::json!(1200));
        assert_eq!(v["display_name"], serde_json::json!("Rowan"));
    }

    #[test]
    fn old_v1_json_still_deserializes_with_defaults() {
        // v1 manifests lack the new keys; serde(default) fills them.
        let v1 = r#"{"schema_version":1,"session_id":"s","created_at_unix_seconds":0,"sample_rate":16000,"channels":1,"mic_source_id":1,"mic_source_node_name":"m","mic_source_description":"m","system_source_id":2,"system_source_node_name":"s","system_source_description":"s","aec_mode":"always","chunks":[],"finalized_at_unix_seconds":null,"title":"old"}"#;
        let m: SessionManifest = serde_json::from_str(v1).unwrap();
        assert_eq!(m.title.as_deref(), Some("old"));
        assert!(m.tag_ids.is_empty());
        assert!(!m.meeting_id.is_empty()); // default_meeting_id() ran
    }

    #[test]
    fn v3_manifest_without_dismissed_field_reads_with_empty_default() {
        // A v3 manifest JSON written before this feature has no dismissed_suggested_tag_ids key.
        let m = sample_v2();
        let mut json = serde_json::to_value(&m).unwrap();
        json.as_object_mut().unwrap().remove("dismissed_suggested_tag_ids");
        let back: SessionManifest = serde_json::from_value(json).unwrap();
        assert!(back.dismissed_suggested_tag_ids.is_empty(), "missing dismissed_suggested_tag_ids defaults to empty");
        assert!(back.schema_is_supported());
    }

    #[test]
    fn dismissed_field_round_trips() {
        let mut m = sample_v2();
        m.dismissed_suggested_tag_ids = vec!["t1".into(), "t2".into()];
        let s = serde_json::to_string(&m).unwrap();
        let back: SessionManifest = serde_json::from_str(&s).unwrap();
        assert_eq!(back.dismissed_suggested_tag_ids, vec!["t1", "t2"]);
    }

    #[test]
    fn schema_constant_is_four() {
        assert_eq!(SessionManifest::SCHEMA, 4);
    }

    /// Round-tripping a real manifest through the probe.
    #[test]
    fn gate_probe_reads_a_real_manifest() {
        fn chunk(index: u32, voice_processed: bool) -> ChunkManifest {
            ChunkManifest {
                extra: Default::default(),
                index,
                started_at_unix_seconds: 0,
                ended_at_unix_seconds: Some(1),
                duration_seconds: Some(1),
                mic_wav_relative: "chunks/0001/mic.wav".into(),
                system_wav_relative: "chunks/0001/system.wav".into(),
                mic_aec_wav_relative: None,
                mic_dn_wav_relative: None,
                mic_host_ns: None,
                system_host_ns: None,
                mic_samples: None,
                system_samples: None,
                mic_voice_processed: voice_processed,
                mic_zero_samples: None,
                mic_longest_zero_run: None,
                audio_samples: None,
            }
        }
        let dir = std::env::temp_dir().join(format!("daisy-probe-{}", std::process::id()));
        let write = |m: &SessionManifest| {
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("manifest.json"), serde_json::to_vec(m).unwrap()).unwrap();
        };

        let mut m = sample_v2();
        m.mic_source_description = "Fictional USB Mic".into();
        m.chunks = vec![chunk(1, true), chunk(2, true)];
        write(&m);
        let p = manifest_gate_probe(&dir);
        assert_eq!(p.mic_description.as_deref(), Some("Fictional USB Mic"));
        assert_eq!(p.mic_voice_processed, Some(true));

        // A session that fell back mid-recording spans two capture domains.
        m.chunks = vec![chunk(1, true), chunk(2, false)];
        write(&m);
        assert_eq!(manifest_gate_probe(&dir).mic_voice_processed, None);

        // No chunks reads as raw, matching the absent-field default.
        m.chunks = vec![];
        write(&m);
        assert_eq!(manifest_gate_probe(&dir).mic_voice_processed, Some(false));
        let _ = std::fs::remove_dir_all(&dir);
    }
    /// A closed desktop chunk: wall stamps from `started` to `ended`, and
    /// `samples` of audio on both tracks.
    fn timed_chunk(index: u32, started: i64, ended: i64, samples: Option<u64>) -> ChunkManifest {
        ChunkManifest {
            extra: Default::default(), index, started_at_unix_seconds: started,
            ended_at_unix_seconds: Some(ended), duration_seconds: Some((ended - started) as u64),
            mic_wav_relative: format!("chunks/{index:04}/mic.wav").into(),
            system_wav_relative: format!("chunks/{index:04}/system.wav").into(),
            mic_aec_wav_relative: None, mic_dn_wav_relative: None, mic_host_ns: None,
            system_host_ns: None, mic_samples: samples, system_samples: samples,
            mic_voice_processed: false, mic_zero_samples: None, mic_longest_zero_run: None,
            audio_samples: None,
        }
    }

    fn timeline(created: i64, chunks: Vec<ChunkManifest>) -> SessionManifest {
        let mut m = sample_v2();
        m.created_at_unix_seconds = created;
        m.chunks = chunks;
        m
    }

    const SECOND: u64 = 16_000;

    #[test]
    fn a_pause_takes_no_time_on_the_timeline() {
        // 60 s recorded, paused 1000 s, 60 s more.
        let m = timeline(1000, vec![
            timed_chunk(1, 1000, 1060, Some(60 * SECOND)),
            timed_chunk(2, 2060, 2120, Some(60 * SECOND)),
        ]);
        let off = m.chunk_offsets_ms();
        assert_eq!(off[&1], 0);
        assert_eq!(off[&2], 60_000, "chunk 2 plays right after chunk 1 in meeting.opus");
        assert_eq!(m.audio_ms(), 120_000);
    }

    #[test]
    fn a_capture_gap_inside_a_chunk_takes_no_time() {
        // Chunk 3's wall span is 483 s; it holds 4316160 mic samples.
        let m = timeline(1_790_887_969, vec![
            timed_chunk(1, 1_790_887_970, 1_790_887_971, Some(14_720)),
            timed_chunk(2, 1_790_887_981, 1_790_888_281, Some(4_799_680)),
            timed_chunk(3, 1_790_888_281, 1_790_888_764, Some(4_316_160)),
            timed_chunk(4, 1_790_888_764, 1_790_889_064, Some(4_760_960)),
        ]);
        let off = m.chunk_offsets_ms();
        assert_eq!(off[&2], 920, "the 10 s pause after chunk 1 takes no time");
        assert_eq!(off[&3], 920 + 299_980);
        assert_eq!(off[&4], 920 + 299_980 + 269_760, "the 213 s sleep takes no time");
        assert_eq!(m.audio_ms(), 920 + 299_980 + 269_760 + 297_560);
    }

    #[test]
    fn pauses_and_several_gaps_add_up_to_the_audio() {
        // Chunk 1 loses 100 s to a gap, a pause follows, chunk 3 loses two
        // gaps of 50 s each.
        let m = timeline(0, vec![
            timed_chunk(1, 0, 300, Some(200 * SECOND)),
            timed_chunk(2, 900, 1000, Some(100 * SECOND)),
            timed_chunk(3, 1000, 1300, Some(200 * SECOND)),
            timed_chunk(4, 1300, 1310, Some(10 * SECOND)),
        ]);
        let off = m.chunk_offsets_ms();
        assert_eq!([off[&1], off[&2], off[&3], off[&4]], [0, 200_000, 300_000, 500_000]);
        assert_eq!(m.audio_ms(), 510_000);
    }

    #[test]
    fn the_written_length_wins_over_the_capture_counts() {
        // A stalled stream the WAV writer filled with silence: the file is
        // longer than what the capture delivered, and meeting.opus plays
        // the file.
        let mut c1 = timed_chunk(1, 0, 300, Some(250 * SECOND));
        c1.audio_samples = Some(300 * SECOND);
        let m = timeline(0, vec![c1, timed_chunk(2, 300, 310, Some(10 * SECOND))]);
        assert_eq!(m.chunk_offsets_ms()[&2], 300_000);
    }

    #[test]
    fn an_unmeasured_chunk_falls_back_to_its_duration_then_the_wall_clock() {
        let mut c1 = timed_chunk(1, 0, 60, None);
        c1.duration_seconds = Some(40);
        let mut c2 = timed_chunk(2, 100, 160, None);
        c2.duration_seconds = None;
        let m = timeline(0, vec![c1, c2, timed_chunk(3, 400, 410, Some(10 * SECOND))]);
        let off = m.chunk_offsets_ms();
        assert_eq!(off[&2], 40_000, "duration_seconds when nothing was counted");
        assert_eq!(off[&3], 40_000 + 300_000, "no length at all: up to the next chunk's start");
    }

    #[test]
    fn the_live_timeline_leaves_out_the_silence_the_writer_filled() {
        // Chunk 1's mic stalled; the writer filled 2 s of silence the live
        // decode never heard.
        let mut c1 = timed_chunk(1, 0, 300, Some(298 * SECOND));
        c1.audio_samples = Some(300 * SECOND);
        let m = timeline(0, vec![c1, timed_chunk(2, 300, 310, Some(10 * SECOND))]);
        assert_eq!(m.chunk_offsets_ms()[&2], 300_000, "meeting.opus plays the fill");
        assert_eq!(m.capture_offsets_ms()[&2], 298_000, "the live decode did not hear it");
    }

    fn chunk_json(index: u32, mic: &str) -> serde_json::Value {
        serde_json::json!({
            "index": index, "started_at_unix_seconds": 0, "mic_wav_relative": mic,
        })
    }

    fn manifest_json(chunks: Vec<serde_json::Value>) -> serde_json::Value {
        serde_json::json!({
            "schema_version": 4, "session_id": "daisy-1", "created_at_unix_seconds": 0,
            "sample_rate": 16000, "channels": 1, "chunks": chunks,
        })
    }

    #[test]
    fn a_second_chunk_under_one_index_is_dropped_and_the_first_kept() {
        let v = manifest_json(vec![
            chunk_json(1, "chunks/0001/mic.wav"),
            chunk_json(2, "chunks/0002/mic.wav"),
            chunk_json(1, "chunks/0001/other.wav"),
        ]);
        let mut m: SessionManifest = serde_json::from_value(v.clone()).unwrap();
        let mics: Vec<_> = m.chunks.iter().map(|c| c.mic_wav_relative.clone()).collect();
        assert_eq!(mics, vec![PathBuf::from("chunks/0001/mic.wav"), "chunks/0002/mic.wav".into()]);
        assert!(m.normalize_chunks().is_empty(), "a read manifest is already normal");

        m.chunks.push(m.chunks[0].clone());
        assert_eq!(m.normalize_chunks(), vec![1]);
        assert_eq!(m.chunks.len(), 2);
    }

    #[test]
    fn chunks_are_written_in_index_order() {
        let mut m: SessionManifest =
            serde_json::from_value(manifest_json(vec![chunk_json(1, "a.wav")])).unwrap();
        let mut c2 = m.chunks[0].clone();
        c2.index = 2;
        let mut dup = m.chunks[0].clone();
        dup.mic_wav_relative = "b.wav".into();
        m.chunks = vec![c2, m.chunks[0].clone(), dup];
        let v: serde_json::Value = serde_json::to_value(&m).unwrap();
        let written: Vec<(u64, &str)> = v["chunks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| (c["index"].as_u64().unwrap(), c["mic_wav_relative"].as_str().unwrap()))
            .collect();
        assert_eq!(written, vec![(1, "a.wav"), (2, "a.wav")]);
        assert_eq!(m.chunks.len(), 3, "writing leaves the value alone");
    }

    #[test]
    fn a_normal_manifest_round_trips_byte_identical() {
        let m: SessionManifest = serde_json::from_value(manifest_json(vec![
            chunk_json(1, "chunks/0001/mic.wav"),
            chunk_json(2, "chunks/0002/mic.wav"),
        ]))
        .unwrap();
        let bytes = serde_json::to_vec_pretty(&m).unwrap();
        let again: SessionManifest = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(serde_json::to_vec_pretty(&again).unwrap(), bytes);
    }

    #[test]
    fn no_chunk_starts_or_ends_past_seven_days() {
        const WEEK_MS: u64 = 7 * 86_400 * 1000;
        let mut c1 = timed_chunk(1, 0, 60, None);
        c1.audio_samples = Some((WEEK_MS - 1_000) * 16);
        c1.mic_samples = Some((WEEK_MS - 1_000) * 16);
        let mut c2 = timed_chunk(2, 60, 120, None);
        c2.audio_samples = Some(u64::MAX);
        c2.mic_samples = Some(u64::MAX);
        let c3 = timed_chunk(3, 120, 180, Some(60 * SECOND));
        let m = timeline(0, vec![c1, c2, c3]);
        for spans in [m.chunk_spans_ms(), m.capture_spans_ms()] {
            assert_eq!(spans[1], (2, (WEEK_MS - 1_000) as u32, 1_000), "{spans:?}");
            assert_eq!(spans[2], (3, WEEK_MS as u32, 0), "{spans:?}");
            assert!(spans.iter().all(|(_, start, len)| u64::from(*start) + len <= WEEK_MS));
        }
        assert_eq!(m.audio_ms(), WEEK_MS);
        assert_eq!(m.capture_ms(), WEEK_MS);
    }

    #[test]
    fn offsets_follow_index_order() {
        let m = timeline(0, vec![
            timed_chunk(2, 60, 120, Some(60 * SECOND)),
            timed_chunk(1, 0, 60, Some(30 * SECOND)),
        ]);
        assert_eq!(m.chunk_offsets_ms()[&2], 30_000);
    }

}
