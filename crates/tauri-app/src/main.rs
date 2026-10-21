// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
#![cfg_attr(all(not(debug_assertions), target_os = "windows"), windows_subsystem = "windows")]

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Mutex;
use tauri::{Emitter, Manager, State};
use tauri_app_core::ai_jobs::{AiJob, AiJobKind, AiJobs};
use tauri_app_core::commands::bootstrap::{
    bootstrap_set_impl, bootstrap_status_impl, BootstrapStatus,
};
use tauri_app_core::commands::migrate::{move_profile_picked_impl, probe_profile_dir_picked_impl, ProfileProbe};
use tauri_app_core::commands::library::{list_sessions_impl, SessionListEntry};
use tauri_app_core::commands::lifecycle::{
    change_vault_passphrase_impl, init_vault_machine_mode_impl,
    list_providers_impl, read_vault_kind, read_vault_trust, reset_vault_impl,
    set_provider_impl,
    switch_vault_mode_impl,
    unlock_if_machine_mode_impl, unlock_vault_impl, vault_closed, vault_opened, vault_path,
    vault_status_impl, ProviderListEntry, VaultStatus, VaultTrust,
};
use tauri_app_core::commands::meeting::{
    session_assign_tags_impl, session_meta_get_impl, session_meta_update_impl,
    session_notes_load_impl, session_notes_save_impl, SessionMeta, SessionMetaUpdate,
};
use tauri_app_core::commands::search::{search_sessions_impl, SearchRequest, SessionHit};
use tauri_app_core::commands::summary::{
    summary_load_impl, summary_regenerate_impl, summary_save_edit_impl,
};
use tauri_app_core::commands::tags::{
    create_tag_impl, delete_tag_impl, list_tags_impl, search_tags_impl, update_tag_impl,
    CreateTagRequest, DeleteTagResult, UpdateTagRequest,
};
use tauri_app_core::commands::tag_suggest::{
    tag_suggest_prerecord_impl, tag_suggest_for_session_impl, session_dismiss_tag_suggestions_impl,
    session_undismiss_tag_suggestions_impl, session_apply_tag_suggestions_impl,
    LastTimeSuggestion, SessionSuggestions,
};
use tauri_app_core::commands::settings::{
    list_audio_sources_impl, update_settings_impl, AudioSourceInfo, MachinePatch,
    MachineSettingsStatus, SettingsPatch,
};
use tauri_app_core::settings::Settings;
use tauri_app_core::commands::pipeline::{
    dedup_impl, polish_impl, transcribe_impl, DedupRequest, DedupSummary, PolishRequest,
    PolishSummary, TranscribeRequest,
};
use tauri_app_core::commands::recording::{
    pause_impl, quick_start_recording, recording_snapshot_impl, resume_impl,
    mic_meter_registry, start_recording_from,
    ActiveRecordingHandle, ClockStyle, RecordingSnapshot, StartRequest, StopMode,
    TopologyHubHandle,
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
    remove_speaker_cluster_impl, set_session_speaker_label_impl,
    SessionSpeaker, SessionView,
};
use tauri_app_core::error::AppError;
use tauri_app_core::mini_window::{
    close_action, snooze_until, CloseAction, CloseChoice, MiniWindowConfig, ReminderChoice, ReminderPayload, MAIN_LABEL, MINI_LABEL,
    REMINDER_LABEL,
};
use tauri_app_core::profile::ProfileDir;
use tauri_app_core::state::{AppState, ProviderConfig, Tag, VaultState};

/// Builds a frameless auxiliary webview window (created hidden). Idempotent:
/// returns the existing window if already built. Must be called on the main
/// thread (Tauri's `setup` hook or the main event loop). A WM close
/// (Alt+F4 / window-menu) is intercepted — the window stays alive — and
/// then handed to `on_close`.
fn ensure_aux_window(
    app: &tauri::AppHandle,
    label: &str,
    cfg: MiniWindowConfig,
    on_close: impl Fn(&tauri::AppHandle) + Send + 'static,
) -> Result<tauri::WebviewWindow, String> {
    use tauri::{Manager, WebviewUrl, WebviewWindowBuilder, WindowEvent};
    if let Some(w) = app.get_webview_window(label) {
        return Ok(w);
    }
    let built = WebviewWindowBuilder::new(app, label, WebviewUrl::App("index.html".into()))
        .title("Daisy")
        .inner_size(cfg.width, cfg.height)
        .decorations(cfg.decorations)
        .always_on_top(cfg.always_on_top)
        .resizable(cfg.resizable)
        .skip_taskbar(cfg.skip_taskbar)
        .visible(false)
        .build()
        .map_err(|e| format!("build {label} window: {e}"))?;
    let app_for_close = app.clone();
    built.on_window_event(move |event| {
        if let WindowEvent::CloseRequested { api, .. } = event {
            api.prevent_close();
            on_close(&app_for_close);
        }
    });
    Ok(built)
}

/// The mini-window: closing it restores the main window.
fn ensure_mini_window(app: &tauri::AppHandle) -> Result<tauri::WebviewWindow, String> {
    ensure_aux_window(app, MINI_LABEL, MiniWindowConfig::card(), |app| {
        let _ = tauri_app_core::mini_window::show_main(app);
    })
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
    tauri_app_core::mini_window::show_main(&app)
}

/// What the reminder popup is currently showing. The reminder webview reads
/// it on mount via `reminder_payload`.
struct ReminderState(Mutex<Option<ReminderPayload>>);

/// Set while the webview shows the close prompt.
struct ClosePromptPending(AtomicBool);

/// Minimizes the main window, leaving fullscreen first.
fn minimize_main(app: &tauri::AppHandle) {
    let Some(main) = app.get_webview_window(MAIN_LABEL) else { return };
    #[cfg(target_os = "macos")]
    if main.is_fullscreen().unwrap_or(false) {
        if let Err(e) = main.set_fullscreen(false) {
            log::warn!("leave fullscreen: {e}");
        }
        std::thread::spawn(move || {
            for _ in 0..60 {
                if !main.is_fullscreen().unwrap_or(false) {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            if let Err(e) = main.minimize() {
                log::warn!("minimize main window: {e}");
            }
        });
        return;
    }
    if let Err(e) = main.minimize() {
        log::warn!("minimize main window: {e}");
    }
}

/// Answers the close prompt. Minimize and Quit are remembered.
#[tauri::command]
fn resolve_close_prompt(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    pending: State<'_, ClosePromptPending>,
    choice: String,
) -> Result<(), AppError> {
    use tauri_app_core::commands::settings::apply_machine_patch;
    let choice = CloseChoice::parse(&choice)
        .ok_or_else(|| AppError::Config(format!("unknown close choice: {choice}")))?;
    if !pending.0.swap(false, std::sync::atomic::Ordering::SeqCst) {
        return Ok(());
    }
    let remember = match choice {
        CloseChoice::Minimize => true,
        CloseChoice::Quit => false,
        CloseChoice::Cancel => return Ok(()),
    };
    let patch = MachinePatch { minimize_on_close: Some(remember), ..Default::default() };
    if let Err(e) = apply_machine_patch(&state, patch) {
        log::warn!("close prompt: save settings failed: {e}");
    }
    if remember {
        minimize_main(&app);
    } else {
        app.exit(0);
    }
    Ok(())
}

// ---- ASR model management --------------------------------------------------

/// List every registry model row: engine, installed/active flags, floor.
#[tauri::command]
fn list_asr_models(
    state: State<'_, AppState>,
) -> Result<Vec<tauri_app_core::commands::asr_models::AsrModelInfo>, AppError> {
    tauri_app_core::commands::asr_models::list_asr_models_impl(&state)
}

/// Switch the model used for transcription. `id` must be an installed registry row.
#[tauri::command]
fn set_active_asr_model(
    id: String,
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<(), AppError> {
    tauri_app_core::commands::asr_models::set_active_asr_model_impl(&state, &id)?;
    tauri_app_core::commands::asr_models::emit(&app);
    Ok(())
}

/// Delete an installed model (never a bundled floor row; resets active if needed).
#[tauri::command]
fn delete_asr_model(
    id: String,
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<(), AppError> {
    tauri_app_core::commands::asr_models::delete_asr_model_impl(&state, &id)?;
    tauri_app_core::downloads::emit(&app, &state.downloads);
    tauri_app_core::commands::asr_models::emit(&app);
    Ok(())
}

// ---- model downloads -------------------------------------------------------

/// Everything the download queue is doing right now.
#[tauri::command]
fn download_queue_status(state: State<'_, AppState>) -> Vec<tauri_app_core::downloads::DownloadJob> {
    state.downloads.status()
}

/// Queue a model download. Enqueuing an id already queued changes nothing.
/// The built-in model is queued only on a computer that can run it.
#[tauri::command]
async fn download_queue_enqueue(
    kind: tauri_app_core::downloads::DownloadKind,
    id: String,
    app: tauri::AppHandle,
) -> Result<(), AppError> {
    use tauri_app_core::downloads::DownloadKind;
    let task = app.clone();
    tokio::task::spawn_blocking(move || {
        let state = task.state::<AppState>();
        match kind {
            DownloadKind::Llm if id == tauri_app_core::builtin_model::row().id => {
                tauri_app_core::builtin_model::install(&state)
            }
            DownloadKind::Llm => Err(AppError::Config(format!("unknown model {id:?}"))),
            _ => state.downloads.enqueue(kind, &id),
        }
    })
    .await
    .map_err(|e| AppError::Io(format!("enqueue task join: {e}")))??;
    tauri_app_core::downloads::emit(&app, &app.state::<AppState>().downloads);
    if kind == DownloadKind::Llm {
        tauri_app_core::builtin_model::emit(&app);
    }
    Ok(())
}

/// Drop a queued or running download and everything it staged. Cancelling
/// the built-in model's download deletes whatever of it reached the disk.
#[tauri::command]
async fn download_queue_cancel(id: String, app: tauri::AppHandle) -> Result<(), AppError> {
    let builtin = id == tauri_app_core::builtin_model::row().id;
    let task = app.clone();
    tokio::task::spawn_blocking(move || {
        let state = task.state::<AppState>();
        if builtin {
            tauri_app_core::builtin_model::delete(&state)
        } else {
            state.downloads.cancel(&id)
        }
    })
    .await
    .map_err(|e| AppError::Io(format!("cancel task join: {e}")))??;
    tauri_app_core::downloads::emit(&app, &app.state::<AppState>().downloads);
    if builtin {
        tauri_app_core::builtin_model::emit(&app);
    }
    Ok(())
}

/// Put a failed download back in line with a fresh attempt budget.
#[tauri::command]
fn download_queue_retry(
    id: String,
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<(), AppError> {
    state.downloads.retry(&id)?;
    tauri_app_core::downloads::emit(&app, &state.downloads);
    if id == tauri_app_core::builtin_model::row().id {
        tauri_app_core::builtin_model::emit(&app);
    }
    Ok(())
}

// ---- the built-in model ------------------------------------------------------

/// The built-in model's row and the AI provider status. The first call
/// probes the hardware; `builtin-model:changed` carries every later change.
#[tauri::command]
async fn builtin_model_status(
    app: tauri::AppHandle,
) -> Result<tauri_app_core::builtin_model::BuiltinStatus, AppError> {
    tokio::task::spawn_blocking(move || {
        tauri_app_core::builtin_model::status(&app.state::<AppState>(), &app.state::<VaultState>())
    })
    .await
    .map_err(|e| AppError::Io(format!("status task join: {e}")))
}

/// Delete the built-in model: its download, worker, weights and a `local`
/// provider choice on this machine. A request in flight on it ends with an
/// error.
#[tauri::command]
async fn builtin_model_delete(app: tauri::AppHandle) -> Result<(), AppError> {
    let task = app.clone();
    let deleted = tokio::task::spawn_blocking(move || {
        tauri_app_core::builtin_model::delete(&task.state::<AppState>())
    })
    .await
    .map_err(|e| AppError::Io(format!("delete task join: {e}")))?;
    tauri_app_core::downloads::emit(&app, &app.state::<AppState>().downloads);
    tauri_app_core::builtin_model::emit(&app);
    deleted
}

/// The meeting-reminder popup: a WM close only hides it; the main window
/// stays put.
fn ensure_reminder_window(app: &tauri::AppHandle) -> Result<tauri::WebviewWindow, String> {
    ensure_aux_window(app, REMINDER_LABEL, MiniWindowConfig::reminder(), |_| {})
}

/// Positions the reminder popup at the top-right of the monitor under the cursor, else the main window's, else the mini's, else the primary. `logical_width` is the width the window has just been sized to.
fn place_reminder_top_right(win: &tauri::WebviewWindow, logical_width: f64) {
    use tauri::Manager;
    let app = win.app_handle();
    let monitor = app
        .cursor_position()
        .ok()
        .and_then(|p| app.monitor_from_point(p.x, p.y).ok().flatten())
        .or_else(|| {
            app.get_webview_window(MAIN_LABEL)
                .and_then(|w| w.current_monitor().ok().flatten())
        })
        .or_else(|| {
            app.get_webview_window(MINI_LABEL)
                .and_then(|w| w.current_monitor().ok().flatten())
        })
        .or_else(|| app.primary_monitor().ok().flatten());
    if let Some(mon) = monitor {
        let msize = mon.size();
        let mpos = mon.position();
        let scale = mon.scale_factor();
        let win_w = (logical_width * scale) as i32;
        let margin = (16.0 * scale) as i32;
        let x = mpos.x + msize.width as i32 - win_w - margin;
        let y = mpos.y + margin;
        let _ = win.set_position(tauri::PhysicalPosition::new(x, y));
    }
}

/// Shows the reminder popup with `payload`, sized and positioned per `cfg`:
/// stores the payload, sizes the window, positions it top-right, shows +
/// focuses it, and tells the reminder webview to re-read.
fn show_reminder(
    app: &tauri::AppHandle,
    payload: ReminderPayload,
    cfg: MiniWindowConfig,
) -> Result<(), String> {
    use tauri::{Emitter, Manager};
    let win = ensure_reminder_window(app)?;
    if let Some(state) = app.try_state::<ReminderState>() {
        *state.0.lock().unwrap() = Some(payload);
    }
    let _ = win.set_size(tauri::LogicalSize::new(cfg.width, cfg.height));
    place_reminder_top_right(&win, cfg.width);
    let _ = app.emit_to(REMINDER_LABEL, "reminder:show", ());
    win.show().map_err(|e| format!("show reminder window: {e}"))?;
    win.set_focus().map_err(|e| format!("focus reminder window: {e}"))?;
    Ok(())
}

/// Shows the meeting-reminder popup for `title`: stores the calendar payload,
/// restores the calendar size, positions the window top-right, shows +
/// focuses it, and tells the reminder webview to re-read.
#[tauri::command]
fn show_reminder_window(app: tauri::AppHandle, title: String) -> Result<(), String> {
    log::info!("reminder: showing popup for {title:?}");
    show_reminder(
        &app,
        ReminderPayload::Calendar { title },
        MiniWindowConfig::reminder(),
    )
}

/// Shows the meeting-started prompt: stores the meeting payload, sizes the
/// window to the taller face, positions it top-right, and shows it.
fn show_meeting_prompt(app: &tauri::AppHandle) -> Result<(), String> {
    log::info!("reminder: showing meeting prompt");
    show_reminder(app, ReminderPayload::Meeting, MiniWindowConfig::reminder_meeting())
}

/// Shows the meeting-ended prompt for `app`, the app that left the call.
fn show_meeting_ended_prompt(app: &tauri::AppHandle, name: &str) -> Result<(), String> {
    log::info!("reminder: showing meeting-ended prompt");
    show_reminder(
        app,
        ReminderPayload::MeetingEnded { app: name.to_string() },
        MiniWindowConfig::reminder(),
    )
}

/// What the reminder popup should display.
#[tauri::command]
fn reminder_payload(app: tauri::AppHandle) -> Option<ReminderPayload> {
    use tauri::Manager;
    app.try_state::<ReminderState>()
        .and_then(|s| s.0.lock().unwrap().clone())
}

/// Whether this machine can tell when another app opens the microphone.
#[derive(serde::Serialize)]
struct MeetingDetectionSupport {
    supported: bool,
    reason: Option<&'static str>,
}

#[tauri::command]
fn meeting_detection_support() -> MeetingDetectionSupport {
    use audio_engine::mic_clients::{mic_clients_support, MicClientsSupport};
    match mic_clients_support() {
        MicClientsSupport::Yes => MeetingDetectionSupport { supported: true, reason: None },
        MicClientsSupport::No(reason) => {
            MeetingDetectionSupport { supported: false, reason: Some(reason) }
        }
    }
}

/// Writes to the machine's `meeting_prompt` block. A write failure is logged and returned.
fn write_meeting_prompt(
    app: &tauri::AppHandle,
    f: impl FnOnce(&mut tauri_app_core::settings::MeetingPrompt),
) -> Result<(), String> {
    use tauri::Manager;
    let Some(state) = app.try_state::<AppState>() else {
        return Ok(());
    };
    state.update_machine_settings(|m| f(&mut m.meeting_prompt)).map_err(|e| {
        log::warn!("meeting prompt setting: {e}");
        AppError::from(e).friendly()
    })?;
    Ok(())
}

/// Answers one of the reminder popup's buttons. Always hides the popup.
#[tauri::command]
fn reminder_action(app: tauri::AppHandle, choice: String) -> Result<(), String> {
    use tauri::{Emitter, Manager};
    let action = ReminderChoice::parse(&choice)
        .ok_or_else(|| format!("unknown reminder choice: {choice}"))?;
    if let Some(w) = app.get_webview_window(REMINDER_LABEL) {
        let _ = w.hide();
    }
    let now = tauri_app_core::now_unix();
    match action {
        ReminderChoice::Open => {
            let _ = app.emit("daisy://reminder/open", ());
        }
        ReminderChoice::Dismiss => {}
        ReminderChoice::Start => {
            let handle = app.clone();
            tauri::async_runtime::spawn(async move {
                if let Err(e) = quick_start_recording(handle).await {
                    log::warn!("reminder start: {e:?}");
                }
            });
        }
        ReminderChoice::Stop => request_finish(&app),
        ReminderChoice::Snooze30m | ReminderChoice::Snooze1h | ReminderChoice::SnoozeToday => {
            let until = snooze_until(action, now).expect("snooze choices snooze");
            write_meeting_prompt(&app, |m| m.snooze_until = Some(until))?;
        }
        ReminderChoice::Never => {
            write_meeting_prompt(&app, |m| m.enabled = false)?;
        }
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
async fn sync_status(
    app_handle: tauri::AppHandle,
    app: tauri::State<'_, AppState>,
    vs: tauri::State<'_, VaultState>,
) -> Result<tauri_app_core::commands::sync::SyncStatus, AppError> {
    // Brings the listener down again after a pairing code expires unused, and
    // reports what a phone would actually find: a bound socket, a radio.
    let listening = tauri_app_core::sync::server::apply_sync_state(&app_handle);
    let bluetooth = tauri_app_core::sync::peripheral::radio_state(
        &app_handle.state::<tauri_app_core::sync::peripheral::PeripheralHandle>(),
    );
    tauri_app_core::commands::sync::sync_status_impl(&app, &vs, listening, bluetooth)
}

/// The Linux host-firewall command for Mobile Sync; `None` elsewhere.
#[tauri::command]
async fn linux_firewall_hint() -> Option<tauri_app_core::sync::firewall::FirewallHint> {
    tauri_app_core::sync::firewall::linux_firewall_hint()
}

#[tauri::command]
async fn sync_pairing_begin(
    app_handle: tauri::AppHandle,
    app: tauri::State<'_, AppState>,
    vs: tauri::State<'_, VaultState>,
) -> Result<tauri_app_core::commands::sync::PairingView, AppError> {
    use tauri_app_core::commands::sync::{pairing_host, sync_pairing_begin_impl, NO_NETWORK};
    let host = pairing_host().ok_or_else(|| AppError::Config(NO_NETWORK.into()))?;
    let view = sync_pairing_begin_impl(&app, &vs, host, tauri_app_core::now_unix())?;
    // Without a reachable listener, report the error and close the window.
    if !tauri_app_core::sync::server::apply_sync_state(&app_handle) {
        tauri_app_core::commands::sync::sync_pairing_cancel_impl(&app)?;
        return Err(AppError::Config(format!(
            "Port {} is in use by another program, so pairing can't start.",
            tauri_app_core::commands::sync::sync_port()
        )));
    }
    if let Err(e) = tauri_app_core::commands::sync::advertise_live_key(&app, &vs) {
        log::warn!("sync: pairing key not advertised: {e}");
    }
    let handle = app_handle.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(tauri_app_core::sync::pairing::KEY_SECS as u64 + 1)).await;
        tauri_app_core::sync::server::end_expired_key(&handle);
    });
    Ok(view)
}

#[tauri::command]
async fn sync_pairing_cancel(app_handle: tauri::AppHandle, app: tauri::State<'_, AppState>) -> Result<(), AppError> {
    tauri_app_core::commands::sync::sync_pairing_cancel_impl(&app)?;
    tauri_app_core::sync::server::apply_sync_state(&app_handle);
    Ok(())
}

#[tauri::command]
async fn sync_unpair(
    app_handle: tauri::AppHandle,
    app: tauri::State<'_, AppState>,
    vs: tauri::State<'_, VaultState>,
    device_id: String,
) -> Result<tauri_app_core::commands::sync::SyncStatus, AppError> {
    tauri_app_core::commands::sync::sync_unpair_impl(&app, &vs, &device_id, false, "off")?;
    let listening = tauri_app_core::sync::server::apply_sync_state(&app_handle);
    let bluetooth = tauri_app_core::sync::peripheral::radio_state(
        &app_handle.state::<tauri_app_core::sync::peripheral::PeripheralHandle>(),
    );
    tauri_app_core::commands::sync::sync_status_impl(&app, &vs, listening, bluetooth)
}

#[tauri::command]
async fn sync_rename_device(
    app_handle: tauri::AppHandle,
    app: tauri::State<'_, AppState>,
    vs: tauri::State<'_, VaultState>,
    device_id: String,
    name: String,
) -> Result<tauri_app_core::commands::sync::SyncStatus, AppError> {
    let listening = tauri_app_core::sync::server::apply_sync_state(&app_handle);
    let bluetooth = tauri_app_core::sync::peripheral::radio_state(
        &app_handle.state::<tauri_app_core::sync::peripheral::PeripheralHandle>(),
    );
    tauri_app_core::commands::sync::sync_rename_device_impl(&app, &vs, &device_id, &name, listening, bluetooth)
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
    rerender_and_notify(&app_handle, &state, &session_id);
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
    rerender_and_notify(&app_handle, &state, &session_id);
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
    rerender_and_notify(&app_handle, &state, &session_id);
    Ok(cluster_id)
}

/// The live speaker names of a stopped session whose transcript has no
/// speakers yet; `None` once speakers exist or when no overlay was kept.
#[tauri::command]
fn read_live_names(
    session_id: String,
    state: State<'_, AppState>,
    vault: State<'_, VaultState>,
) -> Option<tauri_app_core::live_names::display::UndiarizedNames> {
    tauri_app_core::live_names::display::read(&state, &session_id, &|| {
        let contacts = tauri_app_core::commands::contacts::load_contacts(&state).unwrap_or_default();
        let keys = vault.keys.lock().unwrap_or_else(|e| e.into_inner());
        keys.as_ref().map(|k| tauri_app_core::live_names::worker::known_voices(k, &contacts)).unwrap_or_default()
    })
}

#[tauri::command]
fn list_session_speakers(
    session_id: String,
    state: State<'_, AppState>,
) -> Result<Vec<SessionSpeaker>, AppError> {
    list_session_speakers_impl(&state, &session_id)
}

/// Re-render transcript.md and push an Updated library event — the shared
/// tail of every speaker/label mutation command.
fn rerender_and_notify(app_handle: &tauri::AppHandle, state: &AppState, session_id: &str) {
    let _ = tauri_app_core::commands::session::rerender_session_transcript_impl(state, session_id);
    tauri_app_core::library_events::emit(
        app_handle,
        tauri_app_core::library_events::LibraryChangeKind::Updated,
        session_id,
    );
}

#[tauri::command]
async fn session_speaker_sample_audio_bytes(
    session_id: String,
    cluster_id: u32,
    app_handle: tauri::AppHandle,
) -> Result<tauri::ipc::Response, AppError> {
    // Returns a binary IPC Response.
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
async fn split_session_at(
    session_id: String,
    cut_ms: u64,
    state: State<'_, AppState>,
    vault: State<'_, VaultState>,
    app_handle: tauri::AppHandle,
) -> Result<tauri_app_core::commands::session::SplitOutcome, AppError> {
    let out = tauri_app_core::commands::session::split_session_at_impl(&state, &session_id, cut_ms)?;
    for sid in [&session_id, &out.new_session_id] {
        tauri_app_core::commands::integrity::repair_local_now(&state, &vault, sid);
    }
    for sid in [&session_id, &out.new_session_id] {
        tauri_app_core::library_events::emit(
            &app_handle,
            tauri_app_core::library_events::LibraryChangeKind::Finalized,
            sid,
        );
    }
    Ok(out)
}

#[tauri::command]
async fn truncate_session_after(
    session_id: String,
    cut_ms: u64,
    state: State<'_, AppState>,
    vault: State<'_, VaultState>,
    app_handle: tauri::AppHandle,
) -> Result<tauri_app_core::commands::session::TruncateOutcome, AppError> {
    let out = tauri_app_core::commands::session::truncate_session_after_impl(
        &state,
        &session_id,
        cut_ms,
    )?;
    tauri_app_core::commands::integrity::repair_local_now(&state, &vault, &session_id);
    // The row's duration and summary badge both change.
    tauri_app_core::library_events::emit(
        &app_handle,
        tauri_app_core::library_events::LibraryChangeKind::Finalized,
        &session_id,
    );
    Ok(out)
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
        let emit_handle = app_handle.clone();
        run_on_os_thread(move || {
            let _ = tauri_app_core::commands::recording::teardown(ar, StopMode::Discard, || {
                let _ = emit_handle.emit::<Option<RecordingSnapshot>>("recording:snapshot", None);
            });
        })
        .await
        .map_err(AppError::Recording)?;
    }
    // Snapshot the manifest before removing the directory.
    let pre_delete_snap = tauri_app_core::commands::workflows::snapshot_for_session(
        &app_handle.state::<AppState>(),
        &session_id,
    )
    .ok();
    // Remove the directory on a blocking thread.
    let sid_for_emit = session_id.clone();
    let emit_handle = app_handle.clone();
    let result = tauri::async_runtime::spawn_blocking(move || {
        tauri_app_core::live_names::cancel(&session_id);
        tauri_app_core::live_diarize::cancel(&session_id);
        let state = app_handle.state::<AppState>();
        let vault = app_handle.state::<VaultState>();
        delete_session_impl(&state, &vault, &session_id)
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
/// at the top of import_audio_meeting; recording start has its own
/// features_enabled() check in start_recording_from. Every other command
/// runs regardless of license state.
fn ensure_licensed() -> Result<(), AppError> {
    if tauri_app_core::commands::license::features_enabled() {
        Ok(())
    } else {
        Err(AppError::LicenseExpired)
    }
}

// Async command.
#[tauri::command]
async fn start_recording(
    req: StartRequest,
    app_handle: tauri::AppHandle,
) -> Result<RecordingSnapshot, AppError> {
    start_recording_from(&app_handle, req)
}

// Async: pausing stops the capture units.
#[tauri::command]
async fn pause_recording(
    state: State<'_, AppState>,
    active: State<'_, ActiveRecordingHandle>,
    app_handle: tauri::AppHandle,
) -> Result<&'static str, AppError> {
    let r = pause_impl(&state, &active.0)?;
    if let Some(snap) = recording_snapshot_impl(&active.0) {
        let _ = app_handle.emit("recording:snapshot", &snap);
    }
    Ok(r)
}

/// Switch the recording mic mid-session (e.g. to AirPods). Fail-safe: recording
/// continues on the current mic if the new device can't start.
#[tauri::command]
async fn switch_recording_mic(
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

// Async: resuming reopens the capture units.
#[tauri::command]
async fn resume_recording(
    state: State<'_, AppState>,
    active: State<'_, ActiveRecordingHandle>,
    app_handle: tauri::AppHandle,
) -> Result<&'static str, AppError> {
    let r = resume_impl(&state, &active.0)?;
    if let Some(snap) = recording_snapshot_impl(&active.0) {
        let _ = app_handle.emit("recording:snapshot", &snap);
    }
    Ok(r)
}

/// Runs `f` on a plain OS thread (no tokio context) and awaits its join via `spawn_blocking`.
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

#[tauri::command]
async fn stop_recording(
    active: State<'_, ActiveRecordingHandle>,
    app_handle: tauri::AppHandle,
) -> Result<String, AppError> {
    let ar = {
        let mut slot = active.0.lock().unwrap();
        slot.take().ok_or(AppError::NotRecording)?
    };
    // Stops only the recorder (releases the mic/system devices and the heartbeat keeper). Finalize runs in-app via `recording_finalize_and_summarize`.
    let result: Result<String, AppError> = run_on_os_thread(move || {
        let final_root = tauri_app_core::commands::recording::teardown(ar, StopMode::Keep, || {
            let _ = app_handle.emit::<Option<RecordingSnapshot>>("recording:snapshot", None);
        })?;
        Ok(final_root.to_string_lossy().into_owned())
    })
    .await
    .map_err(AppError::Recording)?;
    result
}

/// Asks the main window to finish the active recording: it flushes the notes,
/// stops the recorder and runs the finalize cascade, exactly as the record
/// screen's Finish does. The main window is brought forward.
fn request_finish(app: &tauri::AppHandle) {
    use tauri::Emitter;
    let _ = tauri_app_core::mini_window::show_main(app);
    let _ = app.emit_to(MAIN_LABEL, "daisy://recording/finish", ());
}

/// Finish request from a window other than the record screen (the mini
/// window's Stop).
#[tauri::command]
fn finish_recording(
    active: State<'_, ActiveRecordingHandle>,
    app_handle: tauri::AppHandle,
) -> Result<(), AppError> {
    if active.0.lock().unwrap().is_none() {
        return Err(AppError::NotRecording);
    }
    request_finish(&app_handle);
    Ok(())
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
        // Stop the recorder before deleting the directory.
        let _ = tauri_app_core::commands::recording::teardown(ar, StopMode::Discard, || {
            if root.is_dir() {
                let _ = std::fs::remove_dir_all(&root);
            }
            let _ = app_handle.emit::<Option<RecordingSnapshot>>("recording:snapshot", None);
        });
    })
    .await
    .map_err(AppError::Recording)?;
    Ok(())
}

#[tauri::command]
fn recording_snapshot(active: State<'_, ActiveRecordingHandle>) -> Option<RecordingSnapshot> {
    recording_snapshot_impl(&active.0)
}

/// The title a new recording carries when nobody typed one.
#[tauri::command]
fn default_recording_title(style: State<'_, ClockStyle>) -> String {
    let hour12 = style.0.load(std::sync::atomic::Ordering::Relaxed);
    tauri_app_core::commands::recording::default_recording_title(chrono::Local::now(), hour12)
}

/// Sets the machine's clock-cycle preference (12-hour vs 24-hour) for the
/// default recording title, read from the OS locale at frontend startup.
#[tauri::command]
fn set_clock_hour12(hour12: bool, style: State<'_, ClockStyle>) {
    style.0.store(hour12, std::sync::atomic::Ordering::Relaxed);
}

#[tauri::command]
async fn transcribe(
    req: TranscribeRequest,
    state: State<'_, AppState>,
) -> Result<usize, AppError> {
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

/// One final live-caption segment for the post-call "live while finalizing"
/// view.
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
    let sid = req.session_id.clone();
    let sid_for_emit = sid.clone();
    let emit_handle = app_handle.clone();
    let lib_emit_handle = app_handle.clone();
    let result = run_on_os_thread(move || {
        let state = app_handle.state::<AppState>();
        let vault = app_handle.state::<VaultState>();
        tauri_app_core::commands::finalize::finalize_and_summarize_impl(&state, &vault, req, |ev| {
            // Emit Updated when the summarize stage starts.
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
        let has_provider = state.ai_provider().is_some() && vault.is_unlocked();
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
    state
        .update_settings(|s| s.default_summary_prompt_id = Some(id))
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
    app_handle: tauri::AppHandle,
) -> Result<DeleteTagResult, AppError> {
    let result = delete_tag_impl(&state, &id, force)?;
    if result.dangling_session_count > 0 {
        tauri_app_core::library_events::emit(
            &app_handle,
            tauri_app_core::library_events::LibraryChangeKind::Updated,
            "",
        );
    }
    Ok(result)
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
    vault: State<'_, VaultState>,
    app_handle: tauri::AppHandle,
) -> Result<(), AppError> {
    tauri_app_core::commands::voiceprints::detach_speaker_voiceprint_impl(
        &state,
        &vault,
        &session_id,
        cluster_id,
    )?;
    rerender_and_notify(&app_handle, &state, &session_id);
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

// ---- AI job registry ----
#[tauri::command]
fn ai_jobs_list(state: State<'_, AiJobs>) -> Vec<AiJob> {
    state.list()
}

// ---- topic chapters ----
#[tauri::command]
async fn extract_session_chapters(
    req: tauri_app_core::commands::chapters::ChaptersRequest,
    app_handle: tauri::AppHandle,
) -> Result<tauri_app_core::commands::chapters::ChaptersResult, AppError> {
    let _job = app_handle.state::<AiJobs>().begin(
        &app_handle,
        req.session_id.clone(),
        AiJobKind::Chapters,
    )?;
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
    // A finalize still running is left alone.
    if tauri_app_core::commands::finalize::finalize_running(&session_id) {
        return Ok(());
    }
    let dir = {
        let state = app_handle.state::<AppState>();
        state.profile.session_path(&session_id)
    };
    tauri_app_core::commands::finalize::clear_finalize_recovery(&dir);
    // Replace the stale error sidecar before the thread starts.
    tauri_app_core::commands::finalize::write_finalize_status(
        &dir,
        "finalizing",
        0.05,
        Some("retrying"),
    );
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

/// The speaker-encoder model's state (ready, missing or unreadable), for the voice-grouping banner.
#[tauri::command]
async fn voice_model_status() -> tauri_app_core::commands::voiceprints::VoiceModelStatus {
    tauri_app_core::commands::voiceprints::voice_model_status_impl()
}

/// The attendee count a diarize run on `track` would rest on, for the count
/// field's placeholder.
#[tauri::command]
async fn diarize_attendee_count(
    session_id: String,
    track: Option<tauri_app_core::commands::voiceprints::DiarizeScope>,
    app_handle: tauri::AppHandle,
) -> Result<Option<u32>, AppError> {
    tauri::async_runtime::spawn_blocking(move || {
        tauri_app_core::commands::voiceprints::attendee_count_hint_impl(
            &app_handle.state::<AppState>(),
            &session_id,
            track,
        )
    })
    .await
    .map_err(|e| AppError::Io(e.to_string()))?
}

/// How the session's last diarization run settled its speaker count.
#[tauri::command]
async fn diarize_count(
    session_id: String,
    app_handle: tauri::AppHandle,
) -> Result<Option<recording::manifest::DiarizeCount>, AppError> {
    tauri::async_runtime::spawn_blocking(move || {
        tauri_app_core::commands::voiceprints::diarize_count_impl(&app_handle.state::<AppState>(), &session_id)
    })
    .await
    .map_err(|e| AppError::Io(e.to_string()))?
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
    let sid_for_emit = session_id.clone();
    let emit_handle = app_handle.clone();
    let res = tauri::async_runtime::spawn_blocking(move || {
        let state = app_handle.state::<AppState>();
        let vault = app_handle.state::<VaultState>();
        let gallery = vault.keys.lock().unwrap().as_ref().map(|k| k.voiceprints.clone()).unwrap_or_default();
        let result = tauri_app_core::commands::voiceprints::diarize_session_impl(
            &state,
            &session_id,
            expected_speakers,
            track,
            &gallery,
        )?;
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

/// Streaming Q&A: pushes each answer token through `on_token`, returns the full
/// answer + citations when done. Non-streaming providers send the whole answer
/// as one token.
#[tauri::command]
async fn qa_ask_stream(
    req: tauri_app_core::commands::qa::QaAskRequest,
    on_token: tauri::ipc::Channel<String>,
    app_handle: tauri::AppHandle,
) -> Result<tauri_app_core::commands::qa::QaAnswer, AppError> {
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

/// Streaming in-call chat: pushes each reply token through `on_token` as it
/// arrives, then returns the persisted thread. Providers without SSE support
/// send the whole reply as one token.
#[tauri::command]
async fn live_chat_send_stream(
    req: tauri_app_core::commands::live_chat::LiveChatSendRequest,
    on_token: tauri::ipc::Channel<String>,
    app_handle: tauri::AppHandle,
) -> Result<tauri_app_core::commands::live_chat::LiveChatReply, AppError> {
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
        let vault = impl_handle.state::<VaultState>();
        let picks = impl_handle.state::<tauri_app_core::commands::files::DialogPicks>();
        tauri_app_core::commands::meeting::import_audio_meeting_impl(&state, &vault, &picks, req)
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

/// Opens the OS audio picker and remembers the chosen file for
/// `import_audio_meeting`. Returns the chosen path, or `None` when cancelled.
#[tauri::command]
async fn pick_import_audio(
    app_handle: tauri::AppHandle,
    vault: State<'_, VaultState>,
) -> Result<Option<String>, AppError> {
    tauri_app_core::commands::files::require_unlocked(&vault)?;
    tauri::async_runtime::spawn_blocking(move || {
        use tauri_plugin_dialog::DialogExt;
        let picked = app_handle
            .dialog()
            .file()
            .add_filter("Audio", &["mp3", "m4a", "aac", "wav", "flac", "ogg", "opus"])
            .blocking_pick_file()
            .and_then(|p| p.into_path().ok());
        Ok(picked.map(|path| {
            let shown = path.to_string_lossy().into_owned();
            app_handle
                .state::<tauri_app_core::commands::files::DialogPicks>()
                .record_audio(path);
            shown
        }))
    })
    .await
    .map_err(|e| AppError::Config(format!("pick task failed: {e}")))?
}

/// UI-test builds only: records `path` as the audio file the picker returned,
/// standing in for the OS dialog the native suite cannot drive.
#[cfg(feature = "ui-test")]
#[tauri::command]
fn ui_test_record_audio_pick(
    path: PathBuf,
    picks: State<'_, tauri_app_core::commands::files::DialogPicks>,
) {
    picks.record_audio(path);
}

/// Opens the OS save dialog and writes `contents` to the chosen file.
/// Returns the chosen path, or `None` when cancelled.
#[tauri::command]
async fn save_text_file(
    title: String,
    default_file_name: String,
    filter_name: String,
    extensions: Vec<String>,
    contents: String,
    app_handle: tauri::AppHandle,
    vault: State<'_, VaultState>,
) -> Result<Option<String>, AppError> {
    tauri_app_core::commands::files::require_unlocked(&vault)?;
    tauri::async_runtime::spawn_blocking(move || {
        use tauri_plugin_dialog::DialogExt;
        let exts: Vec<&str> = extensions.iter().map(String::as_str).collect();
        let Some(path) = app_handle
            .dialog()
            .file()
            .set_title(title)
            .set_file_name(default_file_name)
            .add_filter(filter_name, &exts)
            .blocking_save_file()
            .and_then(|p| p.into_path().ok())
        else {
            return Ok(None);
        };
        tauri_app_core::commands::files::save_text_file_impl(&path, &contents)?;
        Ok(Some(path.to_string_lossy().into_owned()))
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

// ---- tag suggestions ----
#[tauri::command]
fn tag_suggest_prerecord(
    title: Option<String>,
    calendar: Option<recording::manifest::CalendarLink>,
    state: State<'_, AppState>,
) -> Result<Option<LastTimeSuggestion>, AppError> {
    tag_suggest_prerecord_impl(&state, title, calendar)
}
#[tauri::command]
fn tag_suggest_for_session(
    session_id: String,
    state: State<'_, AppState>,
) -> Result<SessionSuggestions, AppError> {
    tag_suggest_for_session_impl(&state, &session_id)
}
#[tauri::command]
fn session_dismiss_tag_suggestions(
    session_id: String,
    tag_ids: Vec<String>,
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), AppError> {
    session_dismiss_tag_suggestions_impl(&state, &session_id, tag_ids)?;
    tauri_app_core::library_events::emit(
        &app_handle,
        tauri_app_core::library_events::LibraryChangeKind::Updated,
        &session_id,
    );
    Ok(())
}
#[tauri::command]
fn session_undismiss_tag_suggestions(
    session_id: String,
    tag_ids: Vec<String>,
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), AppError> {
    session_undismiss_tag_suggestions_impl(&state, &session_id, tag_ids)?;
    tauri_app_core::library_events::emit(
        &app_handle,
        tauri_app_core::library_events::LibraryChangeKind::Updated,
        &session_id,
    );
    Ok(())
}
/// Atomic accept for the last-time chip: applies `tag_ids` (the full desired
/// tag set) and dismisses `dismiss_tag_ids` in one manifest write.
#[tauri::command]
fn session_apply_tag_suggestions(
    session_id: String,
    tag_ids: Vec<String>,
    dismiss_tag_ids: Vec<String>,
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), AppError> {
    session_apply_tag_suggestions_impl(&state, &session_id, tag_ids, dismiss_tag_ids)?;
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
    let _job =
        app_handle
            .state::<AiJobs>()
            .begin(&app_handle, session_id.clone(), AiJobKind::Summary)?;
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
fn bootstrap_set(
    profile_dir: PathBuf,
    picks: State<'_, tauri_app_core::commands::files::DialogPicks>,
) -> Result<(), AppError> {
    tauri_app_core::commands::bootstrap::bootstrap_set_picked_impl(&picks, profile_dir)
}

/// Opens the OS folder picker for `purpose` and remembers the choice for the
/// command that serves that purpose. Returns the chosen path, or `None` when
/// cancelled. Before unlock only the profile-selection purposes are served.
#[tauri::command]
async fn pick_directory(
    purpose: String,
    app_handle: tauri::AppHandle,
    vault: State<'_, VaultState>,
) -> Result<Option<String>, AppError> {
    tauri_app_core::commands::files::validate_dir_pick(&vault, &purpose)?;
    tauri::async_runtime::spawn_blocking(move || {
        use tauri_plugin_dialog::DialogExt;
        let picked = app_handle
            .dialog()
            .file()
            .blocking_pick_folder()
            .and_then(|p| p.into_path().ok());
        Ok(picked.map(|path| {
            let shown = path.to_string_lossy().into_owned();
            app_handle
                .state::<tauri_app_core::commands::files::DialogPicks>()
                .record_dir(&purpose, path);
            shown
        }))
    })
    .await
    .map_err(|e| AppError::Config(format!("pick task failed: {e}")))?
}

/// Spawns the platform's default-browser launcher for `url`: one direct
/// arg, no shell.
fn open_url(url: &str) -> std::io::Result<()> {
    #[cfg(target_os = "windows")]
    let r = std::process::Command::new("rundll32")
        .args(["url.dll,FileProtocolHandler", url])
        .spawn();
    #[cfg(target_os = "macos")]
    let r = std::process::Command::new("open").arg(url).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let r = std::process::Command::new("xdg-open").arg(url).spawn();
    r.map(|_| ())
}

/// Reveals `dir` in the OS file browser: one literal arg, no shell.
fn reveal_path(dir: &std::path::Path) -> std::io::Result<()> {
    #[cfg(target_os = "windows")]
    let r = std::process::Command::new("explorer").arg(dir).spawn();
    #[cfg(target_os = "macos")]
    let r = std::process::Command::new("open").arg(dir).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let r = std::process::Command::new("xdg-open").arg(dir).spawn();
    r.map(|_| ())
}

// Opens an http(s) URL in the user's default browser. http/https only; the
// URL is passed as one direct arg, no shell.
#[tauri::command]
fn open_external(url: String) -> Result<(), AppError> {
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err(AppError::Config("only http(s) URLs may be opened".into()));
    }
    open_url(&url).map_err(|e| AppError::Config(format!("open browser: {e}")))
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
    reveal_path(&dir)
        .map(|_| s)
        .map_err(|e| AppError::Config(format!("open logs dir: {e}")))
}

/// Reveals the profile directory in the OS file browser. Same shape as
/// `open_logs_dir`: the path comes from app state, one literal arg.
#[tauri::command]
fn open_profile_dir(state: State<'_, AppState>) -> Result<String, AppError> {
    let dir = state.profile.root().to_path_buf();
    let s = dir.to_string_lossy().to_string();
    reveal_path(&dir)
        .map(|_| s)
        .map_err(|e| AppError::Config(format!("open profile dir: {e}")))
}

#[tauri::command]
fn profile_binding_check(state: State<'_, AppState>) -> Result<tauri_app_core::commands::binding::BindingState, AppError> {
    tauri_app_core::commands::binding::profile_binding_check_impl(&state)
}

#[tauri::command]
fn license_status() -> Result<tauri_app_core::commands::license::LicenseStatus, AppError> {
    Ok(tauri_app_core::commands::license::license_status_impl()?)
}
#[tauri::command]
async fn activate_license(
    key: String,
    state: State<'_, AppState>,
    vault: State<'_, VaultState>,
) -> Result<tauri_app_core::commands::license::LicenseStatus, AppError> {
    // The install's Ed25519 public key is derived from the vault seed and
    // registered at activation. None when the vault is locked; registration
    // then happens when the provider is enabled.
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

/// The Stripe Checkout URL for Perpetual replacing this device's
/// subscription key; the frontend opens it in the browser.
#[tauri::command]
async fn license_upgrade_checkout() -> Result<String, AppError> {
    tauri::async_runtime::spawn_blocking(
        tauri_app_core::commands::license::upgrade_checkout_impl,
    )
    .await
    .map_err(|e| AppError::Config(format!("license upgrade task failed: {e}")))?
}

/// Sets the profile display name to the licensee's first name whenever a
/// valid license is present. The name is a signed claim in the license
/// token. Idempotent + best-effort; runs on activation and at startup.
fn pin_profile_name_to_license(state: &AppState) {
    use tauri_app_core::commands::license::{license_status_impl, LicenseStatus};
    let Ok(LicenseStatus::Licensed { name, .. }) = license_status_impl() else { return };
    let Some(first) = name.split_whitespace().next() else { return };
    if state.settings().user_display_name.as_deref() != Some(first) {
        let _ = state.update_settings(|s| s.user_display_name = Some(first.to_string()));
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

/// Sets the AI features provider on this machine; `None` is none. The
/// built-in model is refused unless this computer can run it and it is
/// installed.
#[tauri::command]
async fn set_ai_provider(
    provider: Option<tauri_app_core::state::ProviderId>,
    app: tauri::AppHandle,
) -> Result<(), AppError> {
    let task = app.clone();
    tokio::task::spawn_blocking(move || {
        let state = task.state::<AppState>();
        match provider {
            Some(tauri_app_core::state::ProviderId::Local) => tauri_app_core::builtin_model::select(&state),
            p => state.set_ai_provider(p).map_err(|e| AppError::Config(format!("save settings: {e}"))),
        }
    })
    .await
    .map_err(|e| AppError::Io(format!("provider task join: {e}")))??;
    if provider == Some(tauri_app_core::state::ProviderId::Local) {
        tauri_app_core::builtin_model::on_selected(&app);
    }
    tauri_app_core::builtin_model::emit(&app);
    Ok(())
}

/// What first-run setup will do on this machine: AI, transcription, and where
/// recordings land.
#[tauri::command]
async fn onboarding_plan(
    app: tauri::AppHandle,
) -> Result<tauri_app_core::commands::onboarding::OnboardingPlan, AppError> {
    tokio::task::spawn_blocking(move || {
        tauri_app_core::commands::onboarding::onboarding_plan_impl(&app.state::<AppState>())
    })
    .await
    .map_err(|e| AppError::Io(format!("onboarding plan task join: {e}")))
}

/// Applies the plan — selects the engines and queues what is missing — and
/// returns it. Safe to call again.
#[tauri::command]
async fn onboarding_apply(
    app: tauri::AppHandle,
) -> Result<tauri_app_core::commands::onboarding::OnboardingPlan, AppError> {
    let task = app.clone();
    let plan = tokio::task::spawn_blocking(move || {
        tauri_app_core::commands::onboarding::onboarding_apply_impl(&task.state::<AppState>())
    })
    .await
    .map_err(|e| AppError::Io(format!("onboarding apply task join: {e}")))?;
    tauri_app_core::commands::asr_models::emit(&app);
    plan
}

/// Marks first-run setup finished. Called once, when the wizard's finish
/// button is pressed.
#[tauri::command]
fn onboarding_finish() -> Result<(), AppError> {
    tauri_app_core::commands::onboarding::onboarding_finish_impl()
}

#[tauri::command]
fn list_css_themes(state: State<'_, AppState>) -> Result<Vec<String>, AppError> {
    tauri_app_core::commands::settings::list_css_themes_impl(&state)
}

#[tauri::command]
fn read_css_theme(name: String, state: State<'_, AppState>) -> Result<String, AppError> {
    tauri_app_core::commands::settings::read_css_theme_impl(&state, &name)
}

/// Whether translucency is off on this machine (macOS Reduce Transparency, Linux). The frontend reads it at startup and whenever the window takes focus.
#[tauri::command]
fn reduce_transparency() -> bool {
    tauri_app_core::hardware::reduce_transparency()
}

#[tauri::command]
fn machine_settings(state: State<'_, AppState>) -> MachineSettingsStatus {
    tauri_app_core::commands::settings::machine_settings_status(&state)
}

/// Merges the fields `patch` carries into this machine's settings block and
/// returns the block as saved. Every window hears the result as
/// `machine-settings:changed`.
#[tauri::command]
fn update_machine_settings(
    patch: MachinePatch,
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<MachineSettingsStatus, AppError> {
    use tauri_app_core::commands::settings::apply_machine_patch;
    let before = state.machine_settings().record_hotkey;
    let hotkey = patch.record_hotkey.clone().filter(|h| *h != before);
    let tray = patch.show_tray_icon;
    apply_machine_patch(&state, patch).map_err(|e| AppError::Config(format!("save settings: {e}")))?;
    if let Some(show) = tray {
        if let Err(e) = set_tray_visible(&app_handle, show) {
            log::warn!("system tray: {e}");
        }
    }
    if let Some(hotkey) = hotkey {
        if let Err(e) = tauri_app_core::hotkey::apply(&app_handle, hotkey.as_deref()) {
            // The write already landed; a failed re-registration rolls the
            // stored value back, then the registration it names.
            let restore = MachinePatch { record_hotkey: Some(before.clone()), ..Default::default() };
            if let Err(e2) = apply_machine_patch(&state, restore) {
                log::warn!("record hotkey: restore setting failed: {e2}");
            }
            if let Err(e2) = tauri_app_core::hotkey::apply(&app_handle, before.as_deref()) {
                log::warn!("record hotkey: restore registration failed: {e2}");
            }
            return Err(e);
        }
    }
    Ok(tauri_app_core::commands::settings::machine_settings_status(&state))
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

#[tauri::command]
async fn unlock_vault(
    passphrase: String,
    app_handle: tauri::AppHandle,
    state: State<'_, AppState>,
    vault: State<'_, VaultState>,
) -> Result<(), AppError> {
    unlock_vault_impl(&state, &vault, &passphrase)?;
    vault_opened(&app_handle);
    Ok(())
}

#[tauri::command]
async fn init_vault_machine_mode(
    app_handle: tauri::AppHandle,
    state: State<'_, AppState>,
    vault: State<'_, VaultState>,
) -> Result<(), AppError> {
    init_vault_machine_mode_impl(&state, &vault)?;
    vault_opened(&app_handle);
    Ok(())
}

/// Returns the vault kind: "passphrase" or "machine" (or "passphrase" when
/// missing). Frontend uses this to know whether to show the Unlock screen.
#[tauri::command]
fn vault_kind(state: State<'_, AppState>) -> Result<String, AppError> {
    Ok(read_vault_kind(&state))
}

/// The vault kind plus, in machine mode, the computer whose key opens it and
/// this computer's name. The unlock screen reads it.
#[tauri::command]
fn vault_trust(state: State<'_, AppState>) -> Result<VaultTrust, AppError> {
    Ok(read_vault_trust(&state))
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
fn read_settings(state: State<'_, AppState>) -> Settings {
    state.settings()
}

/// Merges the fields `patch` carries into the profile's settings and returns
/// them as saved. Every window hears the result as `settings:changed`.
#[tauri::command]
fn update_settings(patch: SettingsPatch, state: State<'_, AppState>) -> Result<Settings, AppError> {
    update_settings_impl(&state, patch)
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
    // The meter loop runs on a dedicated OS thread.
    std::thread::spawn(move || {
        let app_for_cb = app.clone();
        let event_for_cb = event_name.clone();
        let rid_for_cb = rid.clone();
        // Per-second callback rate and peak, logged at debug while the meter runs.
        let mut win_start = std::time::Instant::now();
        let mut win_count: u32 = 0;
        let mut win_peak: f32 = 0.0;
        // Peak-hold throttle: emits the held max every ~66ms.
        let mut emit_start = std::time::Instant::now();
        let mut emit_peak: f32 = 0.0;
        let on_rms = move |rms: f32| {
            win_count += 1;
            win_peak = win_peak.max(rms);
            if win_start.elapsed() >= std::time::Duration::from_secs(1) {
                log::debug!("mic meter: {win_count} cb/s, peak {win_peak:.4}");
                win_start = std::time::Instant::now();
                win_count = 0;
                win_peak = 0.0;
            }
            emit_peak = emit_peak.max(rms);
            if emit_start.elapsed() < std::time::Duration::from_millis(66) {
                return;
            }
            let payload = MicLevelPayload {
                request_id: rid_for_cb.clone(),
                rms: emit_peak,
            };
            emit_start = std::time::Instant::now();
            emit_peak = 0.0;
            // Best-effort: with the window gone, the emit fails harmlessly.
            let _ = app_for_cb.emit(&event_for_cb, payload);
        };
        // A Bluetooth mic on A2DP has no capture; flip its card and meter the
        // renumbered node. The profile is restored when the meter stops.
        let (source_id, bt_flip) = match audio_engine::source::list_sources()
            .ok()
            .and_then(|v| v.into_iter().find(|s| s.id == source_id))
        {
            Some(src) => match audio_engine::bt_profile::prepare_capture(
                &src.node_name,
                tauri_app_core::bt_prefs::prefers_cvsd_for(
                    app.state::<AppState>().profile.root(),
                    &src.node_name,
                ),
            ) {
                Ok(Some((flip, resolved, _))) => (resolved.id, Some(flip)),
                Ok(None) => (source_id, None),
                Err(e) => {
                    log::warn!("mic meter: BT profile flip failed: {e}");
                    (source_id, None)
                }
            },
            None => (source_id, None),
        };
        match audio_engine::capture::run_level_meter(source_id, on_rms, stop_rx) {
            Ok(()) => log::info!("mic meter: stopped (source_id={source_id})"),
            Err(e) => {
                log::warn!("mic meter: source {source_id} FAILED to start: {e}");
                // A start failure is surfaced as a `mic-meter-error:<id>` event; the frontend listens and retries. The source list is pushed first.
                tauri_app_core::commands::settings::audio_sources_changed(&app);
                let _ = app.emit(&format!("mic-meter-error:{rid}"), rid.clone());
            }
        }
        if let Some(flip) = bt_flip {
            audio_engine::bt_profile::restore_profile(&flip);
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
    let profile_root_for_capture = profile_root.clone();
    let captured = tauri::async_runtime::spawn_blocking(move || {
        let sources = audio_engine::source::list_sources()
            .map_err(|e| AppError::Recording(format!("list sources: {e}")))?;
        let source = sources
            .into_iter()
            .find(|s| s.id == source_id)
            .ok_or_else(|| AppError::Recording(format!("mic source {source_id} not found")))?;
        // A BT mic parked on A2DP records zeros; flip for the calibration
        // and restore after.
        let prefer_cvsd =
            tauri_app_core::bt_prefs::prefers_cvsd_for(&profile_root_for_capture, &source.node_name);
        let (source, bt_flip) = match audio_engine::bt_profile::prepare_capture(&source.node_name, prefer_cvsd)
        {
            Ok(Some((flip, resolved, _))) => (resolved, Some(flip)),
            Ok(None) => (source, None),
            Err(e) => return Err(AppError::Recording(e.to_string())),
        };
        let result = audio_engine::capture::capture_one(
            &source,
            std::time::Duration::from_secs(CALIBRATE_SECS),
            &capture_path,
        )
        .map_err(|e| AppError::Recording(format!("calibration capture: {e}")));
        if let Some(flip) = bt_flip {
            audio_engine::bt_profile::restore_profile(&flip);
        }
        result
    })
    .await
    .map_err(|e| AppError::Config(format!("calibration task failed: {e}")))?;
    // The clip is deleted on every path.
    let loaded = captured.and_then(|voice_processed| {
        transcript::rms::WavSamples::load(&wav_path)
            .map(|wav| (wav, voice_processed))
            .map_err(|e| AppError::Config(format!("read calibration clip: {e}")))
    });
    let _ = syncsafe::remove_file(&wav_path);
    let (wav, voice_processed) = loaded?;
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
        // The one-shot capture runs the same start ladder a recording does,
        // and reports which domain it landed in.
        voice_processed,
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
            // Whichever capture domain this device has samples in.
            let eff = store
                .effective(dev, true)
                .or_else(|| store.effective(dev, false));
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
        // The chat endpoint is probed as well.
        providers_http::probe_chat_path(provider.as_str(), base.as_deref(), key.as_deref())?;
        Ok(models)
    })
    .await
    .map_err(AppError::Provider)?;
    result
}

/// The context length a local server has loaded `model` with, to prefill
/// the provider's context-length field. LM Studio reports it on its native
/// REST API; every other provider answers `None`. `base_url` falls back to
/// the vault entry, then the provider default.
#[tauri::command]
async fn probe_context_length(
    provider: tauri_app_core::state::ProviderId,
    base_url: Option<String>,
    model: String,
    vault: State<'_, VaultState>,
) -> Result<Option<u32>, AppError> {
    if provider != tauri_app_core::state::ProviderId::LmStudio {
        return Ok(None);
    }
    let vault_base: Option<String> = {
        let guard = vault.keys.lock().unwrap();
        guard.as_ref().and_then(|k| k.providers.get(&provider).and_then(|c| c.base_url.clone()))
    };
    let nonempty = |s: String| if s.is_empty() { None } else { Some(s) };
    let base = base_url.and_then(nonempty).or_else(|| vault_base.and_then(nonempty));
    run_on_os_thread(move || {
        Ok(providers_http::lm_studio_loaded_context(base.as_deref(), &model)?)
    })
    .await
    .map_err(AppError::Provider)?
}

/// Saves a provider's key and options; the AI provider status every gate
/// reads follows as `builtin-model:changed`.
#[tauri::command]
async fn set_provider(
    provider: tauri_app_core::state::ProviderId,
    config: ProviderConfig,
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    vault: State<'_, VaultState>,
) -> Result<(), AppError> {
    set_provider_impl(&state, &vault, provider, config)?;
    tauri_app_core::builtin_model::emit(&app);
    Ok(())
}

/// Generate the install keypair if needed (vault) and register its public
/// key with the license server (re-activation).
#[tauri::command]
async fn register_gateway(
    state: State<'_, AppState>,
    vault: State<'_, VaultState>,
) -> Result<(), AppError> {
    tauri_app_core::commands::lifecycle::register_gateway_impl(&state, &vault)
}

#[tauri::command]
async fn lock_vault(app_handle: tauri::AppHandle) -> Result<(), AppError> {
    vault_closed(&app_handle);
    Ok(())
}

#[tauri::command]
async fn reset_vault(
    app_handle: tauri::AppHandle,
    state: State<'_, AppState>,
    vault: State<'_, VaultState>,
) -> Result<(), AppError> {
    reset_vault_impl(&state, &vault)?;
    vault_closed(&app_handle);
    Ok(())
}

#[tauri::command]
async fn move_profile(
    new_path: PathBuf,
    app_handle: tauri::AppHandle,
    vault: State<'_, VaultState>,
) -> Result<(), AppError> {
    tauri_app_core::commands::files::require_unlocked(&vault)?;
    let handle = app_handle.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let state = app_handle.state::<AppState>();
        let picks = app_handle.state::<tauri_app_core::commands::files::DialogPicks>();
        move_profile_picked_impl(&picks, state.profile.root(), &new_path)
    })
    .await
    .map_err(|e| AppError::Config(format!("move profile task failed: {e}")))??;
    // Restart into the new profile root.
    handle.restart();
}

#[tauri::command]
fn probe_profile_dir(
    path: PathBuf,
    state: State<'_, AppState>,
    picks: State<'_, tauri_app_core::commands::files::DialogPicks>,
) -> Result<ProfileProbe, AppError> {
    probe_profile_dir_picked_impl(&picks, state.profile.root(), &path)
}

/// Point the bootstrap at `path` and restart into it. The target keeps (or
/// gets) its own vault/settings; the current profile is left untouched.
#[tauri::command]
fn switch_profile(
    path: PathBuf,
    app_handle: tauri::AppHandle,
    state: State<'_, AppState>,
    active: State<'_, ActiveRecordingHandle>,
    picks: State<'_, tauri_app_core::commands::files::DialogPicks>,
) -> Result<(), AppError> {
    if recording_snapshot_impl(&active.0).is_some() {
        return Err(AppError::Config(
            "a recording is in progress — stop it before switching profiles".into(),
        ));
    }
    let probe = probe_profile_dir_picked_impl(&picks, state.profile.root(), &path)?;
    if probe.is_current {
        return Err(AppError::Config(
            "that is already the current profile directory".into(),
        ));
    }
    bootstrap_set_impl(path)?;
    picks.clear_dir(tauri_app_core::commands::files::PURPOSE_PROFILE_SWITCH);
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
/// bridge; macOS and Windows wrap them in a Blob URL, and Linux plays the file
/// URL from `session_playback_audio_url`. The impl requires an unlocked vault
/// and validates the session id — see playback.rs.
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

/// The `file://` URL of the session's meeting.opus on Linux, `None` on other
/// platforms. Requires an unlocked vault and resolves the path under the
/// sessions root, as `session_playback_audio_bytes` does.
#[tauri::command]
async fn session_playback_audio_url(
    session_id: String,
    app_handle: tauri::AppHandle,
) -> Result<Option<String>, AppError> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app_handle.state::<AppState>();
        let vault = app_handle.state::<VaultState>();
        tauri_app_core::commands::playback::session_playback_audio_url_impl(
            &state,
            &vault,
            &session_id,
        )
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
    limit: usize,
    skip: usize,
    state: State<'_, AppState>,
) -> Result<Vec<HistoryEntry>, AppError> {
    read_history(&state, limit.min(500), skip)
}

// ---- workflows ----

/// Snapshot + dispatch + worker wake for the explicit trigger sites. Best-effort.
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
    vault: State<'_, VaultState>,
    picks: State<'_, tauri_app_core::commands::files::DialogPicks>,
) -> Result<tauri_app_core::commands::workflows::Workflow, AppError> {
    tauri_app_core::commands::files::require_unlocked(&vault)?;
    tauri_app_core::commands::workflows::upsert_workflow_picked_impl(&state, &picks, req)
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

const TRAY_ID: &str = "daisy-tray";

/// Shows the tray icon when this machine's `show_tray_icon` is on, and keeps
/// its tooltip in sync with the recording snapshot: an object carrying
/// `state` while a recording runs, `null` once it ends. On Linux, tray
/// support requires a StatusNotifier host (GNOME needs the AppIndicator
/// extension); with none present, the app runs without a visible tray.
fn setup_tray(app: &tauri::AppHandle) -> tauri::Result<()> {
    use tauri::{Listener, Manager};
    use tauri_app_core::mini_window::tray_status_text;

    let app_for_tray = app.clone();
    app.listen("recording:snapshot", move |event| {
        let snap: serde_json::Value = serde_json::from_str(event.payload()).unwrap_or_default();
        let state = snap.get("state").and_then(|s| s.as_str()).unwrap_or("idle");
        if let Some(tray) = app_for_tray.tray_by_id(TRAY_ID) {
            let _ = tray.set_tooltip(Some(tray_status_text(state)));
        }
    });
    let show = app.try_state::<AppState>().is_none_or(|s| s.machine_settings().show_tray_icon);
    set_tray_visible(app, show)
}

/// Builds the tray icon + menu (Show Daisy / Quit), or removes it. The macOS
/// menu bar gets a light greyscale rendering of the logo; the other
/// platforms use the color app icon.
fn set_tray_visible(app: &tauri::AppHandle, show: bool) -> tauri::Result<()> {
    use tauri::menu::{MenuBuilder, MenuItemBuilder};
    use tauri::tray::TrayIconBuilder;
    use tauri::Manager;
    use tauri_app_core::mini_window::tray_status_text;

    if !show {
        app.remove_tray_by_id(TRAY_ID);
        return Ok(());
    }
    if app.tray_by_id(TRAY_ID).is_some() {
        return Ok(());
    }
    let open = MenuItemBuilder::with_id("tray_show", "Show Daisy").build(app)?;
    let quit = MenuItemBuilder::with_id("tray_quit", "Quit").build(app)?;
    let menu = MenuBuilder::new(app).items(&[&open, &quit]).build()?;

    let state = app
        .try_state::<ActiveRecordingHandle>()
        .and_then(|a| recording_snapshot_impl(&a.0))
        .map_or("idle", |s| s.state);
    let mut builder = TrayIconBuilder::with_id(TRAY_ID)
        .tooltip(tray_status_text(state))
        .menu(&menu)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "tray_show" => {
                let _ = tauri_app_core::mini_window::show_main(app);
            }
            "tray_quit" => app.exit(0),
            _ => {}
        });
    #[cfg(target_os = "macos")]
    {
        builder = builder.icon(tauri::include_image!("icons/tray-mac.png"));
    }
    #[cfg(not(target_os = "macos"))]
    {
        if let Some(icon) = app.default_window_icon().cloned() {
            builder = builder.icon(icon);
        }
    }
    builder.build(app)?;
    Ok(())
}

/// Linux only: enables navigator.mediaDevices.getUserMedia inside the WebKitGTK webview.
/// Turns on WebKitSettings::enable-media-stream and allows UserMediaPermissionRequest in the `permission-request` handler.
#[cfg(target_os = "linux")]
fn enable_webkit_media_access(window: &tauri::WebviewWindow) {
    use webkit2gtk::{
        glib::object::ObjectExt, PermissionRequestExt, SecurityManagerExt, SettingsExt,
        UserMediaPermissionRequest, WebContextExt, WebViewExt,
    };
    let res = window.with_webview(|webview| {
        // wry exposes the underlying WebKitWebView via inner().
        let wkv = webview.inner();
        // The page's scheme counts as local, so <audio> may load a
        // session's meeting.opus from a file:// URL. `tauri` serves the
        // bundled page; `http` serves it from the dev server.
        if let Some(sm) = wkv.context().and_then(|c| c.security_manager()) {
            sm.register_uri_scheme_as_local("tauri");
            sm.register_uri_scheme_as_local("http");
        } else {
            log::warn!("no WebKit security manager; meeting.opus will not play from file://");
        }
        if let Some(settings) = wkv.settings() {
            settings.set_enable_media_stream(true);
            settings.set_enable_mediasource(true);
            // Enables getUserMedia on Tauri's custom-protocol origins.
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

/// Windows only: grants microphone requests from the app's own pages in the
/// WebView2 webview. Requests from any other origin get WebView2's default.
#[cfg(target_os = "windows")]
fn grant_webview2_microphone(window: &tauri::WebviewWindow) {
    use webview2_com::Microsoft::Web::WebView2::Win32::{
        COREWEBVIEW2_PERMISSION_KIND, COREWEBVIEW2_PERMISSION_KIND_MICROPHONE,
        COREWEBVIEW2_PERMISSION_STATE_ALLOW,
    };
    use webview2_com::PermissionRequestedEventHandler;
    let res = window.with_webview(|webview| unsafe {
        let core = match webview.controller().CoreWebView2() {
            Ok(core) => core,
            Err(e) => {
                log::warn!("webview2: no core webview for the microphone grant: {e}");
                return;
            }
        };
        let handler = PermissionRequestedEventHandler::create(Box::new(|_, args| {
            let Some(args) = args else { return Ok(()) };
            let mut kind = COREWEBVIEW2_PERMISSION_KIND::default();
            args.PermissionKind(&mut kind)?;
            if kind != COREWEBVIEW2_PERMISSION_KIND_MICROPHONE {
                return Ok(());
            }
            let mut uri = Default::default();
            args.Uri(&mut uri)?;
            if is_app_page(&webview2_com::take_pwstr(uri)) {
                args.SetState(COREWEBVIEW2_PERMISSION_STATE_ALLOW)?;
            }
            Ok(())
        }));
        let mut token = Default::default();
        if let Err(e) = core.add_PermissionRequested(&handler, &mut token) {
            log::warn!("webview2: could not wire the microphone grant: {e}");
        }
    });
    if let Err(e) = res {
        log::warn!("webview2: could not reach the webview for the microphone grant: {e}");
    }
}

/// True for a page the app serves itself on Windows: `http://tauri.localhost`.
#[cfg(any(target_os = "windows", test))]
fn is_app_page(uri: &str) -> bool {
    let Some(rest) = uri.strip_prefix("http://tauri.localhost") else { return false };
    rest.is_empty() || rest.starts_with('/') || rest.starts_with('?') || rest.starts_with('#')
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

/// Friendly low-memory reason when the machine is currently RAM-starved, else None.
use tauri_app_core::commands::finalize::finalize_orphan_in_app;
use tauri_app_core::openblas::cap_openblas_threads;

/// Flushes an in-progress recording as the app exits, on every way out
/// (window close, tray Quit, Cmd+Q): the recorder closes its chunk, writes
/// the manifest and removes the heartbeat, and next-launch recovery
/// finalizes the session. A start still opening its devices has an empty
/// slot, so it is waited out briefly first.
fn flush_recording_on_exit(app: &tauri::AppHandle) {
    for _ in 0..80 {
        if !tauri_app_core::commands::recording::is_starting() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let ar = app.state::<ActiveRecordingHandle>().0.lock().unwrap().take();
    if let Some(ar) = ar {
        log::info!("app exit: flushing active recording");
        if let Err(e) = tauri_app_core::commands::recording::teardown(ar, StopMode::Keep, || {}) {
            log::warn!("app exit: recorder teardown failed: {e}");
        }
    }
}

fn main() {
    // GDK_BACKEND: DAISY_FORCE_X11=1 opts into Xwayland; the platform
    // default is kept when unset. Set before any GTK/GDK init.
    #[cfg(target_os = "linux")]
    if std::env::var_os("GDK_BACKEND").is_none()
        && std::env::var("DAISY_FORCE_X11").map(|v| v == "1").unwrap_or(false)
    {
        std::env::set_var("GDK_BACKEND", "x11");
    }
    // Heartbeats name this machine the way its per-machine settings do.
    let machine = tauri_app_core::hardware::machine_name();
    let machine = if machine == tauri_app_core::hardware::UNKNOWN_MACHINE { String::new() } else { machine };
    recording::heartbeat::set_this_host(machine);
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
    // Caps per-call intra-op thread pools (OpenBLAS via whisper-rs ggml), set before the first BLAS init.
    std::env::set_var("OPENBLAS_NUM_THREADS", "1");
    std::env::set_var("OMP_NUM_THREADS", "1");
    // The libopenblas C API is also called directly. No-op off Linux.
    cap_openblas_threads(1);
    // Background finalize work creates long-lived threads and processes
    // (OpenBLAS pool, daisy-llm worker) through this runner at normal priority.
    transcript::cpu::start_normal_priority_runner();

    // The profile is resolved before the logger; the log file lives inside
    // the profile. Resolution emits no log macros.
    let profile = match ProfileDir::resolve_with_bootstrap() {
        Ok(Some(p)) => p,
        Ok(None) => ProfileDir::platform_default().expect("resolve platform default"),
        Err(_) => ProfileDir::platform_default().expect("resolve platform default"),
    };

    // Dual-sink logging: stderr at WARN+, file at INFO+/DEBUG+ per settings.
    let debug_level =
        tauri_app_core::settings::MachineSettings::load(&profile.settings_path()).debug_level;
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
    if let Some(line) = daisy_llm_protocol::process_power::opt_out_of_execution_throttling() {
        log::info!("{line}");
    }
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
        // Mirrors build_live_mode's no-key form-factor choice.
        let live_budget = (cores / 2).clamp(1, 6);
        log::info!("=== daisy telemetry ===");
        log::info!(
            "platform: os={} arch={} cores={} form_factor={} whisper_backend={}",
            std::env::consts::OS,
            std::env::consts::ARCH,
            cores,
            if has_battery { "laptop" } else { "desktop" },
            providers_local::whisper_backend(),
        );
        log::info!(
            "version: {} sha: {}",
            env!("CARGO_PKG_VERSION"),
            env!("DAISY_BUILD_SHA"),
        );
        log::info!(
            "asr caps: live_whisper_threads={} finalize_cpu_budget={} finalize_whisper_threads<=8",
            live_budget,
            transcript::cpu::finalize_cpu_budget(),
        );
    }
    let app_state = AppState::new(profile);
    let active = ActiveRecordingHandle(Mutex::new(None));
    let vault_state = VaultState::new();

    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, _shortcut, event| {
                    if !matches!(
                        event.state(),
                        tauri_plugin_global_shortcut::ShortcutState::Pressed
                    ) {
                        return;
                    }
                    let app = app.clone();
                    tauri::async_runtime::spawn(async move {
                        if let Err(e) =
                            tauri_app_core::commands::recording::quick_start_recording(app).await
                        {
                            log::warn!("record hotkey: {e}");
                        }
                    });
                })
                .build(),
        );
    #[cfg(feature = "ui-test")]
    let builder = builder
        .plugin(tauri_plugin_wdio::init())
        .plugin(tauri_plugin_wdio_webdriver::init());
    builder
        .manage(app_state)
        .manage(active)
        .manage(vault_state)
        .manage(AiJobs::default())
        .manage(tauri_app_core::commands::files::DialogPicks::default())
        .manage(ClockStyle(AtomicBool::new(true)))
        .manage(ReminderState(Mutex::new(None)))
        .manage(ClosePromptPending(AtomicBool::new(false)))
        .manage(tauri_app_core::mcp::server::McpHandle::default())
        .manage(tauri_app_core::sync::server::SyncServerHandle::default())
        .manage(tauri_app_core::sync::peripheral::PeripheralHandle::default())
        .setup(|app| {
            {
                let handle = app.handle().clone();
                app.state::<AppState>().on_settings_changed(move |which| {
                    tauri_app_core::commands::settings::emit_changed(&handle, which)
                });
            }
            // Points the model-dir env vars at Tauri's bundled resource dir on non-AppImage installs (Windows NSIS/portable, macOS .app).
            if let Ok(res_dir) = app.path().resource_dir() {
                for (sub, var) in [
                    ("models/voiceprints", "DAISY_VOICEPRINT_DIR"),
                    ("models/whisper", "DAISY_WHISPER_MODEL_DIR"),
                    ("models/nemotron", "DAISY_NEMOTRON_MODEL_DIR"),
                    ("models/embeddings", "DAISY_EMBED_DIR"),
                    ("models/nemotron3-diar", "DAISY_NEMOTRON3_DIAR_DIR"),
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
            // Model downloads live under <profile>/models, outside the vault:
            // the queue drains whether or not the vault is unlocked.
            tauri_app_core::downloads::start(app.handle().clone());
            // App-lifetime suspend/resume watch: a sleep pauses the active
            // recording, the wake resumes it.
            app.manage(tauri_app_core::sleep_watch::start(app.handle()));
            // App-lifetime topology hub: one OS watcher for the process.
            // The UI subscription keeps every source list live
            // (pre-recording screens included; bursts coalesce into one
            // event); each recording's supervisor subscribes alongside.
            {
                let hub = std::sync::Arc::new(audio_engine::topology::TopologyHub::start());
                let (topo_tx, topo_rx) =
                    std::sync::mpsc::channel::<audio_engine::topology::TopologyEvent>();
                let ui_sub = hub.subscribe(topo_tx);
                app.manage(TopologyHubHandle(hub, ui_sub));
                let app_for_topo = app.handle().clone();
                std::thread::Builder::new()
                    .name("daisy-ui-topology".into())
                    .spawn(move || {
                        use audio_engine::topology::TopologyEvent as TEv;
                        let churn = |ev: &TEv| {
                            matches!(ev, TEv::DeviceAdded { .. } | TEv::DeviceRemoved { .. })
                        };
                        while let Ok(ev) = topo_rx.recv() {
                            if !churn(&ev) {
                                continue;
                            }
                            // Coalesce the burst: emit after 300 ms with no further CHURN event.
                            let mut last_churn = std::time::Instant::now();
                            loop {
                                let elapsed = last_churn.elapsed();
                                let Some(remaining) =
                                    std::time::Duration::from_millis(300).checked_sub(elapsed)
                                else {
                                    break;
                                };
                                match topo_rx.recv_timeout(remaining) {
                                    Ok(next) if churn(&next) => {
                                        last_churn = std::time::Instant::now();
                                    }
                                    Ok(_) => {}
                                    Err(_) => break,
                                }
                            }
                            tauri_app_core::commands::settings::audio_sources_changed(
                                &app_for_topo,
                            );
                        }
                    })
                    .expect("spawn ui topology thread");
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
            // Meeting-started and meeting-ended prompts: one monitor thread
            // for the process.
            {
                let app_for_meeting = app.handle().clone();
                let probe_handle = app.handle().clone();
                tauri_app_core::meeting_detect::spawn(
                    app_for_meeting,
                    move || {
                        use tauri::Manager;
                        use tauri_app_core::meeting_detect::Probe;
                        let Some(active) = probe_handle.try_state::<ActiveRecordingHandle>() else {
                            return Probe::Idle;
                        };
                        let slot = active.0.lock().unwrap();
                        let Some(ar) = slot.as_ref() else {
                            return Probe::Idle;
                        };
                        let system_active = ar.recorder.take_system_activity();
                        match ar.recorder.state() {
                            recording::State::Recording => Probe::Recording { system_active },
                            recording::State::Paused => Probe::Paused,
                            _ => Probe::Idle,
                        }
                    },
                    |app| {
                        if let Err(e) = show_meeting_prompt(app) {
                            log::warn!("meeting prompt: {e}");
                        }
                    },
                    |app, name| {
                        if let Err(e) = show_meeting_ended_prompt(app, name) {
                            log::warn!("meeting-ended prompt: {e}");
                        }
                    },
                );
            }
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
                // A bootstrap pointer plus an on-disk vault means first-run
                // setup already finished; backfill the marker to match.
                if !tauri_app_core::bootstrap::Onboarding::is_complete()
                    && tauri_app_core::bootstrap::Bootstrap::load().ok().flatten().is_some()
                    && vault_path(&state).is_file()
                {
                    if let Err(e) = tauri_app_core::bootstrap::Onboarding::mark_complete() {
                        log::warn!("onboarding: backfill marker failed: {e}");
                    }
                }
            }
            // Start-recording hotkey. A combination the OS refuses leaves
            // the app without one; the stored value is unchanged.
            {
                let accel = app.state::<AppState>().machine_settings().record_hotkey;
                if let Err(e) = tauri_app_core::hotkey::apply(app.handle(), accel.as_deref()) {
                    log::warn!("record hotkey: {e}");
                }
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
            // Reconciles the profile name to the license. Needs no vault.
            {
                let app_handle = app.handle().clone();
                tauri::async_runtime::spawn_blocking(move || {
                    pin_profile_name_to_license(&app_handle.state::<AppState>());
                });
            }
            // A machine-mode vault is already open here; a passphrase-mode
            // one opens from the unlock command.
            if app.state::<VaultState>().is_unlocked() {
                vault_opened(app.handle());
            } else {
                log::info!("waiting for the vault to be unlocked");
            }
            // Closing the main window quits, minimizes, or asks, per the
            // machine's `minimize_on_close`.
            if let Some(main) = app.get_webview_window(MAIN_LABEL) {
                let app_handle = app.handle().clone();
                main.on_window_event(move |event| {
                    if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                        let pending = app_handle.state::<ClosePromptPending>();
                        let setting = app_handle.state::<AppState>().machine_settings().minimize_on_close;
                        let vault_open = app_handle.state::<VaultState>().is_unlocked();
                        let prompt_pending = pending.0.load(std::sync::atomic::Ordering::SeqCst);
                        match close_action(setting, prompt_pending, vault_open) {
                            CloseAction::Quit => app_handle.exit(0),
                            CloseAction::Minimize => {
                                api.prevent_close();
                                minimize_main(&app_handle);
                            }
                            CloseAction::Ask => {
                                api.prevent_close();
                                pending.0.store(true, std::sync::atomic::Ordering::SeqCst);
                                if let Err(e) = tauri_app_core::mini_window::show_main(&app_handle) {
                                    log::warn!("close prompt: {e}");
                                }
                                let _ = app_handle.emit_to(MAIN_LABEL, "daisy://close/ask", ());
                            }
                        }
                    }
                });
                #[cfg(target_os = "linux")]
                enable_webkit_media_access(&main);
                #[cfg(target_os = "windows")]
                grant_webview2_microphone(&main);
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
            set_ai_provider,
            onboarding_plan,
            onboarding_apply,
            onboarding_finish,
            machine_settings,
            reduce_transparency,
            update_machine_settings,
            resolve_close_prompt,
            list_css_themes,
            read_css_theme,
            accept_consent,
            eula_status,
            accept_eula,
            profile_binding_check,
            license_status,
            activate_license,
            license_checkin,
            license_upgrade_checkout,
            deactivate_license,
            check_for_update,
            open_external,
            open_logs_dir,
            open_profile_dir,
            vault_status,
            unlock_vault,
            change_vault_passphrase,
            switch_vault_mode,
            init_vault_machine_mode,
            vault_kind,
            vault_trust,
            read_settings,
            update_settings,
            mcp_status,
            mcp_apply,
            mcp_regenerate_token,
            mcp_port_available,
            sync_status,
            linux_firewall_hint,
            sync_pairing_begin,
            sync_pairing_cancel,
            sync_unpair,
            sync_rename_device,
            list_audio_sources,
            start_mic_meter,
            stop_mic_meter,
            calibrate_speech_level,
            speech_level_set_override,
            speech_levels_list,
            list_providers,
            list_provider_models,
            probe_context_length,
            set_provider,
            register_gateway,
            lock_vault,
            reset_vault,
            list_sessions,
            read_session,
            list_asr_models,
            set_active_asr_model,
            delete_asr_model,
            download_queue_status,
            download_queue_enqueue,
            download_queue_cancel,
            download_queue_retry,
            builtin_model_status,
            builtin_model_delete,
            set_session_speaker_label,
            remove_speaker_cluster,
            add_session_speaker,
            list_session_speakers,
            read_live_names,
            session_speaker_sample_audio_bytes,
            session_speaker_sample_text,
            delete_session,
            truncate_session_after,
            split_session_at,
            start_recording,
            pause_recording,
            switch_recording_mic,
            set_mic_muted,
            resume_recording,
            stop_recording,
            cancel_recording,
            recording_snapshot,
            default_recording_title,
            set_clock_hour12,
            show_mini_window,
            show_main_window,
            show_reminder_window,
            reminder_payload,
            reminder_action,
            finish_recording,
            meeting_detection_support,
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
            diarize_attendee_count,
            diarize_count,
            voice_model_status,
            retry_finalize,
            finalize_recovery,
            qa_ask_stream,
            live_chat_send_stream,
            live_chat_load,
            live_chat_delete,
            list_voiceprints,
            rename_voiceprint,
            delete_voiceprint,
            detach_speaker_voiceprint,
            enroll_voiceprint_from_speaker,
            rematch_all_sessions,
            ai_jobs_list,
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
            tag_suggest_prerecord,
            tag_suggest_for_session,
            session_dismiss_tag_suggestions,
            session_undismiss_tag_suggestions,
            session_apply_tag_suggestions,
            summary_load,
            summary_save_edit,
            summary_regenerate,
            search_sessions,
            move_profile,
            probe_profile_dir,
            switch_profile,
            recordings_stats,
            recordings_delete_all,
            session_has_playback_audio,
            session_playback_audio_bytes,
            session_playback_audio_url,
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
            pick_import_audio,
            #[cfg(feature = "ui-test")]
            ui_test_record_audio_pick,
            pick_directory,
            env_profile_dir
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri app")
        .run(|app_handle, event| {
            if let tauri::RunEvent::Exit = event {
                flush_recording_on_exit(app_handle);
                // Stop the live passes before the process ends.
                tauri_app_core::live_names::shutdown(std::time::Duration::from_secs(3));
                tauri_app_core::live_diarize::shutdown(std::time::Duration::from_secs(3));
                log::logger().flush();
                // Exit without running C++ static destructors.
                #[cfg(unix)]
                unsafe {
                    libc::_exit(0)
                };
            }
        });
}

#[cfg(test)]
mod mic_meter_tests {
    use super::*;
    use tauri_app_core::commands::recording::{stop_all_mic_meters, MIC_METER_RELEASE_TIMEOUT};

    /// A meter that releases its device is waited for, and the wait ends as
    /// soon as it has — not on the timeout.
    #[test]
    fn stop_all_mic_meters_waits_for_release_then_returns() {
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let rid = "test-meter".to_string();
        mic_meter_registry()
            .lock()
            .unwrap()
            .insert(rid.clone(), tx);

        // Stands in for the meter thread: exits on the stop signal and drops
        // its own registry entry, which is what marks the device free.
        let worker = std::thread::spawn(move || {
            rx.recv().expect("stop signal");
            std::thread::sleep(std::time::Duration::from_millis(100));
            mic_meter_registry().lock().unwrap().remove("test-meter");
        });

        let started = std::time::Instant::now();
        stop_all_mic_meters();
        let waited = started.elapsed();
        worker.join().unwrap();

        assert!(
            mic_meter_registry().lock().unwrap().is_empty(),
            "registry should be empty once the meter has released"
        );
        assert!(
            waited >= std::time::Duration::from_millis(100),
            "returned before the meter released (waited {waited:?})"
        );
        assert!(
            waited < MIC_METER_RELEASE_TIMEOUT,
            "waited for the full timeout instead of the release (waited {waited:?})"
        );
    }
}

#[cfg(test)]
mod app_page_tests {
    use super::is_app_page;

    #[test]
    fn only_the_apps_own_pages_count() {
        for uri in ["http://tauri.localhost", "http://tauri.localhost/", "http://tauri.localhost/index.html#/record"] {
            assert!(is_app_page(uri), "{uri}");
        }
        for uri in [
            "https://tauri.localhost/",
            "http://tauri.localhost.evil.com/",
            "http://tauri.localhost:8080/",
            "https://example.com/?u=http://tauri.localhost/",
            "",
        ] {
            assert!(!is_app_page(uri), "{uri}");
        }
    }
}
