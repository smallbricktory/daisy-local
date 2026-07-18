//! Tauri commands for the recording lifecycle.
//!
//! Pure-Rust impls; the binary's `main.rs` wraps each as a
//! `#[tauri::command]`.

use crate::error::{AppError, Result};
use crate::state::AppState;
use audio_engine::source::{list_sources, SourceKind};
use audio_engine::virtual_sink::VirtualSink;
use recording::live_pipeline::{LivePipelineEvent, LivePipelineEventKind};
use recording::live_transcript::LiveTrack;
use recording::{LiveMode, Recorder, RecorderConfig, State};
use serde::{Deserialize, Serialize};
use std::sync::Mutex;

/// Holds the VirtualSink alongside the recorder; both are released on stop.
pub struct ActiveRecording {
    pub recorder: Recorder,
    pub _virtual_sink: Option<VirtualSink>,
    pub started_at_unix_seconds: i64,
    pub live_mode_label: String,
    /// Set when a Bluetooth card was forced onto a mic-capable profile at
    /// start; the prior profile is restored on stop.
    pub bt_profile_flip: Option<audio_engine::bt_profile::BtProfileFlip>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct StartRequest {
    pub mic_source_id: u32,
    /// If None, request virtual sink mode (Daisy creates and owns the sink).
    pub system_source_id: Option<u32>,
    pub session_id: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub tag_ids: Vec<String>,
    #[serde(default)]
    pub notes_md: Option<String>,
    #[serde(default)]
    pub meeting_id: Option<String>,
    /// Optional link to a calendar event: the source event's subscription_id
    /// (as `provider`), uid (as `event_id`), and planned time bounds. Written
    /// verbatim to the manifest's `calendar` field.
    #[serde(default)]
    pub calendar_link: Option<recording::manifest::CalendarLink>,
    /// Attendees pre-filled from the source calendar event. Each carries a
    /// display name + role; written to the manifest.
    #[serde(default)]
    pub attendees: Vec<recording::manifest::Attendee>,
    /// False = there is a group on the local end; diarize the mic track too.
    /// Absent / None = use the manifest default (true = solo local end).
    #[serde(default)]
    pub single_local_speaker: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RecordingSnapshot {
    pub state: &'static str,
    pub session_id: String,
    pub session_root: String,
    pub started_at_unix_seconds: i64,
    /// Live-captions state at session start: "off", or the whisper model in
    /// use (e.g. "ggml-base.en.bin").
    pub live_mode_label: String,
}

fn state_label(s: State) -> &'static str {
    match s {
        State::Idle => "idle",
        State::Recording => "recording",
        State::Paused => "paused",
        State::Stopped => "stopped",
    }
}

use crate::now_unix;

/// Construct the `LiveMode` for this machine.
///
/// Live captions resolve per machine (env override → manual choice →
/// benchmark verdict → hardware detection; see
/// `hardware::resolve_live_captions`). Finalize always transcribes +
/// diarizes locally either way.
fn build_live_mode(
    settings: &crate::settings::Settings,
    _session_root: &std::path::Path,
    models_dir: &std::path::Path,
    // Meeting proper-noun terms (title/attendees/tags) to prime live
    // recognition via the Whisper vocab sentence. Empty = no priming.
    terms: &[String],
) -> LiveMode {
    let res = crate::hardware::resolve_live_captions(settings);
    if !res.enabled {
        log::info!("live captions: off (source: {})", res.source);
        return LiveMode::Off;
    }
    if let Some(model) = resolve_live_whisper_model(settings, models_dir) {
        // Shared decode budget: one decode in flight across both tracks
        // (see LocalWhisperRealtime::decode_gate), capped at 6 threads.
        let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
        let budget = (cores / 2).clamp(1, 6) as i32;
        let label = if cfg!(target_os = "macos") { "whisper (Metal)" } else { "whisper" };
        log::info!("live captions: local {label}, {budget} thread(s) (shared, one decode in flight)");
        LiveMode::Realtime {
            client: std::sync::Arc::new(
                providers_local::streaming::transcriber::LocalWhisperRealtime::new(
                    model, budget, label,
                )
                .with_hop_ladder(settings.live_hop_ladder_ms.clone())
                .with_initial_prompt(
                    crate::commands::transcribe_priming::vocab_sentence(terms),
                ),
            ),
        }
    } else {
        log::warn!("live captions: whisper model missing — off");
        LiveMode::Off
    }
}

/// Resolve the whisper GGML model for live captions: settings side-load
/// override → `DAISY_WHISPER_MODEL_DIR` → profile models dir. Returns `None`
/// if no model file exists.
fn resolve_live_whisper_model(
    settings: &crate::settings::Settings,
    models_dir: &std::path::Path,
) -> Option<std::path::PathBuf> {
    if let Some(p) = settings.whisper_model_path.as_ref() {
        let p = std::path::PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
    }
    if let Ok(dir) = std::env::var("DAISY_WHISPER_MODEL_DIR") {
        let p = std::path::Path::new(&dir).join("ggml-base.en.bin");
        if p.is_file() {
            return Some(p);
        }
    }
    let p = models_dir.join("ggml-base.en.bin");
    if p.is_file() {
        return Some(p);
    }
    None
}

fn describe_live_mode(mode: &LiveMode) -> String {
    match mode {
        LiveMode::Off => "off".to_string(),
        // On = the whisper model in use.
        LiveMode::Realtime { client } => client.model().to_string(),
    }
}

/// Serialisable payload for a `LivePipelineEvent`, emitted as a Tauri event.
#[derive(Debug, Clone, Serialize)]
pub struct LiveEventPayload {
    pub track: &'static str,
    pub kind: serde_json::Value,
}

impl LiveEventPayload {
    pub fn from_event(event: &LivePipelineEvent) -> Self {
        let track = match event.track {
            LiveTrack::Mic => "mic",
            LiveTrack::System => "system",
        };
        let kind = match &event.kind {
            LivePipelineEventKind::Interim {
                text,
                start_ms,
                end_ms,
                confidence,
            } => serde_json::json!({
                "type": "interim",
                "start_ms": start_ms,
                "end_ms": end_ms,
                "text": text,
                "confidence": confidence,
            }),
            LivePipelineEventKind::Final {
                text,
                start_ms,
                end_ms,
                confidence,
            } => serde_json::json!({
                "type": "final",
                "start_ms": start_ms,
                "end_ms": end_ms,
                "text": text,
                "confidence": confidence,
            }),
            LivePipelineEventKind::Error(msg) => serde_json::json!({
                "type": "error",
                "message": msg,
            }),
            LivePipelineEventKind::MicSilent { elapsed_ms } => serde_json::json!({
                "type": "mic_silent",
                "elapsed_ms": elapsed_ms,
            }),
            // Forwarded as its own `recording:mic-level` event (see main.rs),
            // not a transcript segment.
            LivePipelineEventKind::MicLevel { peak } => serde_json::json!({
                "type": "mic_level",
                "peak": peak,
            }),
        };
        Self { track, kind }
    }
}

pub fn start_recording_impl(
    app: &AppState,
    active: &Mutex<Option<ActiveRecording>>,
    req: StartRequest,
) -> Result<(RecordingSnapshot, Option<tokio::sync::mpsc::Receiver<LivePipelineEvent>>)> {
    let mut slot = active.lock().unwrap();
    if slot.is_some() {
        return Err(AppError::AlreadyRecording);
    }
    let virtual_sink = if req.system_source_id.is_none() {
        Some(
            VirtualSink::create("daisy-capture")
                .map_err(|e| AppError::Recording(format!("virtual sink: {e}")))?,
        )
    } else {
        None
    };
    let mut sources =
        list_sources().map_err(|e| AppError::Recording(format!("list sources: {e}")))?;
    let mut mic = sources
        .iter()
        .find(|s| s.id == req.mic_source_id)
        .ok_or_else(|| {
            AppError::Recording(format!("mic source {} not found", req.mic_source_id))
        })?
        .clone();

    // Force a Bluetooth card onto a mic-capable profile before opening
    // capture. A profile change renumbers the input node; after a flip,
    // sources are re-listed and the mic re-resolved by its BlueZ address.
    let bt_profile_flip = match audio_engine::bt_profile::ensure_capture_profile(&mic.node_name) {
        Ok(Some(flip)) => {
            let addr = audio_engine::bt_profile::bluez_address(&mic.node_name);
            // BlueZ profile transition latency before the input node appears.
            std::thread::sleep(std::time::Duration::from_millis(250));
            sources = list_sources()
                .map_err(|e| AppError::Recording(format!("list sources (post BT flip): {e}")))?;
            mic = sources
                .iter()
                .find(|s| {
                    s.kind == SourceKind::Mic
                        && audio_engine::bt_profile::bluez_address(&s.node_name) == addr
                })
                .ok_or_else(|| {
                    AppError::Recording(
                        "Bluetooth mic did not expose an input after switching to its \
                         headset profile — reconnect the headset and try again"
                            .to_string(),
                    )
                })?
                .clone();
            Some(flip)
        }
        Ok(None) => None,
        Err(e) => return Err(AppError::Recording(e.to_string())),
    };

    let system_id = match req.system_source_id {
        Some(id) => id,
        None => {
            // The virtual-sink monitor name differs by platform:
            //   Linux: "daisy-capture.monitor"
            //   Windows: "wasapi-loopback"
            let monitor_name = virtual_sink
                .as_ref()
                .expect("virtual_sink is Some when system_source_id is None")
                .monitor_source_name();
            sources
                .iter()
                .find(|s| s.kind == SourceKind::Monitor && s.node_name == monitor_name)
                .ok_or_else(|| {
                    AppError::Recording(format!("{monitor_name} not found"))
                })?
                .id
        }
    };
    let sys = sources
        .iter()
        .find(|s| s.id == system_id)
        .ok_or_else(|| {
            AppError::Recording(format!("system source {} not found", system_id))
        })?
        .clone();

    let session_id = req
        .session_id
        .clone()
        .unwrap_or_else(|| format!("daisy-{}", now_unix()));
    let session_root = app.profile.session_path(&session_id);

    let settings =
        crate::settings::Settings::load_or_default(&app.profile.settings_path());
    // Resolve the meeting's ASR priming terms (attendee names + tag
    // vocabulary) before building the live backend. Tags come from the start
    // request.
    let live_terms = {
        let vocab_terms: Vec<String> = if req.tag_ids.is_empty() {
            Vec::new()
        } else {
            let tags = crate::commands::tags::load_tags_file(app)
                .map(|f| f.tags)
                .unwrap_or_default();
            crate::commands::transcribe_priming::collect_tag_vocab_terms(&tags, &req.tag_ids)
        };
        let attendee_names: Vec<String> =
            req.attendees.iter().map(|a| a.display_name.clone()).collect();
        // ASR primes on attendee names + explicit tag vocabulary only; not
        // the title or tag names.
        crate::commands::transcribe_priming::meeting_terms(None, &attendee_names, &vocab_terms)
    };
    let live_mode = build_live_mode(
        &settings,
        &session_root,
        &app.profile.models_dir(),
        &live_terms,
    );
    // Compute the label before the LiveMode is moved into RecorderConfig.
    let live_mode_label = describe_live_mode(&live_mode);

    let speech_env_min = recording::speech_levels::SpeechLevels::load(app.profile.root())
        .live_speech_env_min(&mic.description);
    if let Some(v) = speech_env_min {
        log::info!(
            "live AGC guard seeded from speech-level store: {:.4} ({:.1} dBFS) for '{}'",
            v,
            20.0 * v.log10(),
            mic.description
        );
    }

    let cfg = RecorderConfig {
        session_root: session_root.clone(),
        mic_source_id: mic.id,
        mic_source_node_name: mic.node_name.clone(),
        mic_source_description: mic.description.clone(),
        system_source_id: sys.id,
        system_source_node_name: sys.node_name.clone(),
        system_source_description: sys.description.clone(),
        sample_rate: 16_000,
        session_id: session_id.clone(),
        live_mode,
        speech_env_min,
        // Rides the debug-logging setting; no toggle of its own.
        flight_recorder: crate::settings::Settings::load_or_default(&app.profile.settings_path())
            .debug_level
            .verbose(),
    };
    let started = now_unix();
    let mut recorder = Recorder::start(cfg)?;
    let live_events_rx = recorder.take_live_events_rx();
    log::info!("recording: started session {session_id} (live={live_mode_label}) @ {started}");
    crate::perf::set_recording_active(true);
    providers_local::streaming::live_metrics::reset();

    // Overlay request metadata (title/tags/notes/meeting_id) onto the manifest
    // the recorder wrote; only fields the request supplied are overridden.
    apply_start_metadata(&session_root, &req)?;
    // Every attendee becomes a Contact (identity only). Contact write
    // failures do not block recording.
    for a in &req.attendees {
        let _ = crate::commands::contacts::upsert_contact_in_store(app, &a.display_name, None);
    }

    let snap = RecordingSnapshot {
        state: state_label(recorder.state()),
        session_id: session_id.clone(),
        session_root: session_root.to_string_lossy().into_owned(),
        started_at_unix_seconds: started,
        live_mode_label,
    };
    *slot = Some(ActiveRecording {
        recorder,
        _virtual_sink: virtual_sink,
        started_at_unix_seconds: started,
        live_mode_label: snap.live_mode_label.clone(),
        bt_profile_flip,
    });
    Ok((snap, live_events_rx))
}

pub fn pause_impl(app: &AppState, active: &Mutex<Option<ActiveRecording>>) -> Result<&'static str> {
    let mut slot = active.lock().unwrap();
    let ar = slot.as_mut().ok_or(AppError::NotRecording)?;
    ar.recorder.pause()?;
    let label = state_label(ar.recorder.state());
    let session_id = session_id_of(ar);
    log::info!("recording: paused session {session_id:?}");
    drop(slot);
    if let Some(sid) = session_id {
        stop_close_segment(&app.profile.session_path(&sid), now_unix())?;
    }
    Ok(label)
}

/// Mute/unmute the local mic during recording (gain → 0).
pub fn set_mic_muted_impl(active: &Mutex<Option<ActiveRecording>>, muted: bool) -> Result<()> {
    let slot = active.lock().unwrap();
    let ar = slot.as_ref().ok_or(AppError::NotRecording)?;
    if !ar.recorder.set_mic_muted(muted) {
        return Err(AppError::Recording("couldn't change mic gain".into()));
    }
    Ok(())
}

/// Switch the recording mic mid-session to `source_id`.
pub fn switch_mic_impl(
    app: &AppState,
    active: &Mutex<Option<ActiveRecording>>,
    source_id: u32,
) -> Result<()> {
    let source = list_sources()
        .map_err(|e| AppError::Recording(format!("list sources: {e}")))?
        .into_iter()
        .find(|s| s.id == source_id)
        .ok_or_else(|| AppError::Recording(format!("mic source {source_id} not found")))?;
    // The new device's learned AGC guard floor rides along with the switch.
    let speech_env_min = recording::speech_levels::SpeechLevels::load(app.profile.root())
        .live_speech_env_min(&source.description);
    let mut slot = active.lock().unwrap();
    let ar = slot.as_mut().ok_or(AppError::NotRecording)?;
    ar.recorder.switch_mic(source, speech_env_min)?;
    Ok(())
}

pub fn resume_impl(
    app: &AppState,
    active: &Mutex<Option<ActiveRecording>>,
) -> Result<&'static str> {
    let mut slot = active.lock().unwrap();
    let ar = slot.as_mut().ok_or(AppError::NotRecording)?;
    ar.recorder.resume()?;
    let label = state_label(ar.recorder.state());
    let session_id = session_id_of(ar);
    log::info!("recording: resumed session {session_id:?}");
    drop(slot);
    if let Some(sid) = session_id {
        resume_open_segment(&app.profile.session_path(&sid), now_unix())?;
    }
    Ok(label)
}

/// Extract the session id (= session-root dir name) for an active recording.
fn session_id_of(ar: &ActiveRecording) -> Option<String> {
    ar.recorder
        .session_root()
        .file_name()
        .and_then(|n| n.to_str())
        .map(|s| s.to_string())
}

pub fn stop_impl(active: &Mutex<Option<ActiveRecording>>) -> Result<String> {
    let mut slot = active.lock().unwrap();
    let mut ar = slot.take().ok_or(AppError::NotRecording)?;
    crate::perf::set_recording_active(false);
    let elapsed = now_unix().saturating_sub(ar.started_at_unix_seconds);
    let final_root = ar.recorder.stop()?;
    log::info!(
        "recording: stopped after {elapsed}s (live={}) -> {}",
        ar.live_mode_label,
        final_root.display()
    );
    // Restore the Bluetooth profile that was flipped at start.
    if let Some(flip) = ar.bt_profile_flip.take() {
        audio_engine::bt_profile::restore_profile(&flip);
    }
    Ok(final_root.to_string_lossy().into_owned())
}

pub fn current_impl(active: &Mutex<Option<ActiveRecording>>) -> Option<&'static str> {
    let slot = active.lock().unwrap();
    slot.as_ref().map(|ar| state_label(ar.recorder.state()))
}

pub fn recording_snapshot_impl(active: &Mutex<Option<ActiveRecording>>) -> Option<RecordingSnapshot> {
    let slot = active.lock().unwrap();
    slot.as_ref().map(|ar| RecordingSnapshot {
        state: state_label(ar.recorder.state()),
        session_id: ar
            .recorder
            .session_root()
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string(),
        session_root: ar.recorder.session_root().to_string_lossy().into_owned(),
        started_at_unix_seconds: ar.started_at_unix_seconds,
        live_mode_label: ar.live_mode_label.clone(),
    })
}

// ── Manifest-mutation helpers (pure, unit-testable) ────────────────────────

fn read_manifest_at(session_dir: &std::path::Path) -> Result<recording::manifest::SessionManifest> {
    let mp = session_dir.join("manifest.json");
    let bytes = syncsafe::read(&mp).map_err(|e| AppError::Config(format!("read manifest: {e}")))?;
    serde_json::from_slice(&bytes).map_err(|e| AppError::Config(format!("parse manifest: {e}")))
}

fn write_manifest_at(
    session_dir: &std::path::Path,
    m: &recording::manifest::SessionManifest,
) -> Result<()> {
    let mp = session_dir.join("manifest.json");
    let tmp = mp.with_extension("json.tmp");
    let bytes = serde_json::to_vec_pretty(m)
        .map_err(|e| AppError::Config(format!("encode manifest: {e}")))?;
    syncsafe::write(&tmp, &bytes).map_err(|e| AppError::Config(format!("write manifest: {e}")))?;
    syncsafe::rename(&tmp, &mp).map_err(|e| AppError::Config(format!("rename manifest: {e}")))?;
    Ok(())
}

#[cfg(test)]
mod live_mode_tests {
    use super::*;

    #[test]
    fn build_live_mode_is_off_without_a_local_whisper_model() {
        // Captions on via this machine's manual choice + no model available:
        // exercises the missing-model fallback on any machine.
        std::env::remove_var("DAISY_WHISPER_MODEL_DIR");
        let mut s = crate::settings::Settings::defaults();
        s.live_captions_by_machine.insert(
            crate::hardware::machine_name(),
            crate::settings::MachineLiveCaptions {
                choice: crate::settings::LiveCaptionsChoice::On,
                bench_xrt: None,
                benched_at_unix_seconds: None,
            },
        );
        let empty = tempfile::tempdir().unwrap();
        let m = build_live_mode(&s, empty.path(), empty.path(), &[]);
        assert!(matches!(m, LiveMode::Off));
    }
}

#[cfg(test)]
mod start_request_tests {
    use super::*;

    #[test]
    fn start_request_deserializes_single_local_speaker() {
        let json = r#"{"mic_source_id":1,"system_source_id":null,"session_id":null,"single_local_speaker":false}"#;
        let req: StartRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.single_local_speaker, Some(false));
    }

    #[test]
    fn start_request_defaults_single_local_speaker_to_none() {
        // Omitted → None → diarize uses the manifest default (solo local end).
        let json = r#"{"mic_source_id":1,"system_source_id":null,"session_id":null}"#;
        let req: StartRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.single_local_speaker, None);
    }
}

/// Close the open recording segment in a session's manifest and persist.
pub fn stop_close_segment(session_dir: &std::path::Path, now_unix: i64) -> Result<()> {
    let mut m = read_manifest_at(session_dir)?;
    let last_chunk = m.chunks.last().map(|c| c.index).unwrap_or(0);
    recording::manifest_ops::close_active_segment(&mut m, now_unix, last_chunk);
    write_manifest_at(session_dir, &m)
}

/// Open a new recording segment in a session's manifest and persist.
pub fn resume_open_segment(session_dir: &std::path::Path, now_unix: i64) -> Result<()> {
    let mut m = read_manifest_at(session_dir)?;
    recording::manifest_ops::open_segment(&mut m, now_unix);
    write_manifest_at(session_dir, &m)
}

/// After `Recorder::start` has written a fresh v2 manifest, overlay the
/// metadata supplied in the start request (title / tag_ids / notes / meeting_id).
fn apply_start_metadata(session_dir: &std::path::Path, req: &StartRequest) -> Result<()> {
    let mut m = read_manifest_at(session_dir)?;
    m.title = req.title.clone();
    m.tag_ids = req.tag_ids.clone();
    if let Some(mid) = req.meeting_id.clone() {
        m.meeting_id = mid;
    }
    if let Some(text) = req.notes_md.as_ref().filter(|t| !t.is_empty()) {
        let notes_path = session_dir.join("notes.md");
        syncsafe::write(&notes_path, text.as_bytes())
            .map_err(|e| AppError::Config(format!("write notes.md: {e}")))?;
        m.notes_md_relative = Some(std::path::PathBuf::from("notes.md"));
    }
    if let Some(link) = req.calendar_link.clone() {
        m.calendar = Some(link);
    }
    if !req.attendees.is_empty() {
        m.attendees = req.attendees.clone();
    }
    if let Some(v) = req.single_local_speaker {
        m.single_local_speaker = v;
    }
    write_manifest_at(session_dir, &m)
}
