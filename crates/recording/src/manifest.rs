//! Session manifest: on-disk source of truth for a recording's structure.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase", from = "String")]
pub enum AecMode {
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
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChunkManifest {
    pub index: u32,
    pub started_at_unix_seconds: i64,
    pub ended_at_unix_seconds: Option<i64>,
    pub duration_seconds: Option<u64>,
    pub mic_wav_relative: PathBuf,
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
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionManifest {
    pub schema_version: u32,
    pub session_id: String,
    pub created_at_unix_seconds: i64,
    pub sample_rate: u32,
    pub channels: u32,
    pub mic_source_id: u32,
    pub mic_source_node_name: String,
    pub mic_source_description: String,
    pub system_source_id: u32,
    pub system_source_node_name: String,
    pub system_source_description: String,
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
    pub const SCHEMA: u32 = 3;

    /// v3 only adds `serde(default)` fields, so a v2 manifest reads correctly
    /// and its sessions keep working. v1 predates fields that carry no usable
    /// default.
    pub fn schema_is_supported(&self) -> bool {
        (2..=Self::SCHEMA).contains(&self.schema_version)
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
            interrupted: false,
            denoise_applied: None,
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
    fn v2_and_v3_are_both_supported() {
        let mut m = sample_v2();
        assert!(m.schema_is_supported());
        m.schema_version = 3;
        assert!(m.schema_is_supported());
        m.schema_version = 4;
        assert!(!m.schema_is_supported());
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

    /// The probe parses the manifest with its own minimal structs, so a
    /// rename on `ChunkManifest` would compile clean and silently make every
    /// chunk read as raw. Round-tripping a real manifest pins the coupling.
    #[test]
    fn gate_probe_reads_a_real_manifest() {
        fn chunk(index: u32, voice_processed: bool) -> ChunkManifest {
            ChunkManifest {
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
