#![cfg_attr(all(not(debug_assertions), target_os = "windows"), windows_subsystem = "windows")]

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use tauri::{Emitter, Manager, State};
use tauri_app_core::commands::bootstrap::{
    bootstrap_set_impl, bootstrap_status_impl, BootstrapStatus,
};
use tauri_app_core::commands::migrate::{move_profile_impl, probe_profile_dir_impl, ProfileProbe};
use tauri_app_core::commands::library::{list_sessions_impl, SessionListEntry};
use tauri_app_core::commands::lifecycle::{
    change_vault_passphrase_impl, init_vault_impl, init_vault_machine_mode_impl,
    list_providers_impl, lock_vault_impl, read_vault_kind, reset_vault_impl, set_provider_impl,
    switch_vault_mode_impl,
    unlock_if_machine_mode_impl, unlock_vault_impl, vault_status_impl, ProviderListEntry,
    VaultStatus,
};
use tauri_app_core::commands::meeting::{
    session_assign_tags_impl, session_meta_get_impl, session_meta_update_impl,
    session_notes_load_impl, session_notes_save_impl, SessionMeta, SessionMetaUpdate,
};
use tauri_app_core::commands::search::{search_sessions_impl, SearchRequest, SessionHit};
use tauri_app_core::commands::summary::{
    summary_load_impl, summary_regenerate_impl, summary_save_edit_impl,
    summary_provider_status_impl_from_state, SummaryProviderStatus,
};
use tauri_app_core::commands::tags::{
    create_tag_impl, delete_tag_impl, list_tags_impl, search_tags_impl, update_tag_impl,
    CreateTagRequest, DeleteTagResult, UpdateTagRequest,
};
use tauri_app_core::commands::settings::{
    list_audio_sources_impl, read_settings_impl, write_settings_impl, AudioSourceInfo,
};
use tauri_app_core::settings::Settings;
use tauri_app_core::commands::pipeline::{
    dedup_impl, polish_impl, transcribe_impl, DedupRequest, DedupSummary, PolishRequest,
    PolishSummary, TranscribeRequest,
};
use tauri_app_core::commands::recording::{
    current_impl, pause_impl, recording_snapshot_impl, resume_impl, start_recording_impl,
    ActiveRecording, LiveEventPayload, RecordingSnapshot, StartRequest,
};
use tauri_app_core::commands::recordings::{
    delete_recordings_impl, recordings_stats_impl, DeleteSummary, RecordingsStats,
};
use tauri_app_core::commands::history::{read_history, HistoryEntry};
use tauri_app_core::commands::integrations::{
    delete_integration_impl, integration_push_impl, list_integrations_impl, upsert_integration_impl,
    IntegrationPublic, UpsertIntegration,
};
use tauri_app_core::commands::session::{
    delete_session_impl, list_session_speakers_impl, read_session_impl,
    rerender_session_transcript_impl,
    remove_speaker_cluster_impl, set_session_speaker_label_impl,
    SessionSpeaker, SessionView,
};
use tauri_app_core::error::AppError;
use tauri_app_core::mini_window::{MiniWindowConfig, MAIN_LABEL, MINI_LABEL, REMINDER_LABEL};
use tauri_app_core::profile::ProfileDir;
use tauri_app_core::state::{AppState, ProviderConfig, Tag, VaultState};

struct ActiveRecordingHandle(Mutex<Option<ActiveRecording>>);

/// Builds the frameless mini-window (created hidden). Idempotent: returns
/// the existing window if already built. Must be called on the main thread
/// (Tauri's `setup` hook or the main event loop).
fn ensure_mini_window(app: &tauri::AppHandle) -> Result<tauri::WebviewWindow, String> {
    use tauri::{Manager, WebviewUrl, WebviewWindowBuilder, WindowEvent};
    if let Some(w) = app.get_webview_window(MINI_LABEL) {
        return Ok(w);
    }
    let cfg = MiniWindowConfig::card();
    let built = WebviewWindowBuilder::new(app, MINI_LABEL, WebviewUrl::App("index.html".into()))
        .title("Daisy")
        .inner_size(cfg.width, cfg.height)
        .decorations(cfg.decorations)
        .always_on_top(cfg.always_on_top)
        .resizable(cfg.resizable)
        .skip_taskbar(cfg.skip_taskbar)
        .visible(false)
        .build()
        .map_err(|e| format!("build mini window: {e}"))?;

    // A WM close (Alt+F4 / window-menu) is intercepted: the mini stays alive
    // and the main window is restored.
    let app_for_close = app.clone();
    built.on_window_event(move |event| {
        if let WindowEvent::CloseRequested { api, .. } = event {
            api.prevent_close();
            let _ = show_main_window(app_for_close.clone());
        }
    });
    Ok(built)
}

/// Shows + focuses the mini-window and hides the main window. The mini
/// webview is pre-built at startup by `ensure_mini_window`; this only
/// toggles visibility.
#[tauri::command]
fn show_mini_window(app: tauri::AppHandle) -> Result<(), String> {
    use tauri::Manager;
    let mini = ensure_mini_window(&app)?;
    mini.show().map_err(|e| format!("show mini window: {e}"))?;
    mini.set_focus().map_err(|e| format!("focus mini window: {e}"))?;
    if let Some(main) = app.get_webview_window(MAIN_LABEL) {
        main.hide().map_err(|e| format!("hide main window: {e}"))?;
    }
    Ok(())
}

/// Shows + focuses the main window and hides the mini-window.
#[tauri::command]
fn show_main_window(app: tauri::AppHandle) -> Result<(), String> {
    use tauri::Manager;
    if let Some(mini) = app.get_webview_window(MINI_LABEL) {
        mini.hide().map_err(|e| format!("hide mini window: {e}"))?;
    }
    if let Some(main) = app.get_webview_window(MAIN_LABEL) {
        main.show().map_err(|e| format!("show main window: {e}"))?;
        main.set_focus().map_err(|e| format!("focus main window: {e}"))?;
    }
    Ok(())
}

/// Title of the meeting the reminder popup is currently showing. The
/// reminder webview reads it on mount via `reminder_payload`.
struct ReminderState(Mutex<Option<String>>);

// ---- Whisper model management ---------------------------------------------

/// Per-request cancel flags for in-flight model downloads.
fn whisper_cancel_registry() -> &'static Mutex<HashMap<String, Arc<AtomicBool>>> {
    static R: OnceLock<Mutex<HashMap<String, Arc<AtomicBool>>>> = OnceLock::new();
    R.get_or_init(|| Mutex::new(HashMap::new()))
}

#[derive(serde::Serialize, Clone)]
struct WhisperDownloadProgress {
    request_id: String,
    downloaded: u64,
    total: Option<u64>,
}

/// List installed/available Whisper models, which is active, and which can be deleted.
#[tauri::command]
fn list_whisper_models(
    state: State<'_, AppState>,
) -> Result<Vec<tauri_app_core::commands::whisper_models::WhisperModelInfo>, AppError> {
    tauri_app_core::commands::whisper_models::list_whisper_models_impl(&state)
}

/// Switch the model used for transcription. `size` ∈ `providers_local::KNOWN_MODELS`.
#[tauri::command]
fn set_active_whisper_model(size: String, state: State<'_, AppState>) -> Result<(), AppError> {
    tauri_app_core::commands::whisper_models::set_active_whisper_model_impl(&state, &size)
}

/// Delete a downloaded model (never the bundled one; resets active to bundled if needed).
#[tauri::command]
fn delete_whisper_model(size: String, state: State<'_, AppState>) -> Result<(), AppError> {
    tauri_app_core::commands::whisper_models::delete_whisper_model_impl(&state, &size)
}

/// Download a Whisper ggml model into `<profile>/models/` and return its path.
/// Progress streams as `"whisper-download:progress"` events tagged by `request_id`.
#[tauri::command]
async fn download_whisper_model(
    request_id: String,
    size: String,
    app: tauri::AppHandle,
) -> Result<String, AppError> {
    use providers_local::{download_ggml_model_opts, DownloadOpts};

    let dest_dir = {
        let state = app.state::<AppState>();
        state.profile.models_dir()
    };

    let cancel = Arc::new(AtomicBool::new(false));
    whisper_cancel_registry()
        .lock()
        .unwrap()
        .insert(request_id.clone(), cancel.clone());

    let app_emit = app.clone();
    let req_id_emit = request_id.clone();
    let progress_cb: Box<dyn FnMut(u64, Option<u64>) + Send + 'static> =
        Box::new(move |downloaded: u64, total: Option<u64>| {
            let _ = app_emit.emit(
                "whisper-download:progress",
                WhisperDownloadProgress {
                    request_id: req_id_emit.clone(),
                    downloaded,
                    total,
                },
            );
        });

    let opts = DownloadOpts {
        progress: Some(progress_cb),
        cancel: Some(cancel),
        ..Default::default()
    };

    let result = tokio::task::spawn_blocking(move || download_ggml_model_opts(&size, &dest_dir, opts))
        .await
        .map_err(|e| AppError::Io(format!("download task join: {e}")))?
        .map_err(|e| AppError::Config(format!("download whisper model: {e}")));

    whisper_cancel_registry().lock().unwrap().remove(&request_id);
    result.map(|p| p.to_string_lossy().into_owned())
}

/// Cancel an in-progress `download_whisper_model` by its `request_id` (no-op if unknown).
#[tauri::command]
fn cancel_whisper_download(request_id: String) -> Result<(), AppError> {
    use std::sync::atomic::Ordering;
    if let Some(flag) = whisper_cancel_registry().lock().unwrap().get(&request_id) {
        flag.store(true, Ordering::SeqCst);
    }
    Ok(())
}

/// Builds the frameless meeting-reminder popup (created hidden). Idempotent.
/// Must be called on the main thread (setup hook). A WM close hides it; the
/// main window stays put.
fn ensure_reminder_window(app: &tauri::AppHandle) -> Result<tauri::WebviewWindow, String> {
    use tauri::{Manager, WebviewUrl, WebviewWindowBuilder, WindowEvent};
    if let Some(w) = app.get_webview_window(REMINDER_LABEL) {
        return Ok(w);
    }
    let cfg = MiniWindowConfig::reminder();
    let built = WebviewWindowBuilder::new(app, REMINDER_LABEL, WebviewUrl::App("index.html".into()))
        .title("Daisy")
        .inner_size(cfg.width, cfg.height)
        .decorations(cfg.decorations)
        .always_on_top(cfg.always_on_top)
        .resizable(cfg.resizable)
        .skip_taskbar(cfg.skip_taskbar)
        .visible(false)
        .build()
        .map_err(|e| format!("build reminder window: {e}"))?;
    built.on_window_event(move |event| {
        if let WindowEvent::CloseRequested { api, .. } = event {
            api.prevent_close();
        }
    });
    Ok(built)
}

/// Positions the reminder popup at the top-right of the current monitor:
/// the main window's monitor, else the mini's, else the reminder's own.
/// Best-effort — with no monitor resolved, the OS default position is kept.
/// Computed fresh on every show.
fn place_reminder_top_right(win: &tauri::WebviewWindow) {
    use tauri::Manager;
    let app = win.app_handle();
    let monitor = app
        .get_webview_window(MAIN_LABEL)
        .and_then(|w| w.current_monitor().ok().flatten())
        .or_else(|| {
            app.get_webview_window(MINI_LABEL)
                .and_then(|w| w.current_monitor().ok().flatten())
        })
        .or_else(|| win.current_monitor().ok().flatten());
    if let Some(mon) = monitor {
        let msize = mon.size();
        let mpos = mon.position();
        let scale = mon.scale_factor();
        let win_w = win
            .outer_size()
            .map(|s| s.width as i32)
            .unwrap_or((MiniWindowConfig::reminder().width * scale) as i32);
        let margin = (16.0 * scale) as i32;
        let x = mpos.x + msize.width as i32 - win_w - margin;
        let y = mpos.y + margin;
        let _ = win.set_position(tauri::PhysicalPosition::new(x, y));
    }
}

/// Shows the meeting-reminder popup for `title`: stores the title, positions
/// the window top-right, shows + focuses it, and pushes the title to the
/// reminder webview.
#[tauri::command]
fn show_reminder_window(app: tauri::AppHandle, title: String) -> Result<(), String> {
    use tauri::{Emitter, Manager};
    log::info!("reminder: showing popup for {title:?}");
    let win = ensure_reminder_window(&app)?;
    if let Some(state) = app.try_state::<ReminderState>() {
        *state.0.lock().unwrap() = Some(title.clone());
    }
    place_reminder_top_right(&win);
    win.show().map_err(|e| format!("show reminder window: {e}"))?;
    win.set_focus().map_err(|e| format!("focus reminder window: {e}"))?;
    let _ = app.emit_to(REMINDER_LABEL, "reminder:show", title);
    Ok(())
}

/// The title the reminder popup should display (set by `show_reminder_window`).
#[tauri::command]
fn reminder_payload(app: tauri::AppHandle) -> Option<String> {
    use tauri::Manager;
    app.try_state::<ReminderState>()
        .and_then(|s| s.0.lock().unwrap().clone())
}

/// Dismisses the reminder popup (always hides it) and, when `open`, signals
/// the main window to start recording the pending meeting.
#[tauri::command]
fn reminder_action(app: tauri::AppHandle, open: bool) -> Result<(), String> {
    use tauri::{Emitter, Manager};
    if let Some(w) = app.get_webview_window(REMINDER_LABEL) {
        let _ = w.hide();
    }
    if open {
        let _ = app.emit("daisy://reminder/open", ());
    }
    Ok(())
}

#[tauri::command]
async fn list_sessions(app_handle: tauri::AppHandle) -> Result<Vec<SessionListEntry>, AppError> {
    tauri::async_runtime::spawn_blocking(move || {
        list_sessions_impl(&app_handle.state::<AppState>())
    })
    .await
    .map_err(|e| AppError::Config(format!("list sessions task failed: {e}")))?
}

#[tauri::command]
fn read_session(session_id: String, state: State<'_, AppState>) -> Result<SessionView, AppError> {
    read_session_impl(&state, &session_id)
}

#[tauri::command]
async fn mcp_status(
    app_handle: tauri::AppHandle,
) -> Result<tauri_app_core::mcp::server::McpStatus, AppError> {
    tauri_app_core::mcp::server::mcp_status_impl(&app_handle)
}

/// Re-read settings and start/stop/restart the MCP listener to match.
/// The frontend calls this right after writing mcp_enabled.
#[tauri::command]
async fn mcp_apply(
    app_handle: tauri::AppHandle,
) -> Result<tauri_app_core::mcp::server::McpStatus, AppError> {
    tauri_app_core::mcp::server::apply_mcp_state(&app_handle)?;
    tauri_app_core::mcp::server::mcp_status_impl(&app_handle)
}

#[tauri::command]
async fn mcp_regenerate_token(
    app_handle: tauri::AppHandle,
) -> Result<tauri_app_core::mcp::server::McpStatus, AppError> {
    tauri_app_core::mcp::server::mcp_regenerate_token_impl(&app_handle)
}

/// Probes whether a loopback port is free for the MCP server.
#[tauri::command]
async fn mcp_port_available(
    port: u16,
    app_handle: tauri::AppHandle,
) -> Result<bool, AppError> {
    Ok(tauri_app_core::mcp::server::mcp_port_available_impl(&app_handle, port))
}

#[tauri::command]
fn set_session_speaker_label(
    session_id: String,
    cluster_id: u32,
    display_name: String,
    email: Option<String>,
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), AppError> {
    set_session_speaker_label_impl(&state, &session_id, cluster_id, display_name, email)?;
    // Re-renders transcript.md; read_session returns the pre-rendered file.
    let _ = tauri_app_core::commands::session::rerender_session_transcript_impl(&state, &session_id);
    tauri_app_core::library_events::emit(
        &app_handle,
        tauri_app_core::library_events::LibraryChangeKind::Updated,
        &session_id,
    );
    Ok(())
}

#[tauri::command]
fn remove_speaker_cluster(
    session_id: String,
    cluster_id: u32,
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), AppError> {
    remove_speaker_cluster_impl(&state, &session_id, cluster_id)?;
    let _ = tauri_app_core::commands::session::rerender_session_transcript_impl(&state, &session_id);
    tauri_app_core::library_events::emit(
        &app_handle,
        tauri_app_core::library_events::LibraryChangeKind::Updated,
        &session_id,
    );
    Ok(())
}

#[tauri::command]
fn add_session_speaker(
    session_id: String,
    display_name: String,
    email: Option<String>,
    voiceprint_id: Option<String>,
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<u32, AppError> {
    let cluster_id = tauri_app_core::commands::session::add_session_speaker_impl(
        &state,
        &session_id,
        display_name,
        email,
        voiceprint_id,
    )?;
    let _ = tauri_app_core::commands::session::rerender_session_transcript_impl(&state, &session_id);
    tauri_app_core::library_events::emit(
        &app_handle,
        tauri_app_core::library_events::LibraryChangeKind::Updated,
        &session_id,
    );
    Ok(cluster_id)
}

#[tauri::command]
fn list_session_speakers(
    session_id: String,
    state: State<'_, AppState>,
) -> Result<Vec<SessionSpeaker>, AppError> {
    list_session_speakers_impl(&state, &session_id)
}

#[tauri::command]
fn rerender_session_transcript(
    session_id: String,
    state: State<'_, AppState>,
) -> Result<(), AppError> {
    rerender_session_transcript_impl(&state, &session_id)
}

#[tauri::command]
async fn session_speaker_sample_audio_bytes(
    session_id: String,
    cluster_id: u32,
    app_handle: tauri::AppHandle,
) -> Result<tauri::ipc::Response, AppError> {
    // Returns a binary IPC Response; Tauri serializes a bare Vec<u8> as a
    // JSON number array.
    tauri::async_runtime::spawn_blocking(move || {
        let state = app_handle.state::<AppState>();
        tauri_app_core::commands::session::session_speaker_sample_audio_impl(
            &state,
            &session_id,
            cluster_id,
        )
        .map(tauri::ipc::Response::new)
    })
    .await
    .map_err(|e| AppError::Config(format!("sample task failed: {e}")))?
}

/// Transcript text of the same segments the sample-audio clip plays.
#[tauri::command]
async fn session_speaker_sample_text(
    session_id: String,
    cluster_id: u32,
    app_handle: tauri::AppHandle,
) -> Result<String, AppError> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app_handle.state::<AppState>();
        tauri_app_core::commands::session::session_speaker_sample_text_impl(
            &state,
            &session_id,
            cluster_id,
        )
    })
    .await
    .map_err(|e| AppError::Config(format!("sample text task failed: {e}")))?
}

#[tauri::command]
async fn delete_session(
    session_id: String,
    active: State<'_, ActiveRecordingHandle>,
    app_handle: tauri::AppHandle,
) -> Result<(), AppError> {
    // Deleting the currently-recording session stops the recorder first.
    let active_ar = {
        let mut slot = active.0.lock().unwrap();
        let is_active = slot
            .as_ref()
            .and_then(|ar| ar.recorder.session_root().file_name().map(|n| n.to_string_lossy().into_owned()))
            .map(|id| id == session_id)
            .unwrap_or(false);
        if is_active { slot.take() } else { None }
    };
    if let Some(ar) = active_ar {
        run_on_os_thread(move || { let _ = ar.recorder.stop(); })
            .await
            .map_err(AppError::Recording)?;
        let _ = app_handle.emit("recording:state", "stopped");
    let _ = app_handle.emit::<Option<RecordingSnapshot>>("recording:snapshot", None);
    }
    // The snapshot is taken before the dir is removed; Deleted-trigger
    // workflows match on manifest fields.
    let pre_delete_snap = tauri_app_core::commands::workflows::snapshot_for_session(
        &app_handle.state::<AppState>(),
        &session_id,
    )
    .ok();
    // `remove_dir_all` is blocking IO; it runs on a blocking thread.
    let sid_for_emit = session_id.clone();
    let emit_handle = app_handle.clone();
    let result = tauri::async_runtime::spawn_blocking(move || {
        let state = app_handle.state::<AppState>();
        delete_session_impl(&state, &session_id)
    })
    .await
    .map_err(|e| AppError::Config(format!("delete task failed: {e}")))?;
    tauri_app_core::library_events::emit(
        &emit_handle,
        tauri_app_core::library_events::LibraryChangeKind::Deleted,
        &sid_for_emit,
    );
    if result.is_ok() {
        if let Some(snap) = pre_delete_snap {
            workflow_dispatch_with_snapshot(
                &emit_handle,
                tauri_app_core::commands::workflows::TriggerEvent::Deleted,
                snap,
            );
        }
    }
    result
}

/// Backend gate for paid features: a valid license or active trial. Called
/// at the top of the create/process commands; read/open/delete/export
/// commands are ungated.
fn ensure_licensed() -> Result<(), AppError> {
    if tauri_app_core::commands::license::features_enabled() {
        Ok(())
    } else {
        Err(AppError::LicenseExpired)
    }
}

#[tauri::command]
fn start_recording(
    req: StartRequest,
    state: State<'_, AppState>,
    active: State<'_, ActiveRecordingHandle>,
    app_handle: tauri::AppHandle,
) -> Result<RecordingSnapshot, AppError> {
    ensure_licensed()?;
    let (snapshot, live_events_rx) = start_recording_impl(&state, &active.0, req)?;

    let _ = app_handle.emit("recording:state", "recording");
    let _ = app_handle.emit("recording:snapshot", Some(&snapshot));
    tauri_app_core::library_events::emit(
        &app_handle,
        tauri_app_core::library_events::LibraryChangeKind::Added,
        &snapshot.session_id,
    );

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
                let track = match event.track {
                    recording::live_transcript::LiveTrack::Mic => "mic",
                    recording::live_transcript::LiveTrack::System => "system",
                };
                let kind_str = match &event.kind {
                    recording::live_pipeline::LivePipelineEventKind::Interim { .. } => "interim",
                    recording::live_pipeline::LivePipelineEventKind::Final { .. } => "final",
                    recording::live_pipeline::LivePipelineEventKind::Error(_) => "error",
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

#[tauri::command]
fn pause_recording(
    state: State<'_, AppState>,
    active: State<'_, ActiveRecordingHandle>,
    app_handle: tauri::AppHandle,
) -> Result<&'static str, AppError> {
    let r = pause_impl(&state, &active.0)?;
    let _ = app_handle.emit("recording:state", "paused");
    if let Some(snap) = recording_snapshot_impl(&active.0) {
        let _ = app_handle.emit("recording:snapshot", &snap);
    }
    Ok(r)
}

/// Switch the recording mic mid-session (e.g. to AirPods). Fail-safe: recording
/// continues on the current mic if the new device can't start.
#[tauri::command]
fn switch_recording_mic(
    source_id: u32,
    active: State<'_, ActiveRecordingHandle>,
    state: State<'_, AppState>,
) -> Result<(), AppError> {
    tauri_app_core::commands::recording::switch_mic_impl(&state, &active.0, source_id)
}

/// Mute/unmute the local mic mid-recording (record system audio only).
#[tauri::command]
fn set_mic_muted(
    muted: bool,
    active: State<'_, ActiveRecordingHandle>,
) -> Result<(), AppError> {
    tauri_app_core::commands::recording::set_mic_muted_impl(&active.0, muted)
}

#[tauri::command]
fn resume_recording(
    state: State<'_, AppState>,
    active: State<'_, ActiveRecordingHandle>,
    app_handle: tauri::AppHandle,
) -> Result<&'static str, AppError> {
    let r = resume_impl(&state, &active.0)?;
    let _ = app_handle.emit("recording:state", "recording");
    if let Some(snap) = recording_snapshot_impl(&active.0) {
        let _ = app_handle.emit("recording:snapshot", &snap);
    }
    Ok(r)
}

/// Runs `f` on a plain OS thread (no tokio context) and awaits its join via
/// `spawn_blocking`. `reqwest::blocking` requires a thread without ambient
/// tokio runtime context.
fn run_on_os_thread<T, F>(f: F) -> impl std::future::Future<Output = Result<T, String>>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let join_handle = std::thread::spawn(f);
    async move {
        tokio::task::spawn_blocking(move || join_handle.join())
            .await
            .map_err(|e| format!("blocking-pool join error: {e}"))?
            .map_err(|_| "os thread panicked".to_string())
    }
}

/// Rotates the active chunk if its open duration exceeds `interval_secs`.
/// Returns true when rotation happened. The frontend polls this every
/// minute; calls outside an active recording are no-ops.
#[tauri::command]
fn maybe_rotate_chunk(
    interval_secs: u64,
    active: State<'_, ActiveRecordingHandle>,
) -> Result<bool, AppError> {
    let mut slot = active.0.lock().unwrap();
    let Some(ar) = slot.as_mut() else { return Ok(false) };
    ar.recorder
        .maybe_rotate_chunk(interval_secs)
        .map_err(|e| AppError::Recording(e.to_string()))
}

#[tauri::command]
async fn stop_recording(
    active: State<'_, ActiveRecordingHandle>,
    app_handle: tauri::AppHandle,
) -> Result<String, AppError> {
    let ar = {
        let mut slot = active.0.lock().unwrap();
        slot.take().ok_or(AppError::NotRecording)?
    };
    // Stops only the recorder (releases the mic/system devices + the
    // heartbeat keeper). Finalize runs in-app via
    // `recording_finalize_and_summarize`, invoked by the frontend's Finish
    // flow; a finalize interrupted by app close is resumed on next launch by
    // the startup integrity audit.
    let result: Result<String, AppError> = run_on_os_thread(move || {
        let final_root = ar.recorder.stop()?;
        Ok(final_root.to_string_lossy().into_owned())
    })
    .await
    .map_err(AppError::Recording)?;
    let _ = app_handle.emit("recording:state", "stopped");
    let _ = app_handle.emit::<Option<RecordingSnapshot>>("recording:snapshot", None);
    result
}

/// Discards the active recording: stops the recorder (releasing devices +
/// the heartbeat keeper) and deletes its session directory. Does not
/// finalize/summarize.
#[tauri::command]
async fn cancel_recording(
    active: State<'_, ActiveRecordingHandle>,
    app_handle: tauri::AppHandle,
) -> Result<(), AppError> {
    let ar = {
        let mut slot = active.0.lock().unwrap();
        slot.take().ok_or(AppError::NotRecording)?
    };
    let root = ar.recorder.session_root().to_path_buf();
    run_on_os_thread(move || {
        // The stop runs first: the heartbeat keeper + pipeline release the
        // dir before it is deleted.
        let _ = ar.recorder.stop();
        if root.is_dir() {
            let _ = std::fs::remove_dir_all(&root);
        }
    })
    .await
    .map_err(AppError::Recording)?;
    let _ = app_handle.emit("recording:state", "stopped");
    let _ = app_handle.emit::<Option<RecordingSnapshot>>("recording:snapshot", None);
    Ok(())
}

#[tauri::command]
fn current_recording(active: State<'_, ActiveRecordingHandle>) -> Option<String> {
    current_impl(&active.0).map(|s| s.to_string())
}

#[tauri::command]
fn recording_snapshot(active: State<'_, ActiveRecordingHandle>) -> Option<RecordingSnapshot> {
    recording_snapshot_impl(&active.0)
}

#[tauri::command]
async fn transcribe(
    req: TranscribeRequest,
    state: State<'_, AppState>,
) -> Result<usize, AppError> {
    ensure_licensed()?;
    let profile = state.profile.clone();
    let result: Result<usize, AppError> = run_on_os_thread(move || {
        let app = AppState::new(profile);
        transcribe_impl(&app, req, None)
    })
    .await
    .map_err(AppError::Provider)?;
    result
}

#[tauri::command]
async fn dedup(
    req: DedupRequest,
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<DedupSummary, AppError> {
    let profile = state.profile.clone();
    let sid_for_emit = req.session_id.clone();
    let result: Result<DedupSummary, AppError> = run_on_os_thread(move || {
        let app = AppState::new(profile);
        dedup_impl(&app, req)
    })
    .await
    .map_err(AppError::Transcript)?;
    if result.is_ok() {
        tauri_app_core::library_events::emit(
            &app_handle,
            tauri_app_core::library_events::LibraryChangeKind::Updated,
            &sid_for_emit,
        );
    }
    result
}

// On-demand transcript polish: cleans punctuation/casing/mis-hearings and
// redacts secrets via the summary LLM, overwrites transcript.dedup.json,
// re-renders transcript.md, then emits library:changed.
#[tauri::command]
async fn polish(
    req: PolishRequest,
    app_handle: tauri::AppHandle,
) -> Result<PolishSummary, AppError> {
    ensure_licensed()?;
    let sid_for_emit = req.session_id.clone();
    let emit_handle = app_handle.clone();
    let result: Result<PolishSummary, AppError> = tauri::async_runtime::spawn_blocking(move || {
        let state = app_handle.state::<AppState>();
        let vault = app_handle.state::<VaultState>();
        polish_impl(&state, &vault, req)
    })
    .await
    .map_err(|e| AppError::Provider(format!("polish task failed: {e}")))?;
    if result.is_ok() {
        tauri_app_core::library_events::emit(
            &emit_handle,
            tauri_app_core::library_events::LibraryChangeKind::Updated,
            &sid_for_emit,
        );
    }
    result
}

/// Reads the on-disk finalize status sidecar for a session, or `None` if no
/// finalize has run (or the sidecar is absent/unparseable).
#[tauri::command]
fn read_finalize_status(
    session_id: String,
    state: State<'_, AppState>,
) -> Option<tauri_app_core::commands::finalize::FinalizeStatus> {
    let dir = state.profile.session_path(&session_id);
    // Reconciled against the manifest: a stale non-terminal sidecar on an
    // already-finalized session reads as `done`.
    tauri_app_core::commands::finalize::read_finalize_status_reconciled(&dir)
}

/// One final live-caption segment for the post-call "live while finalizing" view.
#[derive(serde::Serialize)]
struct LiveTranscriptSeg {
    track: String,
    start_ms: u32,
    end_ms: u32,
    text: String,
}

/// Reads the committed (final) live-caption lines for a session. Empty when
/// live was off or produced nothing.
#[tauri::command]
fn read_live_transcript(
    session_id: String,
    state: State<'_, AppState>,
) -> Result<Vec<LiveTranscriptSeg>, AppError> {
    let path = state
        .profile
        .session_path(&session_id)
        .join("live_transcript.jsonl");
    if !path.is_file() {
        return Ok(vec![]);
    }
    let lines = recording::live_transcript::read_all(&path)
        .map_err(|e| AppError::Config(format!("read live transcript: {e}")))?;
    Ok(lines
        .into_iter()
        .filter(|l| l.is_final)
        .map(|l| LiveTranscriptSeg {
            track: match l.track {
                recording::live_transcript::LiveTrack::Mic => "mic".to_string(),
                recording::live_transcript::LiveTrack::System => "system".to_string(),
            },
            start_ms: l.start_ms,
            end_ms: l.end_ms,
            text: l.text,
        })
        .collect())
}

#[tauri::command]
async fn recording_finalize_and_summarize(
    req: tauri_app_core::commands::finalize::FinalizeRequest,
    app_handle: tauri::AppHandle,
) -> Result<tauri_app_core::commands::finalize::FinalizeOutcome, AppError> {
    ensure_licensed()?;
    let sid = req.session_id.clone();
    let sid_for_emit = sid.clone();
    let emit_handle = app_handle.clone();
    let lib_emit_handle = app_handle.clone();
    let result = run_on_os_thread(move || {
        let state = app_handle.state::<AppState>();
        let vault = app_handle.state::<VaultState>();
        tauri_app_core::commands::finalize::finalize_and_summarize_impl(&state, &vault, req, |ev| {
            // The transcript is on disk once the summarize stage starts; an
            // open session view refreshes now instead of after the whole
            // cascade.
            if ev.stage == "summarizing" {
                tauri_app_core::library_events::emit(
                    &emit_handle,
                    tauri_app_core::library_events::LibraryChangeKind::Updated,
                    &sid,
                );
            }
            let _ = emit_handle.emit(&format!("daisy://session/{sid}/progress"), &ev);
        })
    })
    .await
    .map_err(AppError::Provider)?;
    match &result {
        Ok(tauri_app_core::commands::finalize::FinalizeOutcome::Completed { .. }) => {
            tauri_app_core::library_events::emit(
                &lib_emit_handle,
                tauri_app_core::library_events::LibraryChangeKind::Finalized,
                &sid_for_emit,
            );
        }
        // Paused at the speaker-label gate: the transcript + dedup are
        // already on disk; Updated is emitted.
        Ok(tauri_app_core::commands::finalize::FinalizeOutcome::NeedsLabels { .. }) => {
            tauri_app_core::library_events::emit(
                &lib_emit_handle,
                tauri_app_core::library_events::LibraryChangeKind::Updated,
                &sid_for_emit,
            );
        }
        Err(_) => {
            // Artifacts written before the failure (transcript, dedup) still
            // surface in an open session view.
            tauri_app_core::library_events::emit(
                &lib_emit_handle,
                tauri_app_core::library_events::LibraryChangeKind::Updated,
                &sid_for_emit,
            );
            if let Ok(snap) = tauri_app_core::commands::workflows::snapshot_for_session(
                &lib_emit_handle.state::<AppState>(),
                &sid_for_emit,
            ) {
                workflow_dispatch_with_snapshot(
                    &lib_emit_handle,
                    tauri_app_core::commands::workflows::TriggerEvent::FinalizeFailed,
                    snap,
                );
            }
        }
    }
    result
}

/// Marks a session "finalized" without running the cascade. Called by the
/// frontend after Regen-Transcript (and similar non-cascade recovery flows)
/// completes on a session whose manifest still has finalized_at_unix_seconds
/// = None.
#[tauri::command]
fn mark_session_complete(
    session_id: String,
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), AppError> {
    let res = tauri_app_core::commands::finalize::mark_session_complete_impl(&state, &session_id);
    if res.is_ok() {
        tauri_app_core::library_events::emit(
            &app_handle,
            tauri_app_core::library_events::LibraryChangeKind::Finalized,
            &session_id,
        );
    }
    res
}

/// Manually audits + repairs one session, including the paid LLM repairs
/// (summary/chapters) when a provider is configured + the vault is unlocked.
/// User-triggered; the startup sweep does the free repairs only. Returns the
/// number of repairs performed.
#[tauri::command]
async fn repair_session(
    session_id: String,
    app_handle: tauri::AppHandle,
) -> Result<u32, AppError> {
    let result: Result<u32, AppError> = run_on_os_thread(move || {
        let state = app_handle.state::<AppState>();
        let vault = app_handle.state::<VaultState>();
        let dir = state.profile.session_path(&session_id);
        let has_provider = {
            let settings =
                tauri_app_core::settings::Settings::load_or_default(&state.profile.settings_path());
            settings.default_summary_provider.is_some() && vault.is_unlocked()
        };
        let needs = tauri_app_core::commands::integrity::audit_session(&dir, has_provider);
        let mut done = 0u32;
        for kind in needs {
            match tauri_app_core::commands::integrity::repair_one(&state, &vault, &session_id, kind)
            {
                Ok(()) => done += 1,
                Err(e) => log::warn!("repair_session {session_id} {kind:?}: {e}"),
            }
        }
        if done > 0 {
            tauri_app_core::library_events::emit(
                &app_handle,
                tauri_app_core::library_events::LibraryChangeKind::Updated,
                &session_id,
            );
        }
        Ok(done)
    })
    .await
    .map_err(AppError::Provider)?;
    result
}

// Resumes a finalize cascade that paused at the speaker-label gate: runs the
// summary tail. Emits progress on the same channel as the initial cascade.
#[tauri::command]
async fn recording_resume_finalize(
    req: tauri_app_core::commands::finalize::FinalizeRequest,
    app_handle: tauri::AppHandle,
) -> Result<tauri_app_core::commands::finalize::FinalizeOutcome, AppError> {
    ensure_licensed()?;
    let sid = req.session_id.clone();
    let sid_for_emit = sid.clone();
    let emit_handle = app_handle.clone();
    let lib_emit_handle = app_handle.clone();
    let result = run_on_os_thread(move || {
        let state = app_handle.state::<AppState>();
        let vault = app_handle.state::<VaultState>();
        tauri_app_core::commands::finalize::resume_finalize_impl(&state, &vault, req, |ev| {
            let _ = emit_handle.emit(&format!("daisy://session/{sid}/progress"), &ev);
        })
    })
    .await
    .map_err(AppError::Provider)?;
    if let Ok(tauri_app_core::commands::finalize::FinalizeOutcome::Completed { .. }) = &result {
        tauri_app_core::library_events::emit(
            &lib_emit_handle,
            tauri_app_core::library_events::LibraryChangeKind::Finalized,
            &sid_for_emit,
        );
    }
    if result.is_err() {
        tauri_app_core::library_events::emit(
            &lib_emit_handle,
            tauri_app_core::library_events::LibraryChangeKind::Updated,
            &sid_for_emit,
        );
        if let Ok(snap) = tauri_app_core::commands::workflows::snapshot_for_session(
            &lib_emit_handle.state::<AppState>(),
            &sid_for_emit,
        ) {
            workflow_dispatch_with_snapshot(
                &lib_emit_handle,
                tauri_app_core::commands::workflows::TriggerEvent::FinalizeFailed,
                snap,
            );
        }
    }
    result
}

// ---- prompts ----
// Prompts live in <profile>/prompts.json (plaintext).
#[tauri::command]
fn list_prompts(
    state: State<'_, AppState>,
) -> Result<Vec<summarize::prompts::Prompt>, AppError> {
    tauri_app_core::commands::prompts::list_prompts_impl(&state)
}
#[tauri::command]
fn save_prompt(
    req: tauri_app_core::commands::prompts::SavePromptRequest,
    state: State<'_, AppState>,
) -> Result<summarize::prompts::Prompt, AppError> {
    tauri_app_core::commands::prompts::save_prompt_impl(&state, req)
}
#[tauri::command]
fn delete_prompt(id: String, state: State<'_, AppState>) -> Result<(), AppError> {
    tauri_app_core::commands::prompts::delete_prompt_impl(&state, &id)
}
#[tauri::command]
fn reset_prompt(
    id: String,
    state: State<'_, AppState>,
) -> Result<summarize::prompts::Prompt, AppError> {
    tauri_app_core::commands::prompts::reset_prompt_impl(&state, &id)
}
#[tauri::command]
fn set_default_summary_prompt(id: String, state: State<'_, AppState>) -> Result<(), AppError> {
    let sp = state.profile.settings_path();
    let mut s = tauri_app_core::settings::Settings::load_or_default(&sp);
    s.default_summary_prompt_id = Some(id);
    s.save(&sp)
        .map_err(|e| AppError::Config(format!("save settings: {e}")))?;
    Ok(())
}

// ---- contacts ----
// Contacts live in <profile>/contacts.json (plaintext).
#[tauri::command]
fn list_contacts(
    state: State<'_, AppState>,
) -> Result<Vec<tauri_app_core::commands::contacts::Contact>, AppError> {
    tauri_app_core::commands::contacts::list_contacts_impl(&state)
}

// ---- tags ----
// Tags live in <profile>/tags.json (plaintext).
#[tauri::command]
fn list_tags(state: State<'_, AppState>) -> Result<Vec<Tag>, AppError> {
    list_tags_impl(&state)
}
#[tauri::command]
fn create_tag(req: CreateTagRequest, state: State<'_, AppState>) -> Result<Tag, AppError> {
    create_tag_impl(&state, req)
}
#[tauri::command]
fn update_tag(req: UpdateTagRequest, state: State<'_, AppState>) -> Result<Tag, AppError> {
    update_tag_impl(&state, req)
}
#[tauri::command]
fn delete_tag(
    id: String,
    force: bool,
    state: State<'_, AppState>,
) -> Result<DeleteTagResult, AppError> {
    delete_tag_impl(&state, &id, force)
}
#[tauri::command]
fn search_tags(query: String, state: State<'_, AppState>) -> Result<Vec<Tag>, AppError> {
    search_tags_impl(&state, &query)
}

// ---- voiceprints (cross-session speaker recognition) ----
#[tauri::command]
fn list_voiceprints(
    state: State<'_, AppState>,
    vault: State<'_, VaultState>,
) -> Result<Vec<tauri_app_core::commands::voiceprints::VoiceprintView>, AppError> {
    tauri_app_core::commands::voiceprints::list_voiceprints_impl(&state, &vault)
}

#[tauri::command]
fn rename_voiceprint(
    req: tauri_app_core::commands::voiceprints::VoiceprintRenameRequest,
    state: State<'_, AppState>,
    vault: State<'_, VaultState>,
) -> Result<(), AppError> {
    tauri_app_core::commands::voiceprints::rename_voiceprint_impl(&state, &vault, req)
}

#[tauri::command]
fn delete_voiceprint(
    id: String,
    state: State<'_, AppState>,
    vault: State<'_, VaultState>,
) -> Result<(), AppError> {
    tauri_app_core::commands::voiceprints::delete_voiceprint_impl(&state, &vault, &id)
}

#[tauri::command]
fn detach_speaker_voiceprint(
    session_id: String,
    cluster_id: u32,
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), AppError> {
    tauri_app_core::commands::voiceprints::detach_speaker_voiceprint_impl(
        &state,
        &session_id,
        cluster_id,
    )?;
    let _ = tauri_app_core::commands::session::rerender_session_transcript_impl(&state, &session_id);
    tauri_app_core::library_events::emit(
        &app_handle,
        tauri_app_core::library_events::LibraryChangeKind::Updated,
        &session_id,
    );
    Ok(())
}

#[tauri::command]
async fn enroll_voiceprint_from_speaker(
    req: tauri_app_core::commands::voiceprints::EnrollFromSpeakerRequest,
    app_handle: tauri::AppHandle,
) -> Result<tauri_app_core::commands::voiceprints::EnrollResult, AppError> {
    let sid_for_emit = req.session_id.clone();
    let sid_for_render = req.session_id.clone();
    let emit_handle = app_handle.clone();
    let res = tauri::async_runtime::spawn_blocking(move || {
        let state = app_handle.state::<AppState>();
        let vault = app_handle.state::<VaultState>();
        let result = tauri_app_core::commands::voiceprints::enroll_voiceprint_from_speaker_impl(
            &state, &vault, req,
        )?;
        // Stamping a voiceprint also sets the cluster's display name;
        // transcript.md is re-rendered.
        let _ = tauri_app_core::commands::session::rerender_session_transcript_impl(
            &state, &sid_for_render,
        );
        Ok::<_, AppError>(result)
    })
    .await
    .map_err(|e| AppError::Config(format!("enroll task failed: {e}")))?;
    if res.is_ok() {
        tauri_app_core::library_events::emit(
            &emit_handle,
            tauri_app_core::library_events::LibraryChangeKind::Updated,
            &sid_for_emit,
        );
    }
    res
}

#[derive(Debug, serde::Serialize)]
struct RematchAllResult {
    sessions_scanned: u32,
    clusters_matched: u32,
}

// ---- calendar subscriptions + ICS refresh ----
#[tauri::command]
fn list_calendar_subscriptions(
    state: State<'_, AppState>,
    vault: State<'_, VaultState>,
) -> Result<Vec<tauri_app_core::state::CalendarSubscription>, AppError> {
    tauri_app_core::commands::calendar::list_calendar_subscriptions_impl(&state, &vault)
}

#[tauri::command]
fn add_calendar_subscription(
    req: tauri_app_core::commands::calendar::AddSubscriptionRequest,
    state: State<'_, AppState>,
    vault: State<'_, VaultState>,
) -> Result<tauri_app_core::state::CalendarSubscription, AppError> {
    tauri_app_core::commands::calendar::add_calendar_subscription_impl(&state, &vault, req)
}

#[tauri::command]
fn update_calendar_subscription(
    req: tauri_app_core::commands::calendar::UpdateSubscriptionRequest,
    state: State<'_, AppState>,
    vault: State<'_, VaultState>,
) -> Result<tauri_app_core::state::CalendarSubscription, AppError> {
    tauri_app_core::commands::calendar::update_calendar_subscription_impl(&state, &vault, req)
}

#[tauri::command]
fn delete_calendar_subscription(
    id: String,
    state: State<'_, AppState>,
    vault: State<'_, VaultState>,
) -> Result<(), AppError> {
    tauri_app_core::commands::calendar::delete_calendar_subscription_impl(&state, &vault, &id)
}

#[tauri::command]
async fn refresh_calendars(
    app_handle: tauri::AppHandle,
) -> Result<tauri_app_core::commands::calendar::RefreshResult, AppError> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app_handle.state::<AppState>();
        let vault = app_handle.state::<VaultState>();
        tauri_app_core::commands::calendar::refresh_calendars_impl(&state, &vault)
    })
    .await
    .map_err(|e| AppError::Config(format!("refresh task failed: {e}")))?
}

#[tauri::command]
fn list_upcoming_events(
    req: tauri_app_core::commands::calendar::UpcomingRequest,
    state: State<'_, AppState>,
    vault: State<'_, VaultState>,
) -> Result<Vec<tauri_app_core::commands::calendar::CalendarEvent>, AppError> {
    tauri_app_core::commands::calendar::list_upcoming_events_impl(&state, &vault, req)
}

#[tauri::command]
fn dismiss_calendar_event(
    subscription_id: String,
    uid: String,
    state: State<'_, AppState>,
    vault: State<'_, VaultState>,
) -> Result<(), AppError> {
    tauri_app_core::commands::calendar::dismiss_calendar_event_impl(
        &state,
        &vault,
        &subscription_id,
        &uid,
    )
}

// ---- topic chapters ----
#[tauri::command]
async fn extract_session_chapters(
    req: tauri_app_core::commands::chapters::ChaptersRequest,
    app_handle: tauri::AppHandle,
) -> Result<tauri_app_core::commands::chapters::ChaptersResult, AppError> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app_handle.state::<AppState>();
        let vault = app_handle.state::<VaultState>();
        tauri_app_core::commands::chapters::extract_chapters_impl(&state, &vault, req)
    })
    .await
    .map_err(|e| AppError::Config(format!("chapters task failed: {e}")))?
}

#[tauri::command]
fn load_session_chapters(
    session_id: String,
    state: State<'_, AppState>,
) -> Result<Option<summarize::chapters::SessionChapters>, AppError> {
    tauri_app_core::commands::chapters::load_session_chapters_impl(&state, &session_id)
}

#[tauri::command]
async fn rematch_all_sessions(
    app_handle: tauri::AppHandle,
) -> Result<RematchAllResult, AppError> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app_handle.state::<AppState>();
        let vault = app_handle.state::<VaultState>();
        tauri_app_core::commands::voiceprints::rematch_all_sessions_impl(&state, &vault)
            .map(|(sessions_scanned, clusters_matched)| RematchAllResult {
                sessions_scanned,
                clusters_matched,
            })
    })
    .await
    .map_err(|e| AppError::Config(format!("rematch-all task failed: {e}")))?
}

// Retries a finalize that was given up (the `finalize.recovery.json` cap):
// clears the attempt counter, then re-kicks the in-app finalize on a
// background thread. The command returns immediately.
#[tauri::command]
async fn retry_finalize(
    session_id: String,
    app_handle: tauri::AppHandle,
) -> Result<(), AppError> {
    let dir = {
        let state = app_handle.state::<AppState>();
        state.profile.session_path(&session_id)
    };
    tauri_app_core::commands::finalize::clear_finalize_recovery(&dir);
    std::thread::spawn(move || {
        finalize_orphan_in_app(&app_handle, &session_id);
    });
    Ok(())
}

// Reads the finalize-recovery sidecar (attempt count, terminal-failed flag +
// friendly reason).
#[tauri::command]
async fn finalize_recovery(
    session_id: String,
    app_handle: tauri::AppHandle,
) -> Result<tauri_app_core::commands::finalize::FinalizeRecovery, AppError> {
    let state = app_handle.state::<AppState>();
    let dir = state.profile.session_path(&session_id);
    Ok(tauri_app_core::commands::finalize::read_finalize_recovery(&dir))
}

// Provider-agnostic local diarization: cluster the system track by voice and
// assign Person A/B/C, then name clusters via voiceprint matching and
// re-render. Works on any transcript.
#[tauri::command]
async fn diarize_session(
    session_id: String,
    expected_speakers: Option<u32>,
    track: Option<tauri_app_core::commands::voiceprints::DiarizeScope>,
    app_handle: tauri::AppHandle,
) -> Result<tauri_app_core::commands::voiceprints::DiarizeResult, AppError> {
    ensure_licensed()?;
    let sid_for_emit = session_id.clone();
    let emit_handle = app_handle.clone();
    let res = tauri::async_runtime::spawn_blocking(move || {
        let state = app_handle.state::<AppState>();
        let vault = app_handle.state::<VaultState>();
        let result =
            tauri_app_core::commands::voiceprints::diarize_session_impl(&state, &session_id, expected_speakers, track)?;
        // Best-effort: names the fresh clusters from enrolled voiceprints,
        // then re-renders transcript.md.
        let _ = tauri_app_core::commands::voiceprints::rematch_session_speakers_impl(
            &state, &vault, &session_id,
        );
        let _ = tauri_app_core::commands::session::rerender_session_transcript_impl(
            &state, &session_id,
        );
        Ok::<_, AppError>(result)
    })
    .await
    .map_err(|e| AppError::Config(format!("diarize task failed: {e}")))?;
    if res.is_ok() {
        tauri_app_core::library_events::emit(
            &emit_handle,
            tauri_app_core::library_events::LibraryChangeKind::Updated,
            &sid_for_emit,
        );
    }
    res
}

// ---- corpus-wide Q&A (RAG over all transcripts) ----
#[tauri::command]
async fn qa_ask(
    req: tauri_app_core::commands::qa::QaAskRequest,
    app_handle: tauri::AppHandle,
) -> Result<tauri_app_core::commands::qa::QaAnswer, AppError> {
    ensure_licensed()?;
    tauri::async_runtime::spawn_blocking(move || {
        let state = app_handle.state::<AppState>();
        let vault = app_handle.state::<VaultState>();
        tauri_app_core::commands::qa::qa_ask_impl(&state, &vault, req)
    })
    .await
    .map_err(|e| AppError::Config(format!("qa task failed: {e}")))?
}

/// Streaming Q&A: pushes each answer token through `on_token`, returns the full
/// answer + citations when done. Non-streaming providers send the whole answer
/// as one token.
#[tauri::command]
async fn qa_ask_stream(
    req: tauri_app_core::commands::qa::QaAskRequest,
    on_token: tauri::ipc::Channel<String>,
    app_handle: tauri::AppHandle,
) -> Result<tauri_app_core::commands::qa::QaAnswer, AppError> {
    ensure_licensed()?;
    let state = app_handle.state::<AppState>();
    let vault = app_handle.state::<VaultState>();
    tauri_app_core::commands::qa::qa_ask_streaming_impl(
        &state,
        &vault,
        req,
        |t| {
            let _ = on_token.send(t.to_string());
        },
    )
    .await
}

// ---- in-call chat (per-session, this-meeting-only conversation) ----
#[tauri::command]
async fn live_chat_send(
    req: tauri_app_core::commands::live_chat::LiveChatSendRequest,
    app_handle: tauri::AppHandle,
) -> Result<tauri_app_core::commands::live_chat::LiveChatReply, AppError> {
    ensure_licensed()?;
    tauri::async_runtime::spawn_blocking(move || {
        let state = app_handle.state::<AppState>();
        let vault = app_handle.state::<VaultState>();
        tauri_app_core::commands::live_chat::live_chat_send_impl(&state, &vault, req)
    })
    .await
    .map_err(|e| AppError::Config(format!("chat task failed: {e}")))?
}

/// Streaming in-call chat: pushes each reply token through `on_token` as it
/// arrives, then returns the persisted thread. Providers without SSE support
/// send the whole reply as one token.
#[tauri::command]
async fn live_chat_send_stream(
    req: tauri_app_core::commands::live_chat::LiveChatSendRequest,
    on_token: tauri::ipc::Channel<String>,
    app_handle: tauri::AppHandle,
) -> Result<tauri_app_core::commands::live_chat::LiveChatReply, AppError> {
    ensure_licensed()?;
    let state = app_handle.state::<AppState>();
    let vault = app_handle.state::<VaultState>();
    tauri_app_core::commands::live_chat::live_chat_send_streaming_impl(
        &state,
        &vault,
        req,
        |t| {
            let _ = on_token.send(t.to_string());
        },
    )
    .await
}

#[tauri::command]
fn live_chat_load(
    session_id: String,
    app: State<'_, AppState>,
) -> Result<tauri_app_core::commands::live_chat::CallChat, AppError> {
    tauri_app_core::commands::live_chat::live_chat_load_impl(&app, &session_id)
}

#[tauri::command]
fn live_chat_delete(
    session_id: String,
    app: State<'_, AppState>,
) -> Result<(), AppError> {
    tauri_app_core::commands::live_chat::live_chat_delete_impl(&app, &session_id)
}

// ---- analysis (run any prompt over a session) ----
#[tauri::command]
async fn run_analysis(
    req: tauri_app_core::commands::analysis::RunAnalysisRequest,
    app_handle: tauri::AppHandle,
) -> Result<tauri_app_core::commands::analysis::AnalysisResult, AppError> {
    ensure_licensed()?;
    tauri::async_runtime::spawn_blocking(move || {
        let state = app_handle.state::<AppState>();
        let vault = app_handle.state::<VaultState>();
        tauri_app_core::commands::analysis::run_analysis_impl(&state, &vault, req)
    })
    .await
    .map_err(|e| AppError::Config(format!("analysis task failed: {e}")))?
}
#[tauri::command]
fn analysis_load(
    session_id: String,
    prompt_id: String,
    state: State<'_, AppState>,
) -> Result<Option<tauri_app_core::commands::analysis::AnalysisResult>, AppError> {
    tauri_app_core::commands::analysis::analysis_load_impl(&state, &session_id, &prompt_id)
}

// ---- session metadata + notes ----
#[tauri::command]
fn session_meta_get(session_id: String, state: State<'_, AppState>) -> Result<SessionMeta, AppError> {
    session_meta_get_impl(&state, &session_id)
}
#[tauri::command]
fn session_meta_update(
    req: SessionMetaUpdate,
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), AppError> {
    let session_id = req.session_id.clone();
    session_meta_update_impl(&state, req)?;
    tauri_app_core::library_events::emit(
        &app_handle,
        tauri_app_core::library_events::LibraryChangeKind::Updated,
        &session_id,
    );
    Ok(())
}
#[tauri::command]
fn session_notes_load(session_id: String, state: State<'_, AppState>) -> Result<String, AppError> {
    session_notes_load_impl(&state, &session_id)
}
#[tauri::command]
fn session_notes_save(
    session_id: String,
    markdown: String,
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), AppError> {
    session_notes_save_impl(&state, &session_id, &markdown)?;
    tauri_app_core::library_events::emit(
        &app_handle,
        tauri_app_core::library_events::LibraryChangeKind::Updated,
        &session_id,
    );
    Ok(())
}

#[tauri::command]
fn create_note_session(
    req: tauri_app_core::commands::meeting::CreateNoteRequest,
    state: State<'_, AppState>,
) -> Result<String, AppError> {
    tauri_app_core::commands::meeting::create_note_session_impl(&state, req)
}
#[tauri::command]
async fn import_audio_meeting(
    req: tauri_app_core::commands::meeting::ImportAudioRequest,
    app_handle: tauri::AppHandle,
) -> Result<tauri_app_core::commands::meeting::ImportAudioResult, AppError> {
    ensure_licensed()?;
    let impl_handle = app_handle.clone();
    let result = tauri::async_runtime::spawn_blocking(move || {
        let state = impl_handle.state::<AppState>();
        tauri_app_core::commands::meeting::import_audio_meeting_impl(&state, req)
    })
    .await
    .map_err(|e| AppError::Config(format!("import task failed: {e}")))?;
    if let Ok(r) = &result {
        tauri_app_core::library_events::emit(
            &app_handle,
            tauri_app_core::library_events::LibraryChangeKind::Added,
            &r.session_id,
        );
        if let Ok(snap) = tauri_app_core::commands::workflows::snapshot_for_session(
            &app_handle.state::<AppState>(),
            &r.session_id,
        ) {
            workflow_dispatch_with_snapshot(
                &app_handle,
                tauri_app_core::commands::workflows::TriggerEvent::Imported,
                snap,
            );
        }
    }
    result
}
/// Returns the value of `DAISY_PROFILE_DIR` if set and non-empty, else
/// `None`. Used by the wizard to prefill the profile-dir field.
#[tauri::command]
fn env_profile_dir() -> Option<String> {
    std::env::var("DAISY_PROFILE_DIR").ok().filter(|s| !s.is_empty())
}

#[tauri::command]
async fn save_text_file(path: String, contents: String) -> Result<(), AppError> {
    tauri::async_runtime::spawn_blocking(move || {
        tauri_app_core::commands::files::save_text_file_impl(std::path::Path::new(&path), &contents)
    })
    .await
    .map_err(|e| AppError::Config(format!("save task failed: {e}")))?
}
#[tauri::command]
fn session_assign_tags(
    session_id: String,
    tag_ids: Vec<String>,
    state: State<'_, AppState>,
    vault: State<'_, VaultState>,
    app_handle: tauri::AppHandle,
) -> Result<(), AppError> {
    session_assign_tags_impl(&state, &vault, &session_id, tag_ids)?;
    tauri_app_core::library_events::emit(
        &app_handle,
        tauri_app_core::library_events::LibraryChangeKind::Updated,
        &session_id,
    );
    Ok(())
}

// ---- summaries ----
#[tauri::command]
fn summary_load(
    session_id: String,
    state: State<'_, AppState>,
) -> Result<Option<summarize::SessionSummary>, AppError> {
    summary_load_impl(&state, &session_id)
}
#[tauri::command]
fn summary_save_edit(
    session_id: String,
    markdown: String,
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), AppError> {
    summary_save_edit_impl(&state, &session_id, &markdown)?;
    tauri_app_core::library_events::emit(
        &app_handle,
        tauri_app_core::library_events::LibraryChangeKind::Updated,
        &session_id,
    );
    Ok(())
}
#[tauri::command]
async fn summary_regenerate(
    session_id: String,
    prompt_id: Option<String>,
    app_handle: tauri::AppHandle,
) -> Result<summarize::SessionSummary, AppError> {
    ensure_licensed()?;
    let sid_for_emit = session_id.clone();
    let emit_handle = app_handle.clone();
    let result: Result<summarize::SessionSummary, AppError> = run_on_os_thread(move || {
        summary_regenerate_impl(&app_handle.state::<AppState>(), &app_handle.state::<VaultState>(), &session_id, prompt_id)
    })
    .await
    .map_err(AppError::Provider)?;
    if result.is_ok() {
        tauri_app_core::library_events::emit(
            &emit_handle,
            tauri_app_core::library_events::LibraryChangeKind::Updated,
            &sid_for_emit,
        );
    }
    result
}
#[tauri::command]
fn summary_provider_status(
    state: State<'_, AppState>,
    vault: State<'_, VaultState>,
) -> SummaryProviderStatus {
    summary_provider_status_impl_from_state(&state, &vault)
}

// ---- search ----
#[tauri::command]
async fn search_sessions(
    req: SearchRequest,
    app_handle: tauri::AppHandle,
) -> Result<Vec<SessionHit>, AppError> {
    tauri::async_runtime::spawn_blocking(move || {
        search_sessions_impl(&app_handle.state::<AppState>(), req)
    })
    .await
    .map_err(|e| AppError::Config(format!("search task failed: {e}")))?
}

#[tauri::command]
fn bootstrap_status() -> Result<BootstrapStatus, AppError> {
    bootstrap_status_impl()
}

#[tauri::command]
fn bootstrap_set(profile_dir: PathBuf) -> Result<(), AppError> {
    bootstrap_set_impl(profile_dir)
}

// Opens an http(s) URL in the user's default browser. http/https only; the
// URL is passed as one direct arg, no shell.
#[tauri::command]
fn open_external(url: String) -> Result<(), AppError> {
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err(AppError::Config("only http(s) URLs may be opened".into()));
    }
    #[cfg(target_os = "windows")]
    let r = std::process::Command::new("rundll32")
        .args(["url.dll,FileProtocolHandler", &url])
        .spawn();
    #[cfg(target_os = "macos")]
    let r = std::process::Command::new("open").arg(&url).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let r = std::process::Command::new("xdg-open").arg(&url).spawn();
    r.map(|_| ()).map_err(|e| AppError::Config(format!("open browser: {e}")))
}

#[tauri::command]
async fn check_for_update() -> Result<tauri_app_core::commands::update::UpdateInfo, AppError> {
    tauri::async_runtime::spawn_blocking(tauri_app_core::commands::update::check_for_update_impl)
        .await
        .map_err(|e| AppError::Config(format!("update task failed: {e}")))?
}

/// Reveals the per-profile logs directory in the OS file browser. The path
/// is computed from app state, not user input, and passed as one literal arg.
#[tauri::command]
fn open_logs_dir(state: State<'_, AppState>) -> Result<String, AppError> {
    let dir = state.profile.logs_dir();
    syncsafe::create_dir_all(&dir)
        .map_err(|e| AppError::Config(format!("create logs dir: {e}")))?;
    let s = dir.to_string_lossy().to_string();
    #[cfg(target_os = "windows")]
    let r = std::process::Command::new("explorer").arg(&dir).spawn();
    #[cfg(target_os = "macos")]
    let r = std::process::Command::new("open").arg(&dir).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let r = std::process::Command::new("xdg-open").arg(&dir).spawn();
    r.map(|_| s.clone())
        .map_err(|e| AppError::Config(format!("open logs dir: {e}")))
}

/// Reveals the profile directory in the OS file browser. Same shape as
/// `open_logs_dir`: the path comes from app state, one literal arg.
#[tauri::command]
fn open_profile_dir(state: State<'_, AppState>) -> Result<String, AppError> {
    let dir = state.profile.root().to_path_buf();
    let s = dir.to_string_lossy().to_string();
    #[cfg(target_os = "windows")]
    let r = std::process::Command::new("explorer").arg(&dir).spawn();
    #[cfg(target_os = "macos")]
    let r = std::process::Command::new("open").arg(&dir).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let r = std::process::Command::new("xdg-open").arg(&dir).spawn();
    r.map(|_| s.clone())
        .map_err(|e| AppError::Config(format!("open profile dir: {e}")))
}

#[tauri::command]
fn profile_binding_check(state: State<'_, AppState>) -> Result<tauri_app_core::commands::binding::BindingState, AppError> {
    tauri_app_core::commands::binding::profile_binding_check_impl(&state)
}

#[tauri::command]
fn license_status() -> Result<tauri_app_core::commands::license::LicenseStatus, AppError> {
    tauri_app_core::commands::license::license_status_impl()
}
#[tauri::command]
async fn activate_license(
    key: String,
    state: State<'_, AppState>,
    vault: State<'_, VaultState>,
) -> Result<tauri_app_core::commands::license::LicenseStatus, AppError> {
    // The install's Ed25519 public key is derived from the vault seed and
    // registered at activation. None when the vault is locked; registration
    // then happens when Daisy Cloud is enabled.
    let install_pubkey =
        tauri_app_core::commands::lifecycle::derive_install_pubkey(&state, &vault);
    let status = tauri::async_runtime::spawn_blocking(move || {
        tauri_app_core::commands::license::activate_license_impl(key, install_pubkey)
    })
    .await
    .map_err(|e| AppError::Config(format!("activate task failed: {e}")))??;
    pin_profile_name_to_license(&state);
    Ok(status)
}

/// License check-in (heartbeat). Called from the frontend's 6h update
/// beacon; throttled to about once per day.
#[tauri::command]
async fn license_checkin() -> Result<tauri_app_core::commands::license::LicenseStatus, AppError> {
    tauri::async_runtime::spawn_blocking(
        tauri_app_core::commands::license::checkin_if_needed_impl,
    )
    .await
    .map_err(|e| AppError::Config(format!("license check-in task failed: {e}")))?
}

/// Sets the profile display name to the licensee's first name whenever a
/// valid license is present. The name is a signed claim in the license
/// token. Idempotent + best-effort; runs on activation and at startup.
fn pin_profile_name_to_license(state: &AppState) {
    use tauri_app_core::commands::license::{license_status_impl, LicenseStatus};
    let Ok(LicenseStatus::Licensed { name, .. }) = license_status_impl() else { return };
    let Some(first) = name.split_whitespace().next() else { return };
    let path = state.profile.settings_path();
    let cur = tauri_app_core::settings::Settings::load_or_default(&path);
    if cur.user_display_name.as_deref() != Some(first) {
        let next = tauri_app_core::settings::Settings {
            user_display_name: Some(first.to_string()),
            ..cur
        };
        let _ = next.save(&path);
    }
}
#[tauri::command]
async fn deactivate_license() -> Result<tauri_app_core::commands::license::LicenseStatus, AppError> {
    tauri::async_runtime::spawn_blocking(
        tauri_app_core::commands::license::deactivate_license_impl,
    )
    .await
    .map_err(|e| AppError::Config(format!("deactivate task failed: {e}")))?
}
#[tauri::command]
fn consent_status() -> Result<bool, AppError> {
    Ok(tauri_app_core::bootstrap::Consent::is_accepted())
}

/// App version (the tauri-app crate version) + the git short SHA this binary
/// was built from, and whether it came from a tagged release. Shown in About;
/// untagged builds render the testing-only banner.
#[derive(serde::Serialize)]
struct BuildInfo {
    version: &'static str,
    sha: &'static str,
    tagged: bool,
}

#[tauri::command]
fn build_info() -> BuildInfo {
    BuildInfo {
        version: env!("CARGO_PKG_VERSION"),
        sha: env!("DAISY_BUILD_SHA"),
        tagged: env!("DAISY_BUILD_TAGGED") == "1",
    }
}

/// Microphone capture permission status (macOS TCC):
/// 0 = not determined, 1 = granted, 2 = denied. Non-macOS always returns 1.
#[tauri::command]
fn capture_permission_status() -> i32 {
    audio_engine::capture_permission_status()
}

/// This machine's live-captions resolution + stored choice.
#[derive(serde::Serialize)]
struct LiveCaptionsStatus {
    #[serde(flatten)]
    resolution: tauri_app_core::hardware::LiveCaptionsResolution,
    /// "auto" | "on" | "off" — this machine's stored preference.
    choice: tauri_app_core::settings::LiveCaptionsChoice,
}

fn live_captions_status_from(settings: &tauri_app_core::settings::Settings) -> LiveCaptionsStatus {
    let resolution = tauri_app_core::hardware::resolve_live_captions(settings);
    let choice = settings
        .live_captions_by_machine
        .get(&resolution.machine)
        .map(|e| e.choice)
        .unwrap_or_default();
    LiveCaptionsStatus { resolution, choice }
}

#[tauri::command]
fn live_captions_status(state: State<'_, AppState>) -> LiveCaptionsStatus {
    let settings =
        tauri_app_core::settings::Settings::load_or_default(&state.profile.settings_path());
    live_captions_status_from(&settings)
}

/// Stores this machine's live-captions preference ("auto" | "on" | "off").
#[tauri::command]
fn set_live_captions_choice(
    choice: tauri_app_core::settings::LiveCaptionsChoice,
    state: State<'_, AppState>,
) -> Result<LiveCaptionsStatus, AppError> {
    let path = state.profile.settings_path();
    let mut settings = tauri_app_core::settings::Settings::load_or_default(&path);
    settings
        .live_captions_by_machine
        .entry(tauri_app_core::hardware::machine_name())
        .or_default()
        .choice = choice;
    settings
        .save(&path)
        .map_err(|e| AppError::Config(format!("save settings: {e}")))?;
    Ok(live_captions_status_from(&settings))
}

/// Runs the whisper speed benchmark and stores the verdict for this machine.
#[tauri::command]
async fn run_live_captions_bench(
    app_handle: tauri::AppHandle,
) -> Result<LiveCaptionsStatus, AppError> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app_handle.state::<AppState>();
        let resolution =
            tauri_app_core::commands::bench::run_live_captions_bench_impl(&state)?;
        let settings =
            tauri_app_core::settings::Settings::load_or_default(&state.profile.settings_path());
        let choice = settings
            .live_captions_by_machine
            .get(&resolution.machine)
            .map(|e| e.choice)
            .unwrap_or_default();
        Ok(LiveCaptionsStatus { resolution, choice })
    })
    .await
    .map_err(|e| AppError::Config(format!("benchmark task failed: {e}")))?
}

#[tauri::command]
fn accept_consent() -> Result<(), AppError> {
    tauri_app_core::bootstrap::Consent::accept().map_err(|e| AppError::Io(e.to_string()))
}

#[tauri::command]
fn eula_status() -> Result<bool, AppError> {
    Ok(tauri_app_core::bootstrap::Eula::is_accepted())
}

#[tauri::command]
fn accept_eula() -> Result<(), AppError> {
    tauri_app_core::bootstrap::Eula::accept().map_err(|e| AppError::Io(e.to_string()))
}

#[tauri::command]
fn vault_status(state: State<'_, AppState>, vault: State<'_, VaultState>) -> Result<VaultStatus, AppError> {
    vault_status_impl(&state, &vault)
}

// init_vault_impl's Argon2 KDF blocks briefly on the tokio executor.
#[tauri::command]
async fn init_vault(
    passphrase: String,
    state: State<'_, AppState>,
    vault: State<'_, VaultState>,
) -> Result<(), AppError> {
    init_vault_impl(&state, &vault, &passphrase)?;
    if let Err(e) = tauri_app_core::migrate_v3::v3_cutover(&state, &vault) {
        log::warn!("v3 migration: {e}");
    }
    Ok(())
}

#[tauri::command]
async fn unlock_vault(
    passphrase: String,
    app_handle: tauri::AppHandle,
    state: State<'_, AppState>,
    vault: State<'_, VaultState>,
) -> Result<(), AppError> {
    unlock_vault_impl(&state, &vault, &passphrase)?;
    if let Err(e) = tauri_app_core::migrate_v3::v3_cutover(&state, &vault) {
        log::warn!("v3 migration: {e}");
    }
    // The MCP token lives in the vault; the server binds after unlock.
    // No-op if MCP is disabled or already running.
    if let Err(e) = tauri_app_core::mcp::server::apply_mcp_state(&app_handle) {
        log::warn!("mcp: post-unlock start: {e:?}");
    }
    Ok(())
}

#[tauri::command]
async fn init_vault_machine_mode(
    state: State<'_, AppState>,
    vault: State<'_, VaultState>,
) -> Result<(), AppError> {
    init_vault_machine_mode_impl(&state, &vault)
}

/// Returns the vault kind: "passphrase" or "machine" (or "passphrase" when
/// missing). Frontend uses this to know whether to show the Unlock screen.
#[tauri::command]
fn vault_kind(state: State<'_, AppState>) -> Result<String, AppError> {
    Ok(read_vault_kind(&state))
}

#[tauri::command]
async fn change_vault_passphrase(
    old_passphrase: String,
    new_passphrase: String,
    state: State<'_, AppState>,
    vault: State<'_, VaultState>,
) -> Result<(), AppError> {
    change_vault_passphrase_impl(&state, &vault, &old_passphrase, &new_passphrase)
}

/// Switch the vault between passphrase- and machine-mode in place (no data
/// loss). Pass a non-null `new_passphrase` to switch to passphrase-mode;
/// pass null to switch to machine-mode. Vault must be unlocked. Returns the
/// new kind.
#[tauri::command]
async fn switch_vault_mode(
    new_passphrase: Option<String>,
    state: State<'_, AppState>,
    vault: State<'_, VaultState>,
) -> Result<String, AppError> {
    switch_vault_mode_impl(&state, &vault, new_passphrase.as_deref())
}

#[tauri::command]
fn read_settings(state: State<'_, AppState>) -> Result<Settings, AppError> {
    read_settings_impl(&state)
}

#[tauri::command]
fn write_settings(settings: Settings, state: State<'_, AppState>) -> Result<(), AppError> {
    write_settings_impl(&state, settings)
}

#[tauri::command]
async fn list_audio_sources() -> Result<Vec<AudioSourceInfo>, AppError> {
    tauri::async_runtime::spawn_blocking(list_audio_sources_impl)
        .await
        .map_err(|e| AppError::Config(format!("audio enum task failed: {e}")))?
}

// ---------------------------------------------------------------------------
// Mic level meter — backend-driven, using the same PipeWire/WASAPI capture
// path as recording. Frontend subscribes to a per-request `mic-level:<id>`
// event channel.
// ---------------------------------------------------------------------------

fn mic_meter_registry() -> &'static Mutex<HashMap<String, std::sync::mpsc::Sender<()>>> {
    static R: OnceLock<Mutex<HashMap<String, std::sync::mpsc::Sender<()>>>> = OnceLock::new();
    R.get_or_init(|| Mutex::new(HashMap::new()))
}

#[derive(serde::Serialize, Clone)]
struct MicLevelPayload {
    request_id: String,
    /// Normalised RMS (0.0–1.0, where 1.0 ≈ full-scale int16).
    rms: f32,
}

#[tauri::command]
async fn start_mic_meter(
    request_id: String,
    source_id: u32,
    app_handle: tauri::AppHandle,
) -> Result<(), AppError> {
    use tauri::Emitter;
    log::info!("mic meter: start requested (source_id={source_id}, request_id={request_id})");
    let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();
    {
        let mut reg = mic_meter_registry().lock().unwrap();
        // A previous meter alive for this id is signalled to stop; the new
        // one takes over the channel.
        if let Some(old) = reg.remove(&request_id) {
            let _ = old.send(());
        }
        reg.insert(request_id.clone(), stop_tx);
    }
    let event_name = format!("mic-level:{request_id}");
    let app = app_handle.clone();
    let rid = request_id.clone();
    // The PipeWire MainLoop is blocking; it runs on a dedicated OS thread.
    std::thread::spawn(move || {
        let app_for_cb = app.clone();
        let event_for_cb = event_name.clone();
        let rid_for_cb = rid.clone();
        let on_rms = move |rms: f32| {
            let payload = MicLevelPayload {
                request_id: rid_for_cb.clone(),
                rms,
            };
            // Best-effort: with the window gone, the emit fails harmlessly.
            let _ = app_for_cb.emit(&event_for_cb, payload);
        };
        match audio_engine::capture::run_level_meter(source_id, on_rms, stop_rx) {
            Ok(()) => log::info!("mic meter: stopped (source_id={source_id})"),
            Err(e) => {
                log::warn!("mic meter: source {source_id} FAILED to start: {e}");
                // A start failure is surfaced as a `mic-meter-error:<id>`
                // event; the frontend listens and retries. Best-effort.
                let _ = app.emit(&format!("mic-meter-error:{rid}"), rid.clone());
            }
        }
        // The registry entry is removed once the loop exits.
        let mut reg = mic_meter_registry().lock().unwrap();
        reg.remove(&rid);
    });
    Ok(())
}

// ---------------------------------------------------------------------------
// Speech-level calibration + per-device store commands. Calibration records
// a short clip through the same capture path as recording, measures the
// voiced peak level, and stores it for the active device.
// ---------------------------------------------------------------------------

#[tauri::command]
async fn calibrate_speech_level(
    source_id: u32,
    device: String,
    state: State<'_, AppState>,
) -> Result<(), AppError> {
    const CALIBRATE_SECS: u64 = 8;
    let profile_root = state.profile.root().to_path_buf();
    let wav_path = profile_root.join(".calibrate.tmp.wav");
    let capture_path = wav_path.clone();
    let captured = tauri::async_runtime::spawn_blocking(move || {
        let sources = audio_engine::source::list_sources()
            .map_err(|e| AppError::Recording(format!("list sources: {e}")))?;
        let source = sources
            .into_iter()
            .find(|s| s.id == source_id)
            .ok_or_else(|| AppError::Recording(format!("mic source {source_id} not found")))?;
        audio_engine::capture::capture_one(
            &source,
            std::time::Duration::from_secs(CALIBRATE_SECS),
            &capture_path,
        )
        .map_err(|e| AppError::Recording(format!("calibration capture: {e}")))
    })
    .await
    .map_err(|e| AppError::Config(format!("calibration task failed: {e}")))?;
    // The clip is the user's voice: it must not outlive this function on any
    // path, success or failure.
    let loaded = captured.and_then(|_| {
        transcript::rms::WavSamples::load(&wav_path)
            .map_err(|e| AppError::Config(format!("read calibration clip: {e}")))
    });
    let _ = syncsafe::remove_file(&wav_path);
    let wav = loaded?;
    let Some(speech_dbfs) = transcript::energy_gate::calibration_speech_dbfs(&wav) else {
        return Err(AppError::Config(
            "Calibration didn't hear enough speech — keep talking while it listens.".into(),
        ));
    };

    let mut store = recording::speech_levels::SpeechLevels::load(&profile_root);
    store.record(&device, recording::speech_levels::LevelSample {
        at_unix: tauri_app_core::now_unix(),
        session_id: None,
        source: recording::speech_levels::LevelSource::Calibration,
        speech_dbfs,
        residue_dbfs: None,
    });
    store
        .save(&profile_root)
        .map_err(|e| AppError::Config(format!("save speech levels: {e}")))?;
    log::info!("speech-level calibration: {speech_dbfs:.1} dBFS for '{device}'");
    Ok(())
}

#[tauri::command]
async fn speech_level_set_override(
    device: String,
    dbfs: Option<f32>,
    state: State<'_, AppState>,
) -> Result<(), AppError> {
    let root = state.profile.root();
    let mut store = recording::speech_levels::SpeechLevels::load(root);
    store.set_override(&device, dbfs.map(|v| v.clamp(-60.0, 0.0)));
    store
        .save(root)
        .map_err(|e| AppError::Config(format!("save speech levels: {e}")))
}

#[derive(serde::Serialize)]
struct SpeechLevelInfo {
    device: String,
    effective_dbfs: Option<f32>,
    source: Option<recording::speech_levels::EffectiveSource>,
    samples: usize,
    override_dbfs: Option<f32>,
}

#[tauri::command]
async fn speech_levels_list(state: State<'_, AppState>) -> Result<Vec<SpeechLevelInfo>, AppError> {
    let store = recording::speech_levels::SpeechLevels::load(state.profile.root());
    Ok(store
        .devices
        .iter()
        .map(|(dev, d)| {
            let eff = store.effective(dev);
            SpeechLevelInfo {
                device: dev.clone(),
                effective_dbfs: eff.as_ref().map(|e| e.speech_dbfs),
                source: eff.map(|e| e.source),
                samples: d.history.len(),
                override_dbfs: d.override_dbfs,
            }
        })
        .collect())
}

#[tauri::command]
async fn stop_mic_meter(request_id: String) -> Result<(), AppError> {
    log::info!("mic meter: stop requested (request_id={request_id})");
    let stop = {
        let mut reg = mic_meter_registry().lock().unwrap();
        reg.remove(&request_id)
    };
    if let Some(stop) = stop {
        let _ = stop.send(());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Whisper model download (per-request-id progress events + cancel)
// ---------------------------------------------------------------------------

use std::collections::HashMap;
use std::sync::OnceLock;


#[tauri::command]
fn list_providers(vault: State<'_, VaultState>) -> Result<Vec<ProviderListEntry>, AppError> {
    list_providers_impl(&vault)
}

/// Queries a provider for its available model IDs (also acts as an API-key
/// test — a bad key yields an error). When `api_key`/`base_url` are absent
/// or empty, falls back to the values already stored in the unlocked vault.
#[tauri::command]
async fn list_provider_models(
    provider: tauri_app_core::state::ProviderId,
    api_key: Option<String>,
    base_url: Option<String>,
    vault: State<'_, VaultState>,
) -> Result<Vec<String>, AppError> {
    let vault_cfg: Option<ProviderConfig> = {
        let guard = vault.keys.lock().unwrap();
        guard.as_ref().and_then(|k| k.providers.get(&provider).cloned())
    };
    let nonempty = |s: String| if s.is_empty() { None } else { Some(s) };
    let key = api_key
        .and_then(nonempty)
        .or_else(|| vault_cfg.as_ref().and_then(|c| c.api_key.clone()).and_then(nonempty));
    let base = base_url
        .and_then(nonempty)
        .or_else(|| vault_cfg.as_ref().and_then(|c| c.base_url.clone()).and_then(nonempty));
    let result: Result<Vec<String>, AppError> = run_on_os_thread(move || {
        let models = providers_http::list_models(provider.as_str(), key.as_deref(), base.as_deref())?;
        // The chat endpoint is probed as well; some backends (LM Studio's
        // native /api/v1) list models but have no /chat/completions.
        providers_http::probe_chat_path(provider.as_str(), base.as_deref(), key.as_deref())?;
        Ok(models)
    })
    .await
    .map_err(AppError::Provider)?;
    result
}

#[tauri::command]
async fn set_provider(
    provider: tauri_app_core::state::ProviderId,
    config: ProviderConfig,
    state: State<'_, AppState>,
    vault: State<'_, VaultState>,
) -> Result<(), AppError> {
    set_provider_impl(&state, &vault, provider, config)
}

/// Enable Daisy Cloud: generate the install keypair if needed (vault) and
/// register its public key with the license server (re-activation).
#[tauri::command]
async fn register_gateway(
    state: State<'_, AppState>,
    vault: State<'_, VaultState>,
) -> Result<(), AppError> {
    tauri_app_core::commands::lifecycle::register_gateway_impl(&state, &vault)
}

#[tauri::command]
fn lock_vault(vault: State<'_, VaultState>) -> Result<(), AppError> {
    lock_vault_impl(&vault)
}

#[tauri::command]
fn reset_vault(state: State<'_, AppState>, vault: State<'_, VaultState>) -> Result<(), AppError> {
    reset_vault_impl(&state, &vault)
}

#[tauri::command]
async fn move_profile(
    new_path: PathBuf,
    app_handle: tauri::AppHandle,
) -> Result<(), AppError> {
    let handle = app_handle.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let state = app_handle.state::<AppState>();
        move_profile_impl(state.profile.root(), &new_path)
    })
    .await
    .map_err(|e| AppError::Config(format!("move profile task failed: {e}")))??;
    // AppState.profile (and the log file, watchers, caches) bind to the old
    // root for the process lifetime — only a restart lands on the new one.
    handle.restart();
}

#[tauri::command]
fn probe_profile_dir(
    path: PathBuf,
    state: State<'_, AppState>,
) -> Result<ProfileProbe, AppError> {
    probe_profile_dir_impl(state.profile.root(), &path)
}

/// Point the bootstrap at `path` and restart into it. The target keeps (or
/// gets) its own vault/settings; the current profile is left untouched.
#[tauri::command]
fn switch_profile(
    path: PathBuf,
    app_handle: tauri::AppHandle,
    state: State<'_, AppState>,
    active: State<'_, ActiveRecordingHandle>,
) -> Result<(), AppError> {
    if recording_snapshot_impl(&active.0).is_some() {
        return Err(AppError::Config(
            "a recording is in progress — stop it before switching profiles".into(),
        ));
    }
    let probe = probe_profile_dir_impl(state.profile.root(), &path)?;
    if probe.is_current {
        return Err(AppError::Config(
            "that is already the current profile directory".into(),
        ));
    }
    bootstrap_set_impl(path)?;
    app_handle.restart();
}

#[tauri::command]
async fn recordings_stats(app_handle: tauri::AppHandle) -> Result<RecordingsStats, AppError> {
    tauri::async_runtime::spawn_blocking(move || {
        recordings_stats_impl(&app_handle.state::<AppState>())
    })
    .await
    .map_err(|e| AppError::Config(format!("recordings task failed: {e}")))?
}

#[tauri::command]
async fn recordings_delete_all(app_handle: tauri::AppHandle) -> Result<DeleteSummary, AppError> {
    tauri::async_runtime::spawn_blocking(move || {
        delete_recordings_impl(&app_handle.state::<AppState>(), &app_handle)
    })
    .await
    .map_err(|e| AppError::Config(format!("recordings task failed: {e}")))?
}

#[tauri::command]
fn session_has_playback_audio(
    session_id: String,
    state: State<'_, AppState>,
    vault: State<'_, VaultState>,
) -> bool {
    tauri_app_core::commands::playback::session_has_playback_audio_impl(
        &state,
        &vault,
        &session_id,
    )
}

/// Stream the session's meeting.opus to the frontend as raw bytes via the IPC
/// bridge (wrapped in a Blob URL there). Bypasses `asset://`, which fails on
/// some WebKitGTK builds for paths under user sync directories. The impl
/// requires an unlocked vault, validates the session id, and confines the
/// resolved path under the sessions root — see playback.rs for the model.
#[tauri::command]
async fn session_playback_audio_bytes(
    session_id: String,
    app_handle: tauri::AppHandle,
) -> Result<tauri::ipc::Response, AppError> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app_handle.state::<AppState>();
        let vault = app_handle.state::<VaultState>();
        tauri_app_core::commands::playback::session_playback_audio_bytes_impl(
            &state,
            &vault,
            &session_id,
        )
        .map(tauri::ipc::Response::new)
    })
    .await
    .map_err(|e| AppError::Config(format!("playback task failed: {e}")))?
}

#[tauri::command]
fn list_integrations(vault: State<'_, VaultState>) -> Result<Vec<IntegrationPublic>, AppError> {
    list_integrations_impl(&vault)
}

#[tauri::command]
async fn upsert_integration(
    req: UpsertIntegration,
    app_handle: tauri::AppHandle,
) -> Result<IntegrationPublic, AppError> {
    tauri::async_runtime::spawn_blocking(move || {
        upsert_integration_impl(
            &app_handle.state::<AppState>(),
            &app_handle.state::<VaultState>(),
            req,
        )
    })
    .await
    .map_err(|e| AppError::Config(format!("integrations task failed: {e}")))?
}

#[tauri::command]
async fn delete_integration(id: String, app_handle: tauri::AppHandle) -> Result<(), AppError> {
    tauri::async_runtime::spawn_blocking(move || {
        delete_integration_impl(
            &app_handle.state::<AppState>(),
            &app_handle.state::<VaultState>(),
            &id,
        )
    })
    .await
    .map_err(|e| AppError::Config(format!("integrations task failed: {e}")))?
}

#[tauri::command]
async fn integration_push(
    session_id: String,
    integration_id: String,
    app_handle: tauri::AppHandle,
) -> Result<(), AppError> {
    let result: Result<(), AppError> = run_on_os_thread(move || {
        integration_push_impl(
            &app_handle.state::<AppState>(),
            &app_handle.state::<VaultState>(),
            &session_id,
            &integration_id,
            true,
        )
    })
    .await
    .map_err(AppError::Provider)?;
    result
}

#[tauri::command]
fn integration_history(
    limit: Option<usize>,
    state: State<'_, AppState>,
) -> Result<Vec<HistoryEntry>, AppError> {
    read_history(&state, limit)
}

// ---- workflows ----

/// Snapshot + dispatch + worker wake for the explicit trigger sites
/// (Deleted needs a pre-delete snapshot; Imported and FinalizeFailed have no
/// library event of their own kind). Best-effort — never fails the command.
fn workflow_dispatch_with_snapshot(
    app_handle: &tauri::AppHandle,
    trigger: tauri_app_core::commands::workflows::TriggerEvent,
    snap: tauri_app_core::commands::workflows::SessionSnapshot,
) {
    let state = app_handle.state::<AppState>();
    match tauri_app_core::commands::workflow_engine::dispatch(&state, trigger, &snap) {
        Ok(n) if n > 0 => app_handle
            .state::<tauri_app_core::commands::workflow_engine::WorkflowEngineHandle>()
            .0
            .notify_one(),
        Ok(_) => {}
        Err(e) => log::warn!("workflow dispatch ({trigger:?}): {e}"),
    }
}

#[tauri::command]
fn workflows_list(
    state: State<'_, AppState>,
) -> Result<Vec<tauri_app_core::commands::workflows::Workflow>, AppError> {
    tauri_app_core::commands::workflows::list_workflows_impl(&state)
}

#[tauri::command]
fn workflow_upsert(
    req: tauri_app_core::commands::workflows::Workflow,
    state: State<'_, AppState>,
) -> Result<tauri_app_core::commands::workflows::Workflow, AppError> {
    tauri_app_core::commands::workflows::upsert_workflow_impl(&state, req)
}

#[tauri::command]
fn workflow_delete(id: String, state: State<'_, AppState>) -> Result<(), AppError> {
    tauri_app_core::commands::workflows::delete_workflow_impl(&state, &id)
}

#[tauri::command]
fn workflow_history_read(
    limit: usize,
    skip: usize,
    state: State<'_, AppState>,
) -> Result<Vec<tauri_app_core::commands::workflow_history::WorkflowRunRecord>, AppError> {
    tauri_app_core::commands::workflow_history::read_runs(&state, limit.min(500), skip)
}

/// Pending/executing runs, for the History view's in-flight section.
#[tauri::command]
fn workflow_queue_state(
    state: State<'_, AppState>,
) -> Result<Vec<tauri_app_core::commands::workflow_engine::QueuedRun>, AppError> {
    Ok(tauri_app_core::commands::workflow_engine::load_queue(&state)?.runs)
}

/// Embedded help page shown in debug builds when the Vite dev server at
/// `http://localhost:5173` is unreachable.
#[cfg(debug_assertions)]
const DEV_OFFLINE_HTML: &str = include_str!("dev_offline.html");

/// In debug builds, probes `127.0.0.1:5173` and, if it is not listening,
/// navigates the main webview to the embedded offline-help page. Compiled
/// out of release builds.
#[cfg(debug_assertions)]
fn maybe_show_dev_offline_page(app: &tauri::App) {
    use base64::Engine as _;
    use tauri::Manager;
    let addr: std::net::SocketAddr = "127.0.0.1:5173".parse().unwrap();
    let timeout = std::time::Duration::from_millis(300);
    if std::net::TcpStream::connect_timeout(&addr, timeout).is_ok() {
        return;
    }
    log::warn!(
        "Vite dev server at http://localhost:5173 is not responding — showing offline help page"
    );
    let Some(window) = app.get_webview_window("main") else {
        log::warn!("no 'main' window found; cannot show dev-offline page");
        return;
    };
    let b64 = base64::engine::general_purpose::STANDARD.encode(DEV_OFFLINE_HTML);
    let url_str = format!("data:text/html;charset=utf-8;base64,{b64}");
    match url_str.parse::<tauri::Url>() {
        Ok(url) => {
            if let Err(e) = window.navigate(url) {
                log::warn!("failed to navigate to dev-offline page: {e}");
            }
        }
        Err(e) => log::warn!("dev-offline data URL did not parse: {e}"),
    }
}

/// Builds the system-tray icon + menu (Show Daisy / Quit). The tray uses the
/// app's default window icon. On Linux, tray support requires a
/// StatusNotifier host (GNOME needs the AppIndicator extension); with none
/// present, the app runs without a visible tray.
fn setup_tray(app: &tauri::AppHandle) -> tauri::Result<()> {
    use tauri::menu::{MenuBuilder, MenuItemBuilder};
    use tauri::tray::TrayIconBuilder;
    use tauri::Listener;
    use tauri_app_core::mini_window::tray_status_text;

    let show = MenuItemBuilder::with_id("tray_show", "Show Daisy").build(app)?;
    let quit = MenuItemBuilder::with_id("tray_quit", "Quit").build(app)?;
    let menu = MenuBuilder::new(app).items(&[&show, &quit]).build()?;

    let icon = app.default_window_icon().cloned();
    let mut builder = TrayIconBuilder::with_id("daisy-tray")
        .tooltip(tray_status_text("idle"))
        .menu(&menu)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "tray_show" => {
                let _ = show_main_window(app.clone());
            }
            "tray_quit" => app.exit(0),
            _ => {}
        });
    if let Some(icon) = icon {
        builder = builder.icon(icon);
    }
    builder.build(app)?;

    // Keeps the tray tooltip in sync with recording state. The
    // `recording:state` event payload is a JSON string (e.g. "\"recording\"");
    // the quotes are trimmed before mapping via tray_status_text.
    let app_for_tray = app.clone();
    app.listen("recording:state", move |event| {
        let state = event.payload().trim_matches('"');
        if let Some(tray) = app_for_tray.tray_by_id("daisy-tray") {
            let _ = tray.set_tooltip(Some(tray_status_text(state)));
        }
    });
    Ok(())
}

/// Linux only: enables navigator.mediaDevices.getUserMedia inside the
/// WebKitGTK webview.
///
/// Two settings are required, neither on by default:
///   1. WebKitSettings::enable-media-stream must be true.
///   2. The `permission-request` signal handler must explicitly allow the
///      UserMediaPermissionRequest; an unhandled request is auto-denied.
#[cfg(target_os = "linux")]
fn enable_webkit_media_access(window: &tauri::WebviewWindow) {
    use webkit2gtk::{
        glib::object::ObjectExt, PermissionRequestExt, SettingsExt,
        UserMediaPermissionRequest, WebViewExt,
    };
    let res = window.with_webview(|webview| {
        // wry exposes the underlying WebKitWebView via inner().
        let wkv = webview.inner();
        if let Some(settings) = wkv.settings() {
            settings.set_enable_media_stream(true);
            settings.set_enable_mediasource(true);
            // Some WebKitGTK builds require this for getUserMedia on Tauri's
            // custom-protocol origins.
            settings.set_media_playback_requires_user_gesture(false);
        }
        wkv.connect_permission_request(|_view, req| {
            // Only UserMediaPermissionRequest is allowed; other request
            // types are left unhandled.
            if req.is::<UserMediaPermissionRequest>() {
                req.allow();
                true
            } else {
                false
            }
        });
    });
    if let Err(e) = res {
        log::warn!("could not wire WebKit media permissions: {e}");
    }
}

/// Sets the glib program name to `ai.daisy.app`; the Wayland
/// `xdg_toplevel.app_id` and X11 `WM_CLASS` then match the bundle identifier.
#[cfg(target_os = "linux")]
fn set_linux_app_id() {
    use std::ffi::CString;
    extern "C" {
        fn g_set_prgname(prgname: *const std::os::raw::c_char);
        fn g_set_application_name(name: *const std::os::raw::c_char);
    }
    let prgname = CString::new("ai.daisy.app").unwrap();
    let appname = CString::new("Daisy").unwrap();
    // SAFETY: both pointers are valid, NUL-terminated C strings owned by the
    // local CStrings; glib copies the contents internally. Must run before
    // any gtk_init / Tauri builder call.
    unsafe {
        g_set_prgname(prgname.as_ptr());
        g_set_application_name(appname.as_ptr());
    }
}

/// Friendly low-memory reason when the machine is currently RAM-starved,
/// else None. Best-effort: reads current free RAM, not the crash moment.
fn low_memory_reason() -> Option<String> {
    let mut sys = sysinfo::System::new();
    sys.refresh_memory();
    let avail_mb = sys.available_memory() / 1024 / 1024;
    (avail_mb < 1500).then(|| {
        format!("Your device was low on memory ({avail_mb} MB free) — close some apps and try again.")
    })
}

/// Finalizes one orphaned session in-process. Runs the canonical cascade
/// (`finalize_and_summarize_impl`): AEC → transcribe → dedup → diarize →
/// mixdown, then the summary tail. The summary is best-effort and degrades
/// to `None` when the vault is locked or no provider is configured; runs at
/// startup before unlock. `skip_label_gate = true`: recovery never pauses
/// for speaker labels. Returns true if the manifest is now stamped
/// finalized. Panics are isolated to the caller's thread; the per-session
/// attempt cap (see `recover_orphan_sessions`) bounds retries.
fn finalize_orphan_in_app(app_handle: &tauri::AppHandle, session_id: &str) -> bool {
    use tauri_app_core::commands::finalize as fin;
    let dir = {
        let state = app_handle.state::<AppState>();
        state.profile.session_path(session_id)
    };
    let req = fin::FinalizeRequest {
        session_id: session_id.to_string(),
        summary_provider: None,
        model: None,
        skip_label_gate: true,
    };
    let outcome = {
        let state = app_handle.state::<AppState>();
        let vault = app_handle.state::<VaultState>();
        fin::finalize_and_summarize_impl(&state, &vault, req, |_ev| {})
    };
    match &outcome {
        Ok(_) => log::info!("orphan recovery: finalized {session_id} in-app"),
        Err(e) => log::warn!("orphan recovery: in-app finalize failed for {session_id}: {e}"),
    }
    let finalized = syncsafe::read(dir.join("manifest.json"))
        .ok()
        .and_then(|b| serde_json::from_slice::<recording::manifest::SessionManifest>(&b).ok())
        .map(|m| m.finalized_at_unix_seconds.is_some())
        .unwrap_or(false);
    if finalized {
        fin::clear_finalize_recovery(&dir);
    }
    tauri_app_core::library_events::emit(
        app_handle,
        tauri_app_core::library_events::LibraryChangeKind::Finalized,
        session_id,
    );
    finalized
}

/// Runs in-app finalize recovery for any session whose manifest is missing
/// finalized_at_unix_seconds. Skips sessions younger than 60s or older than
/// 30 days.
fn recover_orphan_sessions(state: &AppState, app_handle: tauri::AppHandle) {
    let sessions_dir = state.profile.root().join("sessions");
    let Ok(entries) = std::fs::read_dir(&sessions_dir) else {
        return;
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    // Eligible orphans + attempt bookkeeping are collected up front, then one
    // background thread finalizes them sequentially.
    let mut eligible: Vec<String> = Vec::new();
    for ent in entries.flatten() {
        let p = ent.path();
        if !p.is_dir() {
            continue;
        }
        let mp = p.join("manifest.json");
        let Ok(bytes) = syncsafe::read(&mp) else { continue };
        let Ok(m): std::result::Result<recording::manifest::SessionManifest, _> =
            serde_json::from_slice(&bytes)
        else {
            continue;
        };
        if m.finalized_at_unix_seconds.is_some() {
            continue;
        }
        let age = now - m.created_at_unix_seconds;
        if age < 60 || age > 60 * 60 * 24 * 30 {
            continue;
        }
        // Only sessions with at least one audio chunk on disk are processed.
        let has_audio = m.chunks.iter().any(|c| {
            p.join(&c.mic_wav_relative).is_file() || p.join(&c.system_wav_relative).is_file()
        });
        if !has_audio {
            continue;
        }
        // Attempts are counted in a sidecar; recovery gives up once over the
        // cap.
        use tauri_app_core::commands::finalize as fin;
        let mut rec = fin::read_finalize_recovery(&p);
        if rec.failed {
            continue; // already given up — waits for a user "Retry"
        }
        if rec.attempts >= fin::FINALIZE_MAX_ATTEMPTS {
            let reason = low_memory_reason()
                .unwrap_or_else(|| "We couldn't finish processing this recording.".to_string());
            log::warn!(
                "orphan recovery: giving up on {} after {} attempts ({reason})",
                m.session_id,
                rec.attempts
            );
            rec.failed = true;
            rec.reason = Some(reason);
            rec.updated_at_unix = now;
            fin::write_finalize_recovery(&p, &rec);
            tauri_app_core::library_events::emit(
                &app_handle,
                tauri_app_core::library_events::LibraryChangeKind::Finalized,
                &m.session_id,
            );
            continue;
        }
        rec.attempts += 1;
        rec.updated_at_unix = now;
        fin::write_finalize_recovery(&p, &rec);
        eligible.push(m.session_id.clone());
    }
    if eligible.is_empty() {
        return;
    }
    log::info!("orphan recovery: {} session(s) to finalize in-app", eligible.len());
    std::thread::spawn(move || {
        for sid in eligible {
            finalize_orphan_in_app(&app_handle, &sid);
        }
        log::info!("orphan recovery: finished");
    });
}


use tauri_app_core::openblas::cap_openblas_threads;

fn main() {
    // GDK_BACKEND: DAISY_FORCE_X11=1 opts into Xwayland; the platform
    // default is kept when unset. Set before any GTK/GDK init.
    #[cfg(target_os = "linux")]
    if std::env::var_os("GDK_BACKEND").is_none()
        && std::env::var("DAISY_FORCE_X11").map(|v| v == "1").unwrap_or(false)
    {
        std::env::set_var("GDK_BACKEND", "x11");
    }
    // Logs which webkit compositor path is active. The WEBKIT_DISABLE_*
    // flags are set by the AppRun wrapper (DAISY_DISABLE_GPU=1 selects the
    // software path).
    {
        let dmabuf_off = std::env::var("WEBKIT_DISABLE_DMABUF_RENDERER")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        let compositing_off = std::env::var("WEBKIT_DISABLE_COMPOSITING_MODE")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        let mode = if compositing_off {
            "software (compositing disabled)"
        } else if dmabuf_off {
            "software (dmabuf disabled, GBM fallback off)"
        } else {
            "hardware (dmabuf/GBM, WebKit-default)"
        };
        // The logger is not initialized yet; printed straight to stderr.
        eprintln!("[startup] webkit compositor: {mode}");
    }
    #[cfg(target_os = "linux")]
    set_linux_app_id();
    // Caps per-call intra-op thread pools (OpenBLAS via whisper-rs ggml, and
    // the DTLN-AEC ONNX sessions). Set before the first BLAS/ORT init; both
    // libraries read the env vars at session/pool creation.
    std::env::set_var("OPENBLAS_NUM_THREADS", "1");
    std::env::set_var("OMP_NUM_THREADS", "1");
    std::env::set_var("DAISY_AEC_THREADS", "1");
    // The libopenblas C API is also called directly; the env vars alone do
    // not always take effect.
    #[cfg(target_os = "linux")]
    cap_openblas_threads(1);

    // The profile is resolved before the logger; the log file lives inside
    // the profile. Resolution emits no log macros.
    let profile = match ProfileDir::resolve_with_bootstrap() {
        Ok(Some(p)) => p,
        Ok(None) => ProfileDir::platform_default().expect("resolve platform default"),
        Err(_) => ProfileDir::platform_default().expect("resolve platform default"),
    };

    // Dual-sink logging: stderr at WARN+, file at INFO+/DEBUG+ per settings.
    let settings_for_logger =
        tauri_app_core::settings::Settings::load_or_default(&profile.settings_path());
    let debug_level = settings_for_logger.debug_level;
    // `Full` sets DAISY_LIVE_TRACE, a cross-crate env signal read by
    // providers-local / audio-engine.
    if debug_level == tauri_app_core::settings::DebugLevel::Full {
        std::env::set_var("DAISY_LIVE_TRACE", "1");
    }
    let debug_logging = debug_level.verbose();
    if let Err(e) = tauri_app_core::logging::init(&profile.logs_dir(), debug_logging) {
        eprintln!("logging init failed: {e}");
    }
    log::info!("daisy build sha: {}", env!("DAISY_BUILD_SHA"));
    log::info!("daisy profile: {}", profile.root().display());
    log::info!(
        "logging: file={} level={debug_level:?} verbose={}",
        profile.logs_dir().join("daisy.log").display(),
        debug_logging,
    );
    // Single-instance guard: one daisy per profile. A stale lockfile is
    // reclaimed by claim() below.
    if tauri_app_core::single_instance::another_instance_alive(profile.root()) {
        log::error!(
            "another daisy instance is already running for this profile ({}); refusing to start a second copy (it would share the vault, log, and MCP port). Close the other instance first.",
            profile.root().display()
        );
        eprintln!("Daisy is already running for this profile — close the other window first.");
        // Pre-webview: a native OS dialog is shown (Linux uses rfd's gtk3
        // sync backend).
        rfd::MessageDialog::new()
            .set_level(rfd::MessageLevel::Warning)
            .set_title("Daisy is already running")
            .set_description(
                "Daisy is already open for this profile. Switch to the existing Daisy window — only one copy can run at a time.",
            )
            .set_buttons(rfd::MessageButtons::Ok)
            .show();
        std::process::exit(1);
    }
    tauri_app_core::single_instance::claim(profile.root());
    // Diagnostics: samples system memory/swap/CPU + heaviest processes into
    // the log. Thread runs for the app lifetime.
    if debug_logging {
        // Samples every 10s, only while a recording is in progress (gated by
        // perf::set_recording_active).
        let _ = tauri_app_core::perf::spawn(10);
        // Top processes are logged as per-session salted hashes, not names.
        log::info!("perf sampler: started (every 10s while recording; salted-hash process ids; whisper rtf/backlog)");
    }
    // One platform/capabilities block is logged per launch.
    {
        let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(0);
        let has_battery = tauri_app_core::hardware::has_battery();
        let is_mac = cfg!(target_os = "macos");
        let metal = cfg!(target_os = "macos");
        // Mirrors build_live_mode's no-key form-factor choice + the finalize cap.
        let live_budget = (cores / 2).clamp(1, 6);
        log::info!("=== daisy telemetry ===");
        log::info!(
            "platform: os={} arch={} cores={} form_factor={} mac={} metal={}",
            std::env::consts::OS,
            std::env::consts::ARCH,
            cores,
            if has_battery { "laptop" } else { "desktop" },
            is_mac,
            metal,
        );
        log::info!(
            "version: {} sha: {}",
            env!("CARGO_PKG_VERSION"),
            env!("DAISY_BUILD_SHA"),
        );
        log::info!(
            "asr caps: live_whisper_threads={} finalize_whisper_threads<=8",
            live_budget,
        );
    }
    let app_state = AppState::new(profile);
    let active = ActiveRecordingHandle(Mutex::new(None));
    let vault_state = VaultState::new();

    tauri::Builder::default()
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(app_state)
        .manage(active)
        .manage(vault_state)
        .manage(ReminderState(Mutex::new(None)))
        .manage(tauri_app_core::mcp::server::McpHandle::default())
        .setup(|app| {
            // Points the model-dir env vars at Tauri's bundled resource dir
            // on non-AppImage installs (Windows NSIS/portable, macOS .app).
            // The Linux AppImage's AppRun exports these before launch; there
            // resource_dir() has no models/* and the `is_dir()` guards leave
            // AppRun's values in place.
            if let Ok(res_dir) = app.path().resource_dir() {
                for (sub, var) in [
                    ("models/voiceprints", "DAISY_VOICEPRINT_DIR"),
                    ("models/whisper", "DAISY_WHISPER_MODEL_DIR"),
                    ("models/dtln-aec", "DAISY_MODEL_DIR"),
                    ("models/embeddings", "DAISY_EMBED_DIR"),
                    ("models/speakrs", "SPEAKRS_MODELS_DIR"),
                ] {
                    let dir = res_dir.join(sub);
                    if dir.is_dir() {
                        std::env::set_var(var, &dir);
                        log::info!("{var}: {}", dir.display());
                    }
                }
            }
            #[cfg(debug_assertions)]
            maybe_show_dev_offline_page(app);
            if let Err(e) = setup_tray(app.handle()) {
                log::warn!("system tray unavailable: {e}");
            }
            // License check-in (heartbeat) — fire-and-forget on launch;
            // throttled to about once per day.
            tauri::async_runtime::spawn_blocking(|| {
                if let Err(e) = tauri_app_core::commands::license::checkin_if_needed_impl() {
                    log::warn!("license check-in on launch: {e:?}");
                }
            });
            // Periodic process telemetry (CPU%, RSS, threads), logged to the
            // regular log stream.
            tauri_app_core::telemetry::spawn();
            // Auto-unlocks machine-mode vaults on launch. Runs synchronously,
            // before setup returns and the frontend can call vault_status.
            // Passphrase-mode vaults are a no-op and still prompt.
            {
                let state = app.state::<AppState>();
                let vault = app.state::<VaultState>();
                match unlock_if_machine_mode_impl(&state, &vault) {
                    Ok(true) => log::info!("vault: auto-unlocked (machine mode)"),
                    Ok(false) => {}
                    Err(e) => log::warn!("vault: machine-mode auto-unlock failed: {e:?}"),
                }
            }
            // Local MCP server (loopback) — started if enabled in settings,
            // after the machine-mode auto-unlock.
            if let Err(e) = tauri_app_core::mcp::server::apply_mcp_state(app.handle()) {
                log::warn!("mcp: startup: {e:?}");
            }
            // Workflow engine: serial queue worker + the Finalized-trigger
            // listener. The listener subscribes to library:changed; all
            // finalize completion paths (cascade, resume, mark-complete,
            // startup recovery) dispatch through it. Deleted / Imported /
            // FinalizeFailed dispatch explicitly at their command sites.
            {
                let notify = tauri_app_core::commands::workflow_engine::spawn_worker(app.handle().clone());
                app.manage(tauri_app_core::commands::workflow_engine::WorkflowEngineHandle(notify));
                use tauri::Listener;
                let wf_handle = app.handle().clone();
                app.listen("library:changed", move |event| {
                    #[derive(serde::Deserialize)]
                    struct P {
                        kind: String,
                        session_id: String,
                    }
                    let Ok(p) = serde_json::from_str::<P>(event.payload()) else { return };
                    if p.kind != "finalized" {
                        return;
                    }
                    let state = wf_handle.state::<AppState>();
                    match tauri_app_core::commands::workflows::snapshot_for_session(&state, &p.session_id) {
                        Ok(snap) => {
                            drop(state);
                            workflow_dispatch_with_snapshot(
                                &wf_handle,
                                tauri_app_core::commands::workflows::TriggerEvent::Finalized,
                                snap,
                            );
                        }
                        Err(e) => log::warn!("workflow snapshot (finalized {}): {e}", p.session_id),
                    }
                });
            }
            // Orphan recovery: finalizes past sessions whose manifest still
            // has finalized_at_unix_seconds == None and audio on disk.
            {
                let app_handle = app.handle().clone();
                tauri::async_runtime::spawn_blocking(move || {
                    let state = app_handle.state::<AppState>();
                    let vault = app_handle.state::<VaultState>();
                    // Reconciles the profile name to the license on every launch.
                    pin_profile_name_to_license(&state);
                    recover_orphan_sessions(&state, app_handle.clone());
                    // Integrity sweep: rebuilds missing dedup / transcript.md /
                    // meeting.opus across finalized sessions (local repairs
                    // only; LLM repairs are user-triggered).
                    let (sessions, repairs) = tauri_app_core::commands::integrity::audit_and_repair_local(
                        &state, &vault, &app_handle,
                    );
                    if repairs > 0 {
                        log::info!("integrity: repaired {repairs} artifact(s) across {sessions} session(s)");
                    }
                });
            }
            // Closing the main window quits the whole app.
            if let Some(main) = app.get_webview_window(MAIN_LABEL) {
                let app_handle = app.handle().clone();
                main.on_window_event(move |event| {
                    if let tauri::WindowEvent::CloseRequested { .. } = event {
                        // Flushes any in-progress recording before quitting:
                        // recorder.stop() closes the chunk, writes the
                        // manifest, and removes the heartbeat; next-launch
                        // recovery then finalizes the session.
                        let ar = app_handle
                            .state::<ActiveRecordingHandle>()
                            .0
                            .lock()
                            .unwrap()
                            .take();
                        if let Some(ar) = ar {
                            log::info!("app close: flushing active recording before exit");
                            if let Err(e) = ar.recorder.stop() {
                                log::warn!("app close: recorder.stop() failed: {e}");
                            }
                        }
                        app_handle.exit(0);
                    }
                });
                #[cfg(target_os = "linux")]
                enable_webkit_media_access(&main);
            }
            // Pre-builds the mini-window (hidden) on the main thread.
            if let Err(e) = ensure_mini_window(app.handle()) {
                log::warn!("mini-window pre-create failed: {e}");
            }
            // Pre-builds the meeting-reminder popup (hidden).
            if let Err(e) = ensure_reminder_window(app.handle()) {
                log::warn!("reminder-window pre-create failed: {e}");
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            bootstrap_status,
            bootstrap_set,
            consent_status,
            build_info,
            capture_permission_status,
            live_captions_status,
            set_live_captions_choice,
            run_live_captions_bench,
            accept_consent,
            eula_status,
            accept_eula,
            profile_binding_check,
            license_status,
            activate_license,
            license_checkin,
            deactivate_license,
            check_for_update,
            maybe_rotate_chunk,
            open_external,
            open_logs_dir,
            open_profile_dir,
            vault_status,
            init_vault,
            unlock_vault,
            change_vault_passphrase,
            switch_vault_mode,
            init_vault_machine_mode,
            vault_kind,
            read_settings,
            write_settings,
            mcp_status,
            mcp_apply,
            mcp_regenerate_token,
            mcp_port_available,
            list_audio_sources,
            start_mic_meter,
            stop_mic_meter,
            calibrate_speech_level,
            speech_level_set_override,
            speech_levels_list,
            list_providers,
            list_provider_models,
            set_provider,
            register_gateway,
            lock_vault,
            reset_vault,
            list_sessions,
            read_session,
            list_whisper_models,
            set_active_whisper_model,
            delete_whisper_model,
            download_whisper_model,
            cancel_whisper_download,
            set_session_speaker_label,
            remove_speaker_cluster,
            add_session_speaker,
            list_session_speakers,
            rerender_session_transcript,
            session_speaker_sample_audio_bytes,
            session_speaker_sample_text,
            delete_session,
            start_recording,
            pause_recording,
            switch_recording_mic,
            set_mic_muted,
            resume_recording,
            stop_recording,
            cancel_recording,
            current_recording,
            recording_snapshot,
            show_mini_window,
            show_main_window,
            show_reminder_window,
            reminder_payload,
            reminder_action,
            transcribe,
            dedup,
            polish,
            recording_finalize_and_summarize,
            mark_session_complete,
            repair_session,
            recording_resume_finalize,
            read_finalize_status,
            read_live_transcript,
            diarize_session,
            retry_finalize,
            finalize_recovery,
            qa_ask,
            qa_ask_stream,
            live_chat_send,
            live_chat_send_stream,
            live_chat_load,
            live_chat_delete,
            list_voiceprints,
            rename_voiceprint,
            delete_voiceprint,
            detach_speaker_voiceprint,
            enroll_voiceprint_from_speaker,
            rematch_all_sessions,
            extract_session_chapters,
            load_session_chapters,
            list_calendar_subscriptions,
            add_calendar_subscription,
            update_calendar_subscription,
            delete_calendar_subscription,
            refresh_calendars,
            list_upcoming_events,
            dismiss_calendar_event,
            run_analysis,
            analysis_load,
            list_contacts,
            list_prompts,
            save_prompt,
            delete_prompt,
            reset_prompt,
            set_default_summary_prompt,
            list_tags,
            create_tag,
            update_tag,
            delete_tag,
            search_tags,
            session_meta_get,
            session_meta_update,
            session_notes_load,
            session_notes_save,
            create_note_session,
            import_audio_meeting,
            session_assign_tags,
            summary_load,
            summary_save_edit,
            summary_regenerate,
            summary_provider_status,
            search_sessions,
            move_profile,
            probe_profile_dir,
            switch_profile,
            recordings_stats,
            recordings_delete_all,
            session_has_playback_audio,
            session_playback_audio_bytes,
            list_integrations,
            upsert_integration,
            delete_integration,
            integration_push,
            integration_history,
            workflows_list,
            workflow_upsert,
            workflow_delete,
            workflow_history_read,
            workflow_queue_state,
            save_text_file,
            env_profile_dir
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri app");
}
