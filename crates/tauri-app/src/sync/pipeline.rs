// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! What runs on a session that arrived from a phone: the same tail a desktop
//! recording gets, minus the arms a phone recording has no use for. The
//! phone transcribed it, there is no system track and no echo to cancel, so
//! the work is the playback file, the rendered transcript, the summary and
//! the chapters.

use crate::state::{AppState, VaultState};
use crate::sync::server::ImportStage;

/// Builds `meeting.opus` from the phone's AAC chunks, renders
/// `transcript.md`, then writes `summary.json` and `chapters.json`. Every
/// step is best-effort and logged: a session with no summary provider
/// configured still lands in the library with its transcript, and the phone
/// is offered whatever did get built. `step` is called as each one starts,
/// so the person watching sees where a long import has got to.
pub fn run(app: &AppState, vs: &VaultState, session_id: &str, step: &dyn Fn(ImportStage)) {
    step(ImportStage::Audio);
    let session_dir = app.profile.session_path(session_id);
    match crate::commands::session_store::read_manifest_opt(&session_dir) {
        Some(m) => match recording::mixdown::build_meeting_audio(
            &session_dir,
            &m,
            &recording::compress::CompressParams::default(),
        ) {
            Ok(n) => log::info!("sync: meeting.opus for {session_id} ({n} bytes)"),
            Err(e) => log::warn!("sync: meeting.opus for {session_id}: {e}"),
        },
        None => log::warn!("sync: meeting.opus for {session_id}: manifest unreadable"),
    }
    step(ImportStage::Transcript);
    if let Err(e) = crate::commands::session::rerender_session_transcript_impl(app, session_id) {
        log::warn!("sync: transcript.md for {session_id}: {e}");
        return;
    }
    step(ImportStage::Summary);
    match crate::commands::summary::summary_generate_impl(
        app,
        vs,
        crate::commands::summary::SummaryGenerateRequest {
            session_id: session_id.to_string(),
            provider: None,
            model: None,
            force: Some(false),
            prompt_id: None,
        },
    ) {
        Ok(_) => log::info!("sync: summarized {session_id}"),
        Err(e) => log::warn!("sync: summary for {session_id}: {e}"),
    }
    step(ImportStage::Chapters);
    match crate::commands::chapters::extract_chapters_impl(
        app,
        vs,
        crate::commands::chapters::ChaptersRequest {
            session_id: session_id.to_string(),
            provider: None,
            model: None,
        },
    ) {
        Ok(r) if r.skipped => log::info!("sync: chapters skipped for {session_id}: {}", r.reason.unwrap_or_default()),
        Ok(r) => log::info!("sync: {} chapters for {session_id}", r.chapters.len()),
        Err(e) => log::warn!("sync: chapters for {session_id}: {e}"),
    }
}
