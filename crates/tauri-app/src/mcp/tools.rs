//! MCP tool surface: read-only queries over the meeting library, plus one
//! write tool (`import_session`) gated behind `mcp_allow_write`.

use crate::commands::{library, meeting, qa, summary, tags};
use crate::error::AppError;
use crate::library_events::{self, LibraryChangeKind};
use crate::mcp::protocol::{ToolDef, ToolHost};
use crate::settings::Settings;
use crate::state::{AppState, VaultState};
use serde_json::{json, Value};
use tauri::Manager as _;

/// Rejects session ids containing path separators, `..`, or a leading dot.
pub fn valid_session_id(id: &str) -> bool {
    !id.is_empty()
        && !id.contains(['/', '\\'])
        && !id.contains("..")
        && !id.starts_with('.')
}

/// `ToolHost` backed by the live app — resolves `AppState`/`VaultState`
/// from the handle per call.
pub struct AppHost {
    pub app_handle: tauri::AppHandle,
}

impl AppHost {
    fn settings(&self) -> Settings {
        let app = self.app_handle.state::<AppState>();
        Settings::load_or_default(&app.profile.settings_path())
    }
}

impl ToolHost for AppHost {
    fn tools(&self) -> Vec<ToolDef> {
        let mut defs = read_tool_defs();
        // Write tools are advertised only when the user has opted in.
        if self.settings().mcp_allow_write {
            defs.push(import_session_def());
        }
        defs
    }

    fn call_tool(&self, name: &str, args: &Value) -> Result<String, String> {
        let app = self.app_handle.state::<AppState>();
        let vault = self.app_handle.state::<VaultState>();
        // The server answers nothing while the vault is locked.
        if !vault.is_unlocked() {
            return Err("Daisy's vault is locked — unlock the app first.".into());
        }
        match name {
            "list_sessions" => list_sessions(&app, args),
            "list_tags" => list_tags(&app),
            "get_transcript" => get_transcript(&app, args),
            "get_summary" => get_summary(&app, args),
            "search_meetings" => search_meetings(&app, args),
            "import_session" => {
                // Writes are refused unless explicitly enabled, including
                // calls to the unadvertised tool name.
                if !self.settings().mcp_allow_write {
                    return Err(
                        "writes are disabled — enable them in Settings → MCP first.".into(),
                    );
                }
                self.import_session(&app, args)
            }
            other => Err(format!("unknown tool: {other}")),
        }
    }
}

impl AppHost {
    fn import_session(&self, app: &AppState, args: &Value) -> Result<String, String> {
        let req: meeting::ImportSessionRequest = serde_json::from_value(args.clone())
            .map_err(|e| format!("bad import_session arguments: {e}"))?;
        let sid = meeting::import_session_impl(app, req).map_err(err_str)?;
        // Refresh any open library view.
        library_events::emit(&self.app_handle, LibraryChangeKind::Added, &sid);
        // Dispatches imported-trigger workflows and wakes the worker via the
        // managed handle; when the handle is absent, the periodic tick picks
        // the work up.
        if let Ok(snap) = crate::commands::workflows::snapshot_for_session(app, &sid) {
            use tauri::Manager;
            match crate::commands::workflow_engine::dispatch(
                app,
                crate::commands::workflows::TriggerEvent::Imported,
                &snap,
            ) {
                Ok(n) if n > 0 => {
                    if let Some(h) = self
                        .app_handle
                        .try_state::<crate::commands::workflow_engine::WorkflowEngineHandle>()
                    {
                        h.0.notify_one();
                    }
                }
                Ok(_) => {}
                Err(e) => log::warn!("workflow dispatch (imported {sid}): {e}"),
            }
        }
        Ok(format!("imported session {sid}"))
    }
}

fn read_tool_defs() -> Vec<ToolDef> {
    vec![
        ToolDef {
            name: "list_sessions",
            description: "List recorded meeting sessions, newest first. Start here to get the \
                          session_id that the other tools need.\n\
                          Returns a JSON array of objects: {session_id (string — pass to \
                          get_transcript/get_summary), title (string|null), \
                          created_at_unix_seconds, finalized_at_unix_seconds (null if still \
                          processing), duration_seconds, tag_ids (string[] — resolve names via \
                          list_tags), has_transcript, has_summary, has_dedup, interrupted \
                          (true if recovered from a crash)}.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "limit": { "type": "integer", "description": "Max sessions to return (default 25)." }
                }
            }),
        },
        ToolDef {
            name: "list_tags",
            description: "List the meeting tags the user has defined, most-used first. Use \
                          this to resolve the tag_ids returned by list_sessions into names, \
                          or to pick valid tag_ids for import_session.\n\
                          Returns a JSON array: {id (string — the tag_id used elsewhere), \
                          name, color_hex, use_count}.",
            input_schema: json!({ "type": "object", "properties": {} }),
        },
        ToolDef {
            name: "get_transcript",
            description: "Full finalized transcript of one session, returned as a single \
                          markdown string. Speaker-labelled, one turn per line as \
                          '**Speaker:** text'. Use a session_id from list_sessions. Errors if \
                          the session has no transcript (still processing, or wrong id).",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "session_id": { "type": "string", "description": "From list_sessions." }
                },
                "required": ["session_id"]
            }),
        },
        ToolDef {
            name: "get_summary",
            description: "The session's AI summary as a single markdown string (sections: \
                          ## TL;DR, ## Action items, ## Decisions). Use a session_id from \
                          list_sessions. Errors if no summary has been generated for it \
                          (check has_summary first).",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "session_id": { "type": "string", "description": "From list_sessions." }
                },
                "required": ["session_id"]
            }),
        },
        ToolDef {
            name: "search_meetings",
            description: "Semantic search across ALL meeting transcripts at once — no LLM \
                          synthesis; you read the excerpts and reason yourself.\n\
                          Returns JSON: {citations: [{session_id (pass to get_transcript for \
                          full context), session_title (string|null), chunk_index, start_ms \
                          (offset into the meeting, may be null), excerpt (the matched text), \
                          score (cosine, higher = closer)}], indexed_sessions, total_chunks}. \
                          Citations are sorted best-first.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Natural-language search query." },
                    "top_k": { "type": "integer", "description": "Excerpts to return (default 8, max 25)." }
                },
                "required": ["query"]
            }),
        },
    ]
}

/// Write tool — only advertised when `mcp_allow_write` is on.
fn import_session_def() -> ToolDef {
    ToolDef {
        name: "import_session",
        description: "Create a new meeting session from text (no audio). Supply any of \
                      transcript / summary / notes as markdown; the session joins the library \
                      and the searchable corpus. Always creates a NEW session — never \
                      overwrites or deletes an existing one. At least one of transcript_md / \
                      summary_md / notes_md must be non-empty.\n\
                      Match Daisy's native format so it renders cleanly: transcript lines as \
                      '**Speaker:** text', summary with '## TL;DR', '## Action items' \
                      (markdown list), '## Decisions'. Plain prose also works.\n\
                      Returns a confirmation string containing the new session_id.",
        input_schema: json!({
            "type": "object",
            "properties": {
                "title": { "type": "string", "description": "Meeting title shown in the library." },
                "occurred_at": { "type": "integer", "description": "Unix seconds the meeting happened (sets its library sort position). Default: now." },
                "transcript_md": { "type": "string", "description": "Transcript markdown, e.g. '**Alice:** Let's ship Friday.\\n\\n**Bob:** Agreed.'" },
                "summary_md": { "type": "string", "description": "Summary markdown, e.g. '## TL;DR\\nShipping Friday.\\n\\n## Action items\\n- Bob: cut release'" },
                "notes_md": { "type": "string", "description": "Freeform notes markdown." },
                "tag_ids": { "type": "array", "items": { "type": "string" }, "description": "Existing tag ids to attach (optional)." }
            }
        }),
    }
}

fn str_arg<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| format!("missing required argument: {key}"))
}

fn list_sessions(app: &AppState, args: &Value) -> Result<String, String> {
    let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(25) as usize;
    let mut entries = library::list_sessions_impl(app).map_err(err_str)?;
    entries.truncate(limit);
    serde_json::to_string_pretty(&entries).map_err(|e| e.to_string())
}

fn list_tags(app: &AppState) -> Result<String, String> {
    // Projects to the public shape; prompt_md is not exposed.
    let tags: Vec<Value> = tags::list_tags_impl(app)
        .map_err(err_str)?
        .into_iter()
        .map(|t| json!({
            "id": t.id,
            "name": t.name,
            "color_hex": t.color_hex,
            "use_count": t.use_count,
        }))
        .collect();
    serde_json::to_string_pretty(&tags).map_err(|e| e.to_string())
}

fn get_transcript(app: &AppState, args: &Value) -> Result<String, String> {
    let sid = str_arg(args, "session_id")?;
    if !valid_session_id(sid) {
        return Err("invalid session_id".into());
    }
    let path = app.profile.sessions_dir().join(sid).join("transcript.md");
    match syncsafe::read_to_string(&path) {
        Ok(md) if !md.trim().is_empty() => Ok(md),
        _ => Err(format!(
            "no transcript for session {sid} (not finalized, or wrong id — use list_sessions)"
        )),
    }
}

fn get_summary(app: &AppState, args: &Value) -> Result<String, String> {
    let sid = str_arg(args, "session_id")?;
    if !valid_session_id(sid) {
        return Err("invalid session_id".into());
    }
    match summary::summary_load_impl(app, sid).map_err(err_str)? {
        Some(s) => Ok(s.markdown),
        None => Err(format!("no summary generated for session {sid}")),
    }
}

fn search_meetings(app: &AppState, args: &Value) -> Result<String, String> {
    let query = str_arg(args, "query")?;
    let top_k = args.get("top_k").and_then(|v| v.as_u64()).unwrap_or(8).min(25) as usize;
    let r = qa::qa_retrieve_impl(app, query, top_k).map_err(err_str)?;
    serde_json::to_string_pretty(&r).map_err(|e| e.to_string())
}

fn err_str(e: AppError) -> String {
    e.to_string()
}

#[cfg(test)]
mod tests {
    use super::valid_session_id;

    #[test]
    fn session_id_guard() {
        assert!(valid_session_id("2026-06-09T10-00-00-abcd"));
        assert!(!valid_session_id(""));
        assert!(!valid_session_id("../other"));
        assert!(!valid_session_id("a/b"));
        assert!(!valid_session_id("a\\b"));
        assert!(!valid_session_id(".hidden"));
    }
}
