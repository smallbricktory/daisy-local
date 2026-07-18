//! `library:changed` event bus. Every library mutation site emits; the
//! frontend subscribes via `listen('library:changed', ...)`.
//!
//! Kinds:
//!   * `added`     — a new session directory was created.
//!   * `updated`   — the session manifest was mutated.
//!   * `finalized` — finalize_at_unix_seconds was set.
//!   * `deleted`   — the session directory was removed.
use serde::Serialize;
use tauri::Emitter;

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LibraryChangeKind {
    Added,
    Updated,
    Finalized,
    Deleted,
}

#[derive(Debug, Clone, Serialize)]
struct LibraryChangedPayload<'a> {
    kind: LibraryChangeKind,
    session_id: &'a str,
}

/// Emits `library:changed` to every webview and invalidates the cached
/// session list. Failures are logged and never propagate.
pub fn emit<R: tauri::Runtime, E: Emitter<R>>(
    emitter: &E,
    kind: LibraryChangeKind,
    session_id: &str,
) {
    crate::commands::library::invalidate_library_cache();
    if let Err(e) = emitter.emit(
        "library:changed",
        LibraryChangedPayload { kind, session_id },
    ) {
        log::warn!("emit library:changed ({kind:?}, {session_id}): {e}");
    }
}
