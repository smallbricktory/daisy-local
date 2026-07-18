//! Prompt CRUD over a plaintext `<profile>/prompts.json`. Built-ins are
//! seeded on first load and re-synced when unmodified; user-forked prompts
//! are never touched. Writes are atomic (`.tmp` + rename).
use crate::error::{AppError, Result};
use crate::state::AppState;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use summarize::prompts::{seed_prompts, Envelope, Prompt, DAISY_DIRECTIVE_MIN_CHARS, DAISY_ID};

#[derive(Debug, Serialize, Deserialize)]
pub struct PromptsFile {
    #[serde(default = "default_schema")]
    pub schema_version: u32,
    #[serde(default)]
    pub prompts: Vec<Prompt>,
}
fn default_schema() -> u32 {
    1
}

pub fn prompts_path(app: &AppState) -> PathBuf {
    app.profile.root().join("prompts.json")
}

/// Load the prompts file, seeding/re-syncing built-ins. A built-in is re-synced
/// from `seed_prompts()` only when its stored copy is still a built-in (user
/// forks get fresh uuids and are left alone). Built-ins missing from disk are
/// inserted.
pub fn load_prompts(app: &AppState) -> Result<Vec<Prompt>> {
    let p = prompts_path(app);
    let mut stored: Vec<Prompt> = if p.is_file() {
        let bytes = syncsafe::read(&p)?;
        serde_json::from_slice::<PromptsFile>(&bytes)
            .map_err(|e| AppError::Config(format!("parse prompts.json: {e}")))?
            .prompts
    } else {
        Vec::new()
    };
    let mut changed = false;
    for seed in seed_prompts() {
        match stored.iter_mut().find(|s| s.id == seed.id) {
            Some(existing) if existing.builtin => {
                // Name/output always re-sync to the shipped values.
                if existing.name != seed.name || existing.output != seed.output {
                    existing.name = seed.name.clone();
                    existing.output = seed.output;
                    changed = true;
                }
                // Daisy Summarizer: a stored directive of at least
                // DAISY_DIRECTIVE_MIN_CHARS is kept as a user override;
                // anything shorter re-syncs to the shipped text. Other
                // built-ins' directives always re-sync.
                let keep_override = seed.id == DAISY_ID
                    && existing.directive_md.trim().chars().count() >= DAISY_DIRECTIVE_MIN_CHARS;
                if !keep_override && existing.directive_md != seed.directive_md {
                    existing.directive_md = seed.directive_md.clone();
                    changed = true;
                }
            }
            Some(_) => {} // a non-builtin entry with a built-in id is left as-is
            None => {
                stored.push(seed);
                changed = true;
            }
        }
    }
    // The Daisy Summarizer is always the first entry; the stable sort keeps
    // the rest in order.
    stored.sort_by_key(|p| if p.id == DAISY_ID { 0 } else { 1 });
    if changed {
        save_prompts(app, &stored)?;
    }
    Ok(stored)
}

pub fn save_prompts(app: &AppState, prompts: &[Prompt]) -> Result<()> {
    let p = prompts_path(app);
    let tmp = p.with_extension("json.tmp");
    let file = PromptsFile {
        schema_version: 1,
        prompts: prompts.to_vec(),
    };
    syncsafe::write(&tmp, serde_json::to_vec_pretty(&file)?)?;
    syncsafe::rename(&tmp, &p)?;
    Ok(())
}

pub fn list_prompts_impl(app: &AppState) -> Result<Vec<Prompt>> {
    load_prompts(app)
}

#[derive(Debug, Deserialize)]
pub struct SavePromptRequest {
    pub id: Option<String>, // None = create
    pub name: String,
    pub directive_md: String,
    pub output: Envelope, // used on create; ignored when updating a user style
}

/// Create a new user style (id=None) or update an existing user style in
/// place. Built-in ids are not mutated here, with one exception: the Daisy
/// Summarizer's directive accepts an override, gated on
/// `DAISY_DIRECTIVE_MIN_CHARS`.
pub fn save_prompt_impl(app: &AppState, req: SavePromptRequest) -> Result<Prompt> {
    let name = req.name.trim().to_string();
    if name.is_empty() {
        return Err(AppError::Config("style name is empty".into()));
    }
    let mut prompts = load_prompts(app)?;
    let style = match req.id {
        Some(id) => {
            let s = prompts
                .iter_mut()
                .find(|s| s.id == id)
                .ok_or_else(|| AppError::Config(format!("no style {id}")))?;
            if s.builtin && s.id != DAISY_ID {
                return Err(AppError::Config(
                    "built-in prompts cannot be edited in place".into(),
                ));
            }
            if s.builtin {
                if req.directive_md.trim().chars().count() < DAISY_DIRECTIVE_MIN_CHARS {
                    return Err(AppError::Config(format!(
                        "The Summarizer prompt must be at least {DAISY_DIRECTIVE_MIN_CHARS} characters — it drives every summary."
                    )));
                }
                s.directive_md = req.directive_md; // name/output are not editable
            } else {
                s.name = name;
                s.directive_md = req.directive_md;
            }
            s.clone()
        }
        None => {
            let s = Prompt {
                id: uuid::Uuid::new_v4().to_string(),
                name,
                output: req.output,
                directive_md: req.directive_md,
                builtin: false,
            };
            prompts.push(s.clone());
            s
        }
    };
    save_prompts(app, &prompts)?;
    Ok(style)
}


pub fn delete_prompt_impl(app: &AppState, id: &str) -> Result<()> {
    let mut prompts = load_prompts(app)?;
    if prompts.iter().any(|s| s.id == id && s.builtin) {
        return Err(AppError::Config("built-in prompts cannot be deleted".into()));
    }
    prompts.retain(|s| s.id != id);
    save_prompts(app, &prompts)?;
    // If the deleted prompt was the default, the default becomes Daisy.
    let sp = app.profile.settings_path();
    let mut settings = crate::settings::Settings::load_or_default(&sp);
    if settings.default_summary_prompt_id.as_deref() == Some(id) {
        settings.default_summary_prompt_id = Some(DAISY_ID.to_string());
        let _ = settings.save(&sp);
    }
    Ok(())
}

/// Drop a built-in prompt's file override and restore the shipped directive.
pub fn reset_prompt_impl(app: &AppState, id: &str) -> Result<Prompt> {
    let seed = seed_prompts()
        .into_iter()
        .find(|s| s.id == id)
        .ok_or_else(|| AppError::Config(format!("{id} is not a built-in prompt")))?;
    let mut prompts = load_prompts(app)?;
    let s = prompts
        .iter_mut()
        .find(|s| s.id == id)
        .ok_or_else(|| AppError::Config(format!("no prompt {id}")))?;
    s.name = seed.name;
    s.directive_md = seed.directive_md;
    s.output = seed.output;
    let out = s.clone();
    save_prompts(app, &prompts)?;
    Ok(out)
}

/// Resolve the style to use: explicit id → settings default → Daisy built-in.
pub fn resolve_prompt(app: &AppState, explicit: Option<&str>) -> Result<Prompt> {
    let prompts = load_prompts(app)?;
    let wanted = explicit
        .map(str::to_string)
        .or_else(|| {
            crate::settings::Settings::load_or_default(&app.profile.settings_path())
                .default_summary_prompt_id
        })
        .unwrap_or_else(|| DAISY_ID.to_string());
    Ok(prompts
        .iter()
        .find(|s| s.id == wanted)
        .or_else(|| prompts.iter().find(|s| s.id == DAISY_ID))
        .cloned()
        .unwrap_or_else(|| seed_prompts().remove(0)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> (AppState, tempfile::TempDir) {
        let tmp = tempfile::tempdir().unwrap();
        let profile = crate::profile::ProfileDir::at(tmp.path()).unwrap();
        (AppState::new(profile), tmp)
    }

    #[test]
    fn seeds_builtins_on_first_load() {
        let (app, _t) = app();
        let s = list_prompts_impl(&app).unwrap();
        assert_eq!(s.len(), 6);
        assert!(s.iter().any(|x| x.id == DAISY_ID));
    }

    #[test]
    fn builtin_wording_resyncs_but_user_prompts_are_untouched() {
        let (app, _t) = app();
        // Stale built-in wording + one user prompt on disk.
        let mut prompts = load_prompts(&app).unwrap();
        prompts
            .iter_mut()
            .find(|p| p.id == DAISY_ID)
            .unwrap()
            .directive_md = "OLD SHIPPED TEXT".into();
        prompts.push(Prompt {
            id: "u1".into(),
            name: "Mine".into(),
            output: Envelope::Sectioned,
            directive_md: "my text".into(),
            builtin: false,
        });
        save_prompts(&app, &prompts).unwrap();

        let reloaded = load_prompts(&app).unwrap();
        let daisy = reloaded.iter().find(|p| p.id == DAISY_ID).unwrap();
        assert_eq!(
            daisy.directive_md,
            seed_prompts()[0].directive_md,
            "built-in re-synced to shipped wording"
        );
        let mine = reloaded.iter().find(|p| p.id == "u1").unwrap();
        assert_eq!(mine.directive_md, "my text", "user prompt left alone");
        assert_eq!(reloaded.len(), 7);
    }

    #[test]
    fn save_creates_user_style_and_resolve_finds_it() {
        let (app, _t) = app();
        let created = save_prompt_impl(
            &app,
            SavePromptRequest {
                id: None,
                name: "Mine".into(),
                directive_md: "narrate".into(),
                output: Envelope::Sectioned,
            },
        )
        .unwrap();
        assert!(!created.builtin);
        let got = resolve_prompt(&app, Some(&created.id)).unwrap();
        assert_eq!(got.name, "Mine");
        assert_eq!(got.output, Envelope::Sectioned);
    }

    #[test]
    fn rejects_empty_name_and_unknown_id_updates() {
        let (app, _t) = app();
        assert!(save_prompt_impl(&app, SavePromptRequest {
            id: None, name: "   ".into(), directive_md: "d".into(), output: Envelope::Sectioned,
        }).is_err(), "blank name rejected");
        assert!(save_prompt_impl(&app, SavePromptRequest {
            id: Some("ghost".into()), name: "x".into(), directive_md: "d".into(), output: Envelope::Sectioned,
        }).is_err(), "updating a nonexistent prompt rejected");
        // Deleting an unknown id is a no-op, not a crash.
        delete_prompt_impl(&app, "ghost").unwrap();
        assert_eq!(load_prompts(&app).unwrap().len(), 6);
    }

    #[test]
    fn builtin_guards_and_daisy_override_rules() {
        let (app, _t) = app();
        // No built-in is deletable; non-daisy built-ins are not editable at all.
        assert!(delete_prompt_impl(&app, DAISY_ID).is_err());
        assert!(save_prompt_impl(
            &app,
            SavePromptRequest {
                id: Some(summarize::prompts::ZOOM_ID.into()),
                name: "x".into(),
                directive_md: "long enough to pass the minimum length gate easily".into(),
                output: Envelope::Classic,
            }
        )
        .is_err());
        // Summarizer: a too-short edit is rejected outright.
        assert!(save_prompt_impl(
            &app,
            SavePromptRequest {
                id: Some(DAISY_ID.into()),
                name: "ignored".into(),
                directive_md: "too short".into(),
                output: Envelope::Classic,
            }
        )
        .is_err());
        // A valid override saves in place (name unchanged) and survives
        // reload.
        let long = "Summarize in exactly three bullet points, then list every date mentioned.";
        let saved = save_prompt_impl(
            &app,
            SavePromptRequest {
                id: Some(DAISY_ID.into()),
                name: "ignored".into(),
                directive_md: long.into(),
                output: Envelope::Classic,
            },
        )
        .unwrap();
        assert_eq!(saved.name, "Daisy Summarizer");
        let daisy = load_prompts(&app).unwrap().into_iter().find(|p| p.id == DAISY_ID).unwrap();
        assert_eq!(daisy.directive_md, long);
        // A short on-disk copy falls back to the shipped default.
        let mut prompts = load_prompts(&app).unwrap();
        prompts.iter_mut().find(|p| p.id == DAISY_ID).unwrap().directive_md = "tampered".into();
        save_prompts(&app, &prompts).unwrap();
        let daisy = load_prompts(&app).unwrap().into_iter().find(|p| p.id == DAISY_ID).unwrap();
        assert_eq!(daisy.directive_md, seed_prompts()[0].directive_md);
    }

    #[test]
    fn reset_restores_hardcoded_directive() {
        let (app, _t) = app();
        let long = "Summarize in exactly three bullet points, then list every date mentioned.";
        save_prompt_impl(
            &app,
            SavePromptRequest {
                id: Some(DAISY_ID.into()),
                name: "ignored".into(),
                directive_md: long.into(),
                output: Envelope::Classic,
            },
        )
        .unwrap();
        let reset = reset_prompt_impl(&app, DAISY_ID).unwrap();
        assert_eq!(reset.directive_md, seed_prompts()[0].directive_md);
        // User prompts and unknown ids can't be reset.
        assert!(reset_prompt_impl(&app, "u-nope").is_err());
    }

    #[test]
    fn daisy_is_always_first() {
        let (app, _t) = app();
        // Force a file where daisy is last, then reload.
        let mut prompts = load_prompts(&app).unwrap();
        prompts.rotate_left(1);
        assert_ne!(prompts[0].id, DAISY_ID);
        save_prompts(&app, &prompts).unwrap();
        let reloaded = load_prompts(&app).unwrap();
        assert_eq!(reloaded[0].id, DAISY_ID);
    }

    #[test]
    fn resolve_defaults_to_daisy_when_unknown() {
        let (app, _t) = app();
        let got = resolve_prompt(&app, Some("nope")).unwrap();
        assert_eq!(got.id, DAISY_ID);
    }

    #[test]
    fn delete_default_user_style_falls_back_to_daisy() {
        let (app, _t) = app();
        let created = save_prompt_impl(
            &app,
            SavePromptRequest {
                id: None,
                name: "Temp".into(),
                directive_md: "d".into(),
                output: Envelope::Sectioned,
            },
        )
        .unwrap();
        let sp = app.profile.settings_path();
        let mut s = crate::settings::Settings::load_or_default(&sp);
        s.default_summary_prompt_id = Some(created.id.clone());
        s.save(&sp).unwrap();
        delete_prompt_impl(&app, &created.id).unwrap();
        let s = crate::settings::Settings::load_or_default(&sp);
        assert_eq!(s.default_summary_prompt_id.as_deref(), Some(DAISY_ID));
    }
}
