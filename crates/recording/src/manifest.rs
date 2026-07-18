//! Session manifest: on-disk source of truth for a recording's structure.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AecMode {
    Disabled,
    Always,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

    /// True when this session was recovered from an interruption (force-quit
    /// / crash) by `finalize_orphan`, rather than completing a clean Stop.
    /// Drives an "Interrupted — recovered" badge.
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
    pub const SCHEMA: u32 = 2;

    pub fn schema_is_supported(&self) -> bool {
        self.schema_version == Self::SCHEMA
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            Attendee { display_name: "Danny".into(), role: AttendeeRole::Self_ },
            Attendee { display_name: "Jassie".into(), role: AttendeeRole::Other },
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
    fn old_v1_json_still_deserializes_with_defaults() {
        // v1 manifests lack the new keys; serde(default) fills them.
        let v1 = r#"{"schema_version":1,"session_id":"s","created_at_unix_seconds":0,"sample_rate":16000,"channels":1,"mic_source_id":1,"mic_source_node_name":"m","mic_source_description":"m","system_source_id":2,"system_source_node_name":"s","system_source_description":"s","aec_mode":"always","chunks":[],"finalized_at_unix_seconds":null,"title":"old"}"#;
        let m: SessionManifest = serde_json::from_str(v1).unwrap();
        assert_eq!(m.title.as_deref(), Some("old"));
        assert!(m.tag_ids.is_empty());
        assert!(!m.meeting_id.is_empty()); // default_meeting_id() ran
    }
}
