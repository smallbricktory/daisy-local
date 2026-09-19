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
    /// so no software pass runs at finalize. Distinct from `Disabled`: the
    /// audio IS echo-cancelled.
    Capture,
    /// A mode written by a newer build than this one. Never written at
    /// record time. Reads as "processing state unidentifiable": no software
    /// AEC pass may run over the audio (a second pass over already-cancelled
    /// audio is destructive; skipping one is not).
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

/// One contiguous capture window. A new segment opens on Resume.
/// Chunk indices are monotonic across segments (segment N may start at chunk 7).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordingSegment {
    pub started_at_unix_seconds: i64,
    pub stopped_at_unix_seconds: Option<i64>,
    pub first_chunk_index: u32,
    pub last_chunk_index: Option<u32>,
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
    /// skipped for it. Per chunk rather than per session: a mid-session mic
    /// failover onto a device VPIO refuses splits at a chunk boundary.
    #[serde(default)]
    pub mic_voice_processed: bool,

    /// Exactly-zero mic samples in this chunk, and the longest unbroken run
    /// of them. Against `mic_samples` this is the measurement that identifies
    /// a capture the OS zeroed — reconstructible from a manifest alone, which
    /// a bug report can carry and the audio cannot.
    #[serde(default)]
    pub mic_zero_samples: Option<u64>,
    #[serde(default)]
    pub mic_longest_zero_run: Option<u64>,

    /// Keys another Daisy client wrote that this build does not declare;
    /// carried through a rewrite untouched.
    #[serde(flatten, default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    /// How far apart two different people sat in this session, as the largest
    /// mean window cosine between any two clusters. Capture conditions set the
    /// scale, so the one-voice check reads it back rather than a fixed number.
    #[serde(default)]
    pub speaker_separation: Option<f32>,

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

impl SessionManifest {
    pub const SCHEMA: u32 = 4;

    /// Every version from v2 up reads: each one since adds only defaulted
    /// fields, and a newer client's additions ride in `extra`. v1 predates
    /// fields that carry no usable default.
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

    /// `chunk_index -> ms from the meeting start to that chunk's start`.
    /// Offsets clamp to 7 days — a corrupt chunk timestamp must not blow up
    /// the rendered timeline.
    pub fn chunk_offsets_ms(&self) -> std::collections::HashMap<u32, u32> {
        const MAX_OFFSET_SECS: i64 = 7 * 86_400;
        self.chunks
            .iter()
            .map(|c| {
                let secs = (c.started_at_unix_seconds - self.created_at_unix_seconds)
                    .clamp(0, MAX_OFFSET_SECS) as u64;
                (c.index, (secs.saturating_mul(1000)).min(u32::MAX as u64) as u32)
            })
            .collect()
    }
}

/// Energy-gate-relevant manifest fields, tolerant of partial/older
/// manifests: the mic device description and whether the mic carries a
/// single local speaker (absent = true, matching the manifest default).
pub struct ManifestGateProbe {
    pub mic_description: Option<String>,
    pub single_local_speaker: bool,
    /// The capture backend behind every chunk, when they agree. A manifest
    /// with no chunks reads as raw capture, matching how the absent field is
    /// interpreted everywhere else. `None` only for a session whose chunks
    /// mix backends (a mid-session failover): its measured levels span two
    /// domains and must not seed either.
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
        // A synced profile routinely puts newer-build manifests in front of
        // older builds; an unrecognized mode must not fail the whole parse.
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
            mixed_clusters: vec![], cluster_sample_spans: vec![],
            speaker_separation: None,
            interrupted: false,
            denoise_applied: None,
            dismissed_suggested_tag_ids: vec![],
        }
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
        "ended_at_unix_seconds": 1787249868,
        "index": 1,
        "mic_host_ns": 327071631296500,
        "mic_samples": 150393,
        "mic_wav_relative": "chunks/0001/mic.wav",
        "started_at_unix_seconds": 1787249858,
        "phone_only_chunk_key": 7
      }],
      "created_at_unix_seconds": 1787249858,
      "finalized_at_unix_seconds": 1787249868,
      "interrupted": false,
      "recording_segments": [{
        "first_chunk_index": 1,
        "last_chunk_index": 1,
        "started_at_unix_seconds": 1787249858,
        "stopped_at_unix_seconds": 1787249868,
        "reason": "user"
      }],
      "dismissed_suggested_tag_ids": ["t1"],
      "sample_rate": 16000,
      "schema_version": 4,
      "session_id": "daisy-1787249858",
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

    /// The probe parses the manifest with its own minimal structs, so a
    /// rename on `ChunkManifest` would compile clean and silently make every
    /// chunk read as raw. Round-tripping a real manifest pins the coupling.
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
}
