// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
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

/// Set while a `start_recording_impl` build is in flight (the recording slot
/// is still empty during it).
static STARTING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// True while a recording start is mid-build. The app-close flush waits on
/// this so an in-flight start is actually flushed instead of dying with the
/// process (which would also leave a flipped BT profile unrestored).
pub fn is_starting() -> bool {
    STARTING.load(std::sync::atomic::Ordering::SeqCst)
}

/// The recorder owns the capture sink and its routing; this holds only what
/// the command layer itself restores on stop.
pub struct ActiveRecording {
    pub recorder: Recorder,
    pub started_at_unix_seconds: i64,
    pub live_mode_label: String,
    /// Set when a Bluetooth card was forced onto a mic-capable profile at
    /// start; the prior profile is restored on stop.
    pub bt_profile_flip: Option<audio_engine::bt_profile::BtProfileFlip>,
}

/// Tauri-managed slot holding the one active recording.
pub struct ActiveRecordingHandle(pub Mutex<Option<ActiveRecording>>);

/// Tauri-managed clock-cycle preference for the default recording title:
/// true renders `%-I:%M %p` (12-hour), false `%-H:%M` (24-hour). The
/// frontend sets it once at startup from the OS locale.
pub struct ClockStyle(pub std::sync::atomic::AtomicBool);

/// Tauri-managed keep-alive for the app-lifetime device watcher feeding the
/// UI: the process-wide topology hub plus the UI subscription.
pub struct TopologyHubHandle(
    pub std::sync::Arc<audio_engine::topology::TopologyHub>,
    #[allow(dead_code)] pub audio_engine::topology::HubSubscription,
);

/// Stop channels of every running preview meter, by request id.
pub fn mic_meter_registry(
) -> &'static Mutex<std::collections::HashMap<String, std::sync::mpsc::Sender<()>>> {
    static R: std::sync::OnceLock<
        Mutex<std::collections::HashMap<String, std::sync::mpsc::Sender<()>>>,
    > = std::sync::OnceLock::new();
    R.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}

/// How long to wait for the preview meters to let go of their device before
/// opening capture anyway.
pub const MIC_METER_RELEASE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Signals every running preview meter to stop and waits until each has
/// released its device. Each meter thread drops its own registry entry as it
/// exits, so an empty registry means every device is free.
pub fn stop_all_mic_meters() {
    let running = {
        let reg = mic_meter_registry().lock().unwrap();
        for stop in reg.values() {
            let _ = stop.send(());
        }
        reg.len()
    };
    if running == 0 {
        return;
    }
    log::info!("mic meter: stopping {running} preview meter(s) before capture opens");
    let deadline = std::time::Instant::now() + MIC_METER_RELEASE_TIMEOUT;
    while std::time::Instant::now() < deadline {
        if mic_meter_registry().lock().unwrap().is_empty() {
            log::info!("mic meter: preview meters released");
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let stuck: Vec<String> = mic_meter_registry().lock().unwrap().keys().cloned().collect();
    log::warn!("mic meter: {stuck:?} still holding a device after {MIC_METER_RELEASE_TIMEOUT:?} — opening capture anyway");
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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
/// The live decode is the session transcript (single-pass): it runs whenever
/// a model is present. The captions *display* preference
/// (`show_live_captions`) only controls the frontend view.
fn build_live_mode(
    machine: &crate::settings::MachineSettings,
    models_dir: &std::path::Path,
    // Meeting proper-noun terms (title/attendees/tags) to prime live
    // recognition via the Whisper vocab sentence. Empty = no priming.
    terms: &[String],
) -> LiveMode {
    use providers_realtime::RealtimeTranscriber;
    // Faint-audio marker detector. Absent model = no markers.
    let vad_model = machine
        .asr
        .faint_audio_marker
        .then(|| vad_model_path(models_dir))
        .flatten();

    // One resolution for the primary engine; the other engine, when also
    // installed, rides along as the supervision fallback.
    use providers_local::registry::Engine;
    let chain = crate::commands::asr_models::resolve_asr_chain(machine, models_dir);
    let resolved = chain.first().map(|(row, _)| *row);
    let parakeet_row = chain.iter().find(|(r, _)| r.engine == Engine::Parakeet);
    let whisper_model = chain
        .iter()
        .find(|(r, _)| r.engine == Engine::Whisper)
        .map(|(_, p)| p.clone());
    let parakeet: Option<std::sync::Arc<dyn RealtimeTranscriber>> =
        parakeet_row.map(|(row, dir)| {
            std::sync::Arc::new(
                providers_local::parakeet::realtime::ParakeetRealtime::new(
                    dir.clone(),
                    machine.asr.parakeet,
                    row.display,
                )
                .with_marker(vad_model.clone()),
            ) as std::sync::Arc<dyn RealtimeTranscriber>
        });
    let whisper: Option<std::sync::Arc<dyn RealtimeTranscriber>> =
        whisper_model.map(|model| {
            // Shared decode budget: one decode in flight across both tracks
            // (see LocalWhisperRealtime::decode_gate).
            let tune = machine.asr.whisper.clone().clamped();
            let label = format!("whisper ({})", providers_local::whisper_backend());
            std::sync::Arc::new(
                providers_local::streaming::transcriber::LocalWhisperRealtime::new(
                    model,
                    tune.live_threads(),
                    label,
                )
                .with_window_cap_s(tune.window_cap_s)
                .with_language(tune.language)
                .with_hop_ladder(tune.hop_ladder_ms.clone())
                .with_initial_prompt(crate::commands::transcribe_priming::vocab_sentence(terms))
                .with_marker(vad_model.clone()),
            ) as std::sync::Arc<dyn RealtimeTranscriber>
        });

    // SpeechAnalyzer resolves only where sa_available() said yes; it needs no
    // model path and no marker (no VAD reports on macOS).
    let sa: Option<std::sync::Arc<dyn RealtimeTranscriber>> =
        matches!(resolved, Some(r) if r.engine == Engine::SpeechAnalyzer).then(|| {
            std::sync::Arc::new(providers_local::macos::realtime::SaRealtime::new(""))
                as std::sync::Arc<dyn RealtimeTranscriber>
        });

    let prefer_parakeet = matches!(resolved, Some(r) if r.engine == Engine::Parakeet);
    let (primary, fallback) = if sa.is_some() {
        (sa, whisper.or(parakeet))
    } else if prefer_parakeet {
        (parakeet, whisper)
    } else {
        (whisper, parakeet)
    };
    match (primary, fallback) {
        (Some(client), fallback) => {
            log::info!(
                "live transcription: {} (fallback: {})",
                client.model(),
                fallback.as_ref().map(|f| f.model().to_string()).unwrap_or_else(|| "none".into())
            );
            LiveMode::Realtime { client, fallbacks: fallback.into_iter().collect() }
        }
        (None, Some(client)) => {
            log::warn!(
                "live transcription: preferred engine unavailable — using {}",
                client.model()
            );
            LiveMode::Realtime { client, fallbacks: Vec::new() }
        }
        (None, None) => {
            log::warn!("live transcription: no engine available — record-only");
            LiveMode::Off
        }
    }
}

/// Silero VAD model location: the profile copy first, else the bundled
/// resource dir (same precedence as the ASR model lookup).
/// A faint-audio marker model resolves on this machine. False where none is
/// bundled (macOS: SpeechAnalyzer never reports and no VAD model ships), so
/// the marker toggle has nothing to switch.
pub fn faint_marker_available_impl(app: &AppState) -> bool {
    vad_model_path(&app.profile.models_dir()).is_some()
}

fn vad_model_path(models_dir: &std::path::Path) -> Option<std::path::PathBuf> {
    let p = models_dir.join("vad").join("silero_vad_v5.onnx");
    if p.is_file() {
        return Some(p);
    }
    std::env::var_os("DAISY_VAD_MODEL_DIR")
        .map(|d| std::path::PathBuf::from(d).join("silero_vad_v5.onnx"))
        .filter(|p| p.is_file())
}

fn describe_live_mode(mode: &LiveMode) -> String {
    match mode {
        LiveMode::Off => "off".to_string(),
        // On = the whisper model in use.
        LiveMode::Realtime { client, .. } => client.model().to_string(),
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
            LivePipelineEventKind::PossibleMissedSpeech { start_ms, end_ms } => serde_json::json!({
                "type": "possible_missed_speech",
                "start_ms": start_ms,
                "end_ms": end_ms,
            }),
            LivePipelineEventKind::Error(msg) => serde_json::json!({
                "type": "error",
                "message": msg,
            }),
            // Forwarded as an updated `recording:snapshot` (see main.rs),
            // not a transcript segment.
            LivePipelineEventKind::EngineSwitched { model } => serde_json::json!({
                "type": "engine_switched",
                "model": model,
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

/// True for start failures whose error indicates a stale device handle —
/// the class an audio-daemon restart leaves behind. macOS AUHAL rejects a
/// dead AudioDeviceID with rc=-10851 (kAudioUnitErr_InvalidPropertyValue);
/// WASAPI raises AUDCLNT_E_DEVICE_INVALIDATED (0x88890004).
pub fn is_stale_device_err(msg: &str) -> bool {
    msg.contains("-10851")
        || msg.contains("0x88890004")
        || msg.contains("AUDCLNT_E_DEVICE_INVALIDATED")
}

/// Opens capture and publishes the recording: the state and snapshot
/// events, the library row, the 1 s rotation tick and the live-event
/// forwarder. Both the `start_recording` command and the hotkey's
/// quick start go through here.
pub fn start_recording_from(
    app: &tauri::AppHandle,
    req: StartRequest,
) -> Result<RecordingSnapshot> {
    use tauri::{Emitter, Manager};
    if !crate::commands::license::features_enabled() {
        return Err(AppError::LicenseExpired);
    }
    let state = app.state::<crate::state::AppState>();
    let active = app.state::<ActiveRecordingHandle>();
    let app_handle = app.clone();
    // Capture opens the microphone itself; a preview meter still holding it
    // would run a second unit on the same device for no reason (and two
    // VoiceProcessingIO units on one pair take up to a minute to start and
    // then deliver no frames).
    stop_all_mic_meters();
    let hub = std::sync::Arc::clone(&app_handle.state::<TopologyHubHandle>().0);
    let hour12 = app_handle.state::<ClockStyle>().0.load(std::sync::atomic::Ordering::Relaxed);
    let (snapshot, live_events_rx) = match start_recording_impl(
        &state,
        &active.0,
        req.clone(),
        std::sync::Arc::clone(&hub),
        hour12,
    ) {
        Err(e) if is_stale_device_err(&e.to_string()) => {
            // An audio-daemon restart invalidates device handles between
            // enumeration and open. One retry re-enumerates and re-resolves.
            log::warn!("start_recording: stale device handle ({e}); re-enumerating and retrying once");
            crate::commands::settings::invalidate_mic_view_cache();
            let _ = app_handle.emit("audio-sources-changed", ());
            start_recording_impl(&state, &active.0, req, hub, hour12)?
        }
        other => other?,
    };

    let _ = app_handle.emit("recording:state", "recording");
    let _ = app_handle.emit("recording:snapshot", Some(&snapshot));
    crate::library_events::emit(
        &app_handle,
        crate::library_events::LibraryChangeKind::Added,
        &snapshot.session_id,
    );

    // Recording tick: rotates the chunk and drains the supervisor's notices
    // — device-list pushes, sink plumbing reactions, and warnings. Runs on
    // the async runtime for the recording's lifetime.
    {
        let app_for_notices = app_handle.clone();
        tauri::async_runtime::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                let handle = app_for_notices.state::<ActiveRecordingHandle>();
                // Short-lock: rotate + drain, then release before acting.
                let notices = {
                    let mut guard = handle.0.lock().unwrap();
                    let Some(ar) = guard.as_mut() else { break };
                    if let Err(e) = ar.recorder.maybe_rotate_chunk() {
                        log::warn!("chunk rotation failed: {e}");
                    }
                    ar.recorder.poll_notices()
                };
                for n in notices {
                    use recording::supervisor::Notice;
                    match n {
                        Notice::CodecDownshifted { mac_stem } => {
                            let state = app_for_notices.state::<crate::state::AppState>();
                            crate::bt_prefs::remember_cvsd(state.profile.root(), &mac_stem);
                            log::info!("bt prefs: {mac_stem} pinned to CVSD on this machine");
                        }
                        n @ (Notice::MicFailedOver { .. }
                        | Notice::MicMutedExternally
                        | Notice::MicVeryQuiet
                        | Notice::MicRetrying
                        | Notice::MicDeadAir
                        | Notice::MicHeldByCall
                        | Notice::MicRecovered
                        | Notice::SystemAudioLost
                        | Notice::SystemAudioRecovered
                        | Notice::ReconnectFailed { .. }) => {
                            // The log line is the field-forensics record of
                            // what the user was shown (toasts leave no other
                            // trace).
                            log::info!("audio notice -> frontend: {n:?}");
                            let _ = app_for_notices.emit("audio-warning", &n);
                        }
                    }
                }
            }
            log::info!("recording tick: exited");
        });
    }

    // Forwarder task: drains the live events receiver and emits Tauri events
    // to the frontend. Lives until the channel closes (Recorder::stop drops
    // the LivePipeline, which drops events_tx).
    if let Some(mut rx) = live_events_rx {
        tauri::async_runtime::spawn(async move {
            log::info!("live event forwarder: started");
            while let Some(event) = rx.recv().await {
                // MicSilent is emitted as its own event, not a transcript line.
                if let recording::live_pipeline::LivePipelineEventKind::MicSilent {
                    elapsed_ms,
                } = &event.kind
                {
                    log::warn!("live event forwarder: mic silent after {elapsed_ms}ms");
                    if let Err(e) = app_handle
                        .emit("recording:mic-silent", serde_json::json!({ "elapsed_ms": elapsed_ms }))
                    {
                        log::warn!("failed to emit recording:mic-silent: {e}");
                    }
                    continue;
                }
                // MicLevel drives the in-call mic meter; high-frequency,
                // emitted as its own event, never a transcript line.
                if let recording::live_pipeline::LivePipelineEventKind::MicLevel { peak } =
                    &event.kind
                {
                    let _ = app_handle
                        .emit("recording:mic-level", serde_json::json!({ "peak": peak }));
                    continue;
                }
                // A mid-session engine switch/demotion updates the snapshot's
                // live_mode_label so the status toast stays honest.
                if let recording::live_pipeline::LivePipelineEventKind::EngineSwitched { model } =
                    &event.kind
                {
                    let handle = app_handle.state::<ActiveRecordingHandle>();
                    if let Some(ar) = handle.0.lock().unwrap().as_mut() {
                        ar.live_mode_label = model.clone();
                    }
                    if let Some(snap) = recording_snapshot_impl(&handle.0) {
                        let _ = app_handle.emit("recording:snapshot", &snap);
                    }
                    continue;
                }
                let track = match event.track {
                    recording::live_transcript::LiveTrack::Mic => "mic",
                    recording::live_transcript::LiveTrack::System => "system",
                };
                let kind_str = match &event.kind {
                    recording::live_pipeline::LivePipelineEventKind::Interim { .. } => "interim",
                    recording::live_pipeline::LivePipelineEventKind::Final { .. } => "final",
                    recording::live_pipeline::LivePipelineEventKind::PossibleMissedSpeech { .. } => {
                        "possible_missed_speech"
                    }
                    recording::live_pipeline::LivePipelineEventKind::Error(_) => "error",
                    // Intercepted above; never reaches transcript:segment.
                    recording::live_pipeline::LivePipelineEventKind::EngineSwitched { .. } => {
                        "engine_switched"
                    }
                    recording::live_pipeline::LivePipelineEventKind::MicSilent { .. } => "mic_silent",
                    recording::live_pipeline::LivePipelineEventKind::MicLevel { .. } => "mic_level",
                };
                log::debug!("live event forwarder: emitting {} event for track={}", kind_str, track);
                let payload = LiveEventPayload::from_event(&event);
                if let Err(e) = app_handle.emit("transcript:segment", payload) {
                    log::warn!("failed to emit transcript:segment: {e}");
                }
            }
            log::info!("live event forwarder: exited");
        });
    }

    Ok(snapshot)
}

/// What the hotkey asks for: one mic, the virtual sink for system audio, a
/// generated session id, and no meeting metadata to fill in.
pub fn quick_start_request(title: String, mic_source_id: u32) -> StartRequest {
    StartRequest {
        mic_source_id,
        system_source_id: None,
        session_id: None,
        title: Some(title),
        tag_ids: Vec::new(),
        notes_md: None,
        meeting_id: None,
        calendar_link: None,
        attendees: Vec::new(),
        single_local_speaker: Some(true),
    }
}

/// Starts a recording with no picker: the default mic, the current time as
/// the title, the main window brought forward and the frontend told
/// `hotkey:started` so it navigates to the live recording screen. Before the
/// terms, the recording-consent screen, or an unlocked vault, the press only
/// raises the window. A recording already running also only raises the
/// window. With no microphone at all, or an audio-enumeration failure, or a
/// start failure, the window comes forward and the frontend is told:
/// `hotkey:no-mic` for an empty device list, `hotkey:start-failed` (with the
/// error's friendly text) for everything else.
pub async fn quick_start_recording(app: tauri::AppHandle) -> Result<()> {
    use tauri::{Emitter, Manager};
    if !crate::bootstrap::first_run_complete(
        crate::bootstrap::Eula::is_accepted(),
        crate::bootstrap::Consent::is_accepted(),
        crate::bootstrap::Onboarding::is_complete(),
        app.state::<crate::state::VaultState>().is_unlocked(),
    ) {
        let _ = crate::mini_window::show_main(&app);
        return Ok(());
    }

    let already = {
        let active = app.state::<ActiveRecordingHandle>();
        recording_snapshot_impl(&active.0)
            .is_some_and(|s| s.state == "recording" || s.state == "paused")
    };
    if already {
        let _ = crate::mini_window::show_main(&app);
        return Ok(());
    }

    let default_key = app.state::<crate::state::AppState>().machine_settings().default_mic_key;
    let sources = tauri::async_runtime::spawn_blocking(|| {
        let sources = list_sources().map_err(|e| AppError::Recording(format!("list sources: {e}")))?;
        let system_default = audio_engine::topology::default_input_key().map(|k| k.0);
        Ok::<_, AppError>((sources, system_default))
    })
    .await
    .map_err(|e| AppError::Recording(format!("audio enum task failed: {e}")))?;
    let (sources, system_default) = match sources {
        Ok(v) => v,
        Err(e) => {
            log::warn!("record hotkey: audio enumeration failed: {e}");
            let _ = crate::mini_window::show_main(&app);
            let _ = app.emit("hotkey:start-failed", e.friendly());
            return Ok(());
        }
    };

    let Some(mic_source_id) = pick_mic(default_key.as_deref(), system_default.as_deref(), &sources)
    else {
        log::warn!("record hotkey: no microphone to start from");
        let _ = crate::mini_window::show_main(&app);
        let _ = app.emit("hotkey:no-mic", ());
        return Ok(());
    };

    let hour12 = app.state::<ClockStyle>().0.load(std::sync::atomic::Ordering::Relaxed);
    let title = default_recording_title(chrono::Local::now(), hour12);
    let req = quick_start_request(title, mic_source_id);
    let app_for_start = app.clone();
    let started = tauri::async_runtime::spawn_blocking(move || start_recording_from(&app_for_start, req))
        .await
        .map_err(|e| AppError::Recording(format!("start task failed: {e}")))?;
    match started {
        Ok(_) => {
            let _ = app.emit("hotkey:started", ());
            crate::mini_window::surface_recording(&app).await;
        }
        Err(e) => {
            let _ = crate::mini_window::show_main(&app);
            log::warn!("record hotkey: start failed: {e}");
            let _ = app.emit("hotkey:start-failed", e.friendly());
        }
    }
    Ok(())
}

pub fn start_recording_impl(
    app: &AppState,
    active: &Mutex<Option<ActiveRecording>>,
    req: StartRequest,
    topology_hub: std::sync::Arc<audio_engine::topology::TopologyHub>,
    hour12: bool,
) -> Result<(RecordingSnapshot, Option<tokio::sync::mpsc::Receiver<LivePipelineEvent>>)> {
    // Reserve the start without holding the recording mutex across the slow
    // device work below (sink create, BT flip, recorder start can take
    // seconds; snapshot/stop polls must not queue behind them). The atomic
    // alone guards start-vs-start; stop paths that fire DURING the build
    // window see an empty slot (see `is_starting` for the app-close case).
    struct StartingGuard;
    impl Drop for StartingGuard {
        fn drop(&mut self) {
            STARTING.store(false, std::sync::atomic::Ordering::SeqCst);
        }
    }
    if STARTING.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return Err(AppError::AlreadyRecording);
    }
    let _starting = StartingGuard;
    if active.lock().unwrap().is_some() {
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
        .cloned()
        .or_else(|| {
            // The requested id came from a UI list that can go stale (an
            // audio-daemon restart renumbers devices). Fall back to the
            // stored key ONLY when the stale id is the one the settings
            // view handed out for it — an explicit pick of a different,
            // now-vanished mic must not silently record from the default.
            let (key, view_id) = crate::commands::settings::cached_mic_view()?;
            if view_id != req.mic_source_id || app.machine_settings().default_mic_key.as_deref() != Some(&key) {
                return None;
            }
            let s = sources.iter().find(|s| s.key.0 == key).cloned()?;
            log::warn!(
                "recording: requested mic id {} is stale — resolved stored key '{key}' to id {} instead",
                req.mic_source_id,
                s.id
            );
            Some(s)
        })
        .ok_or_else(|| {
            AppError::Recording(format!("mic source {} not found", req.mic_source_id))
        })?;

    // Force a Bluetooth card onto a mic-capable profile before opening
    // capture; the mic is re-resolved because the flip renumbers its node.
    let prefer_cvsd = crate::bt_prefs::prefers_cvsd_for(app.profile.root(), &mic.node_name);
    let bt_profile_flip = match audio_engine::bt_profile::prepare_capture(&mic.node_name, prefer_cvsd) {
        Ok(Some((flip, resolved, post_flip_sources))) => {
            sources = post_flip_sources;
            mic = resolved;
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

    let machine = app.machine_settings();
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
    let live_mode = build_live_mode(&machine, &app.profile.models_dir(), &live_terms);
    // Compute the label before the LiveMode is moved into RecorderConfig.
    let live_mode_label = describe_live_mode(&live_mode);

    let cfg = RecorderConfig {
        live_terms: live_terms.clone(),
        session_root: session_root.clone(),
        mic_source_id: mic.id,
        mic_source_node_name: mic.node_name.clone(),
        mic_source_key: mic.key.clone(),
        mic_source_description: mic.description.clone(),
        system_source_id: sys.id,
        system_source_node_name: sys.node_name.clone(),
        system_source_description: sys.description.clone(),
        sample_rate: 16_000,
        session_id: session_id.clone(),
        live_mode,
        // Rides the debug-logging setting; no toggle of its own.
        flight_recorder: machine.debug_level.verbose(),
        aec_override: match machine.aec_mode_override {
            crate::settings::AecModeOverride::Auto => recording::AecOverride::Auto,
            crate::settings::AecModeOverride::Always => recording::AecOverride::Always,
            crate::settings::AecModeOverride::Never => recording::AecOverride::Never,
        },
        // Read at start and on every failover, for the capture domain the
        // session actually runs in.
        mic_levels_lookup: {
            let profile_root = app.profile.root().to_path_buf();
            Some(Box::new(move |device: &str, voice_processed: bool| {
                let levels = recording::speech_levels::SpeechLevels::load(&profile_root);
                (
                    levels.live_speech_env_min(device, voice_processed),
                    levels.dead_air_floor_i16(device, voice_processed),
                )
            }))
        },
        virtual_sink,
        topology_hub,
    };
    let started = now_unix();
    let session_dir_preexisting = session_root.exists();
    let mut recorder = match Recorder::start(cfg) {
        Ok(r) => r,
        Err(e) => {
            // A failed start must not leave a phantom session behind: it
            // would collide with a same-second retry and feed orphan
            // recovery an empty session.
            if !session_dir_preexisting {
                let _ = std::fs::remove_dir_all(&session_root);
            }
            return Err(e.into());
        }
    };
    let live_events_rx = recorder.take_live_events_rx();
    log::info!("recording: started session {session_id} (live={live_mode_label}) @ {started}");
    crate::perf::set_recording_active(true);
    providers_local::streaming::live_metrics::reset();

    // Overlay request metadata (title/tags/notes/meeting_id) onto the manifest
    // the recorder wrote; only fields the request supplied are overridden.
    apply_start_metadata(&session_root, &req, hour12)?;
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
    *active.lock().unwrap() = Some(ActiveRecording {
        recorder,
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
    let sources =
        list_sources().map_err(|e| AppError::Recording(format!("list sources: {e}")))?;
    let source = sources
        .into_iter()
        .find(|s| s.id == source_id)
        .ok_or_else(|| AppError::Recording(format!("mic source {source_id} not found")))?;
    // A BT target may sit on A2DP (no mic); flip and re-resolve first.
    let prefer_cvsd = crate::bt_prefs::prefers_cvsd_for(app.profile.root(), &source.node_name);
    let (flip, source) = match audio_engine::bt_profile::prepare_capture(&source.node_name, prefer_cvsd) {
        Ok(Some((flip, resolved, _))) => (Some(flip), resolved),
        Ok(None) => (None, source),
        Err(e) => return Err(AppError::Recording(e.to_string())),
    };
    let mut slot = active.lock().unwrap();
    let ar = slot.as_mut().ok_or(AppError::NotRecording)?;
    // The new device's learned AGC guard floor and dead-air floor ride along
    // with the switch, read for the domain this session captures in.
    let voice_processed = ar.recorder.mic_voice_processed();
    let levels = recording::speech_levels::SpeechLevels::load(app.profile.root());
    let speech_env_min = levels.live_speech_env_min(&source.description, voice_processed);
    let dead_air_floor = levels.dead_air_floor_i16(&source.description, voice_processed);
    ar.recorder.switch_mic(source, speech_env_min, dead_air_floor)?;
    // Keep the FIRST flip of the session; stop restores the original profile.
    if ar.bt_profile_flip.is_none() {
        ar.bt_profile_flip = flip;
    }
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

/// What becomes of the session after its recorder stops.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopMode {
    /// The session is kept for finalization: the live pipeline drains.
    Keep,
    /// The session is deleted next: the live pipeline is abandoned.
    Discard,
}

/// Complete teardown for a taken [`ActiveRecording`]: perf flag off, stop
/// the recorder, restore a flipped BT profile (even when the stop failed).
/// EVERY stop path — stop, cancel, delete-active, app-close flush — must go
/// through this; inlining `recorder.stop()` at a call site silently skips
/// the profile restore and leaves the headset on the telephony profile.
pub fn teardown(mut ar: ActiveRecording, mode: StopMode) -> Result<std::path::PathBuf> {
    crate::perf::set_recording_active(false);
    let elapsed = now_unix().saturating_sub(ar.started_at_unix_seconds);
    let flip = ar.bt_profile_flip.take();
    let live = ar.live_mode_label.clone();
    let result = match mode {
        StopMode::Keep => ar.recorder.stop(),
        StopMode::Discard => ar.recorder.discard(),
    };
    if let Some(flip) = flip {
        audio_engine::bt_profile::restore_profile(&flip);
    }
    let final_root = result?;
    log::info!(
        "recording: stopped after {elapsed}s (live={live}) -> {}",
        final_root.display()
    );
    Ok(final_root)
}

pub fn stop_impl(active: &Mutex<Option<ActiveRecording>>) -> Result<String> {
    let ar = active.lock().unwrap().take().ok_or(AppError::NotRecording)?;
    teardown(ar, StopMode::Keep).map(|p| p.to_string_lossy().into_owned())
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

/// The title a new recording carries when nobody typed one: this machine's
/// wall clock as "3:42 PM" (`hour12`) or "15:42".
pub fn default_recording_title(now: chrono::DateTime<chrono::Local>, hour12: bool) -> String {
    if hour12 {
        now.format("%-I:%M %p").to_string()
    } else {
        now.format("%-H:%M").to_string()
    }
}

/// The title a recording is stored under: the typed title, or the clock title
/// when nothing was typed.
pub fn stored_title(typed: Option<&str>, now: chrono::DateTime<chrono::Local>, hour12: bool) -> String {
    match typed.map(str::trim) {
        Some(t) if !t.is_empty() => t.to_string(),
        _ => default_recording_title(now, hour12),
    }
}

/// A device the automatic chain may land on: a real input, never a
/// software-synthesized one.
fn is_real_mic(s: &audio_engine::source::Source) -> bool {
    s.kind == SourceKind::Mic && !s.is_virtual
}

/// The microphone a hotkey start records from: this machine's stored
/// default, else the system default input, else the first real mic listed.
pub fn pick_mic(
    default_key: Option<&str>,
    system_default_key: Option<&str>,
    sources: &[audio_engine::source::Source],
) -> Option<u32> {
    if let Some(key) = default_key {
        // A stored key is honoured as chosen, virtual or not.
        if let Some(s) = sources.iter().find(|s| s.key.0 == key) {
            return Some(s.id);
        }
    }
    if let Some(key) = system_default_key {
        if let Some(s) = sources.iter().find(|s| is_real_mic(s) && s.key.0 == key) {
            return Some(s.id);
        }
    }
    sources.iter().find(|s| is_real_mic(s)).map(|s| s.id)
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
    let bytes = serde_json::to_vec_pretty(m)
        .map_err(|e| AppError::Config(format!("encode manifest: {e}")))?;
    syncsafe::write_atomic(&mp, &bytes).map_err(|e| AppError::Config(format!("write manifest: {e}")))?;
    Ok(())
}

#[cfg(test)]
mod live_mode_tests {
    use super::*;

    #[test]
    fn build_live_mode_is_off_without_a_local_whisper_model() {
        // No model file available anywhere → record-only, except on macOS,
        // where the OS SpeechAnalyzer engine needs no bundled model.
        std::env::remove_var("DAISY_WHISPER_MODEL_DIR");
        let ms = crate::settings::MachineSettings::default();
        let empty = tempfile::tempdir().unwrap();
        let m = build_live_mode(&ms, empty.path(), &[]);
        if cfg!(target_os = "macos") {
            match m {
                LiveMode::Realtime { client, .. } => assert_eq!(client.name(), "speechanalyzer"),
                LiveMode::Off => panic!("expected the OS SpeechAnalyzer backend"),
            }
        } else {
            assert!(matches!(m, LiveMode::Off));
        }
    }

    #[test]
    fn build_live_mode_is_off_while_the_planned_model_is_still_downloading() {
        // First-run setup writes the floor engine into asr.active before its
        // files exist; nothing resolves until the download lands.
        std::env::remove_var("DAISY_WHISPER_MODEL_DIR");
        std::env::remove_var("DAISY_PARAKEET_MODEL_DIR");
        let mut ms = crate::settings::MachineSettings::default();
        ms.asr.active = Some("parakeet-tdt-0.6b-v2-int8".into());
        let empty = tempfile::tempdir().unwrap();
        let m = build_live_mode(&ms, empty.path(), &[]);
        if cfg!(target_os = "macos") {
            match m {
                LiveMode::Realtime { client, .. } => assert_eq!(client.name(), "speechanalyzer"),
                LiveMode::Off => panic!("expected the OS SpeechAnalyzer backend"),
            }
        } else {
            assert!(matches!(m, LiveMode::Off), "an uninstalled active model records only");
        }
    }

    #[test]
    fn build_live_mode_dispatches_parakeet_by_active_id() {
        std::env::remove_var("DAISY_WHISPER_MODEL_DIR");
        let mut ms = crate::settings::MachineSettings::default();
        ms.asr.active = Some("parakeet-tdt-0.6b-v2-int8".into());
        let dir = tempfile::tempdir().unwrap();
        // The backend loads lazily; the full pinned file set has to exist.
        let row = providers_local::registry::row("parakeet-tdt-0.6b-v2-int8").unwrap();
        let d = dir.path().join("parakeet/parakeet-tdt-0.6b-v2-int8");
        syncsafe::create_dir_all(&d).unwrap();
        for (n, _) in row.files {
            syncsafe::write(d.join(n), b"stub").unwrap();
        }
        let m = build_live_mode(&ms, dir.path(), &[]);
        match m {
            LiveMode::Realtime { client, .. } => assert_eq!(client.name(), "parakeet-local"),
            LiveMode::Off => panic!("expected parakeet realtime backend"),
        }
    }

    #[test]
    fn missing_parakeet_dir_falls_back_to_whisper() {
        std::env::remove_var("DAISY_WHISPER_MODEL_DIR");
        let mut ms = crate::settings::MachineSettings::default();
        ms.asr.active = Some("parakeet-tdt-0.6b-v2-int8".into());
        let dir = tempfile::tempdir().unwrap();
        syncsafe::write(dir.path().join("ggml-base.en.bin"), b"stub").unwrap();
        let m = build_live_mode(&ms, dir.path(), &[]);
        // macOS resolves the OS SpeechAnalyzer engine ahead of the whisper
        // stub; the other platforms fall back to the local whisper model.
        let expected = if cfg!(target_os = "macos") { "speechanalyzer" } else { "whisper-local" };
        match m {
            LiveMode::Realtime { client, .. } => assert_eq!(client.name(), expected),
            LiveMode::Off => panic!("expected a realtime fallback backend"),
        }
    }

    #[test]
    fn build_live_mode_decodes_even_when_captions_display_is_off() {
        // Single-pass: the live decode is the transcript, so it runs whenever
        // a model is present; show_live_captions only controls the display.
        std::env::remove_var("DAISY_WHISPER_MODEL_DIR");
        let mut ms = crate::settings::MachineSettings::default();
        ms.show_live_captions = false;
        let dir = tempfile::tempdir().unwrap();
        // Stub ggml file: the realtime backend loads lazily, so a fake file
        // satisfies model resolution without a real model.
        syncsafe::write(dir.path().join("ggml-base.en.bin"), b"stub").unwrap();
        let m = build_live_mode(&ms, dir.path(), &[]);
        assert!(matches!(m, LiveMode::Realtime { .. }));
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
fn apply_start_metadata(session_dir: &std::path::Path, req: &StartRequest, hour12: bool) -> Result<()> {
    let mut m = read_manifest_at(session_dir)?;
    m.title = Some(stored_title(req.title.as_deref(), chrono::Local::now(), hour12));
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

#[cfg(test)]
mod tests {
    use super::{default_recording_title, is_stale_device_err, pick_mic, quick_start_request, stored_title};
    use audio_engine::source::SourceKind;

    #[test]
    fn stale_device_error_shapes_match() {
        assert!(is_stale_device_err(
            "recording: macOS audio: mic auhal start rc=-10851"
        ));
        assert!(is_stale_device_err("IAudioClient init failed: 0x88890004"));
        assert!(is_stale_device_err("AUDCLNT_E_DEVICE_INVALIDATED"));
    }

    #[test]
    fn ordinary_errors_do_not_match() {
        assert!(!is_stale_device_err("mic source 42 not found"));
        assert!(!is_stale_device_err("permission denied"));
        assert!(!is_stale_device_err("rc=-108"));
    }

    #[test]
    fn the_default_title_follows_the_hour12_flag() {
        use chrono::TimeZone;
        let afternoon = chrono::Local.with_ymd_and_hms(2026, 9, 16, 15, 42, 0).unwrap();
        assert_eq!(default_recording_title(afternoon, true), "3:42 PM");
        assert_eq!(default_recording_title(afternoon, false), "15:42");
        let past_midnight = chrono::Local.with_ymd_and_hms(2026, 9, 16, 0, 5, 0).unwrap();
        assert_eq!(default_recording_title(past_midnight, true), "12:05 AM");
        assert_eq!(default_recording_title(past_midnight, false), "0:05");
        let noon = chrono::Local.with_ymd_and_hms(2026, 9, 16, 12, 0, 0).unwrap();
        assert_eq!(default_recording_title(noon, true), "12:00 PM");
        assert_eq!(default_recording_title(noon, false), "12:00");
    }

    #[test]
    fn a_missing_or_blank_title_becomes_the_clock_title() {
        use chrono::TimeZone;
        let now = chrono::Local.with_ymd_and_hms(2026, 9, 16, 15, 42, 0).unwrap();
        assert_eq!(stored_title(None, now, true), "3:42 PM");
        assert_eq!(stored_title(Some(""), now, true), "3:42 PM");
        assert_eq!(stored_title(Some("   "), now, true), "3:42 PM");
        assert_eq!(stored_title(Some("  Standup "), now, true), "Standup");
        assert_eq!(stored_title(None, now, false), "15:42");
    }

    fn a_source(id: u32, key: &str, kind: SourceKind, is_virtual: bool) -> audio_engine::source::Source {
        audio_engine::source::Source {
            id,
            key: audio_engine::topology::DeviceKey(key.to_string()),
            node_name: format!("node-{key}"),
            description: format!("Device {key}"),
            kind,
            default_sample_rate: 48_000,
            default_channels: 1,
            is_virtual,
        }
    }

    #[test]
    fn the_mic_chain_falls_from_the_stored_key_to_the_first_real_mic() {
        let sources = vec![
            a_source(1, "echo-cancel", SourceKind::Mic, true),
            a_source(2, "built-in", SourceKind::Mic, false),
            a_source(3, "usb-mic", SourceKind::Mic, false),
            a_source(4, "sink.monitor", SourceKind::Monitor, false),
        ];

        assert_eq!(pick_mic(Some("usb-mic"), Some("built-in"), &sources), Some(3));
        assert_eq!(
            pick_mic(Some("unplugged"), Some("built-in"), &sources),
            Some(2),
            "an unresolvable stored key falls to the system default"
        );
        assert_eq!(
            pick_mic(None, None, &sources),
            Some(2),
            "no preference at all takes the first non-virtual mic"
        );
        assert_eq!(
            pick_mic(None, Some("sink.monitor"), &sources),
            Some(2),
            "a monitor is never the system input"
        );
        assert_eq!(pick_mic(None, None, &[]), None);
        assert_eq!(
            pick_mic(None, None, &[a_source(9, "echo-cancel", SourceKind::Mic, true)]),
            None,
            "a virtual device is not a microphone"
        );
    }

    #[test]
    fn a_quick_start_asks_for_a_solo_untagged_recording() {
        let req = quick_start_request("3:42 PM".to_string(), 7);
        assert_eq!(req.mic_source_id, 7);
        assert_eq!(req.title.as_deref(), Some("3:42 PM"));
        assert_eq!(req.single_local_speaker, Some(true));
        assert_eq!(req.system_source_id, None, "the virtual sink carries system audio");
        assert_eq!(req.session_id, None, "the session id is generated");
        assert!(req.tag_ids.is_empty());
        assert!(req.attendees.is_empty());
        assert!(req.calendar_link.is_none());
        assert!(req.notes_md.is_none());
        assert!(req.meeting_id.is_none());
    }
}
