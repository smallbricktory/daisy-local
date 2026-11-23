// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! What first-run setup does on this machine: the built-in summary model
//! where the hardware allows it, this OS's floor transcription engine, and
//! the downloads either one still needs.

use crate::commands::asr_models::{installed_for, BundledDirs};
use crate::builtin_model::Capability;
use crate::downloads::DownloadKind;
use crate::error::{AppError, Result};
use crate::state::{AppState, ProviderId};
use providers_local::registry::{self, Engine, ModelRow};
use serde::Serialize;
use std::path::Path;
use summarize::local::LOCAL_MODEL_ID;

#[cfg(test)]
mod tests {
    use super::*;

    fn dirs() -> BundledDirs {
        BundledDirs { whisper: None, nemotron: None }
    }

    fn plan(capability: Capability, os: &str, sa_ready: bool, models: &Path) -> OnboardingPlan {
        plan_on(capability, models, os, sa_ready, &dirs(), "/profile")
    }

    #[test]
    fn a_fast_machine_plans_the_builtin_model() {
        let tmp = tempfile::tempdir().unwrap();
        let p = plan(Capability::Fast, "linux", false, tmp.path());
        assert_eq!(p.ai, AiPlan::Builtin { capability: Capability::Fast, installed: false });
        assert_eq!(p.profile_dir, "/profile");
    }

    #[test]
    fn a_machine_without_a_gpu_plans_no_ai() {
        let tmp = tempfile::tempdir().unwrap();
        let p = plan(Capability::Unsupported, "windows", false, tmp.path());
        assert_eq!(p.ai, AiPlan::None { reason: UNSUPPORTED_REASON.into() });
    }

    #[test]
    fn the_builtin_plan_reports_whether_the_model_is_installed() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(
            plan(Capability::Fast, "linux", false, tmp.path()).ai,
            AiPlan::Builtin { capability: Capability::Fast, installed: false }
        );
        let row = crate::builtin_model::row();
        let d = tmp.path().join("llm").join(LOCAL_MODEL_ID);
        std::fs::create_dir_all(&d).unwrap();
        for (n, _, size) in row.files {
            std::fs::File::create(d.join(n)).unwrap().set_len(*size).unwrap();
        }
        assert_eq!(
            plan(Capability::Fast, "linux", false, tmp.path()).ai,
            AiPlan::Builtin { capability: Capability::Fast, installed: true }
        );
    }

    #[test]
    fn an_unsupported_machine_plans_no_ai_and_names_the_hardware() {
        let tmp = tempfile::tempdir().unwrap();
        let p = plan(Capability::Unsupported, "linux", false, tmp.path());
        assert_eq!(
            p.ai,
            AiPlan::None { reason: "needs a graphics card with 6 GB of memory or more".into() }
        );
    }

    #[test]
    fn macos_plans_speech_analyzer_with_its_readiness() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(
            plan(Capability::Fast, "macos", true, tmp.path()).transcription,
            AsrPlan::SpeechAnalyzer { ready: true }
        );
        assert_eq!(
            plan(Capability::Fast, "macos", false, tmp.path()).transcription,
            AsrPlan::SpeechAnalyzer { ready: false }
        );
    }

    #[test]
    fn windows_and_linux_plan_nemotron_with_its_install_state() {
        let tmp = tempfile::tempdir().unwrap();
        for os in ["windows", "linux"] {
            assert_eq!(
                plan(Capability::Fast, os, false, tmp.path()).transcription,
                AsrPlan::Nemotron { installed: false }
            );
        }
        let row = registry::row(NEMOTRON_ID).unwrap();
        let d = tmp.path().join("nemotron").join(NEMOTRON_ID);
        std::fs::create_dir_all(&d).unwrap();
        for (n, _, size) in row.files {
            std::fs::File::create(d.join(n)).unwrap().set_len(*size).unwrap();
        }
        assert_eq!(
            plan(Capability::Fast, "linux", false, tmp.path()).transcription,
            AsrPlan::Nemotron { installed: true }
        );
    }

    fn test_app() -> (crate::state::AppState, tempfile::TempDir) {
        let tmp = tempfile::tempdir().unwrap();
        let profile = crate::profile::ProfileDir::at(tmp.path()).unwrap();
        (crate::state::AppState::new(profile), tmp)
    }

    fn builtin_plan() -> OnboardingPlan {
        OnboardingPlan {
            ai: AiPlan::Builtin { capability: Capability::Fast, installed: false },
            transcription: AsrPlan::Nemotron { installed: false },
            profile_dir: "/profile".into(),
        }
    }

    #[test]
    fn apply_queues_the_builtin_model_and_selects_the_floor_engine() {
        for os in ["linux", "windows"] {
            let (app, _tmp) = test_app();
            let tmp = tempfile::tempdir().unwrap();
            let p = plan_on(Capability::Fast, tmp.path(), os, false, &dirs(), "/profile");
            apply_on(&app, &p).unwrap();
            let m = app.machine_settings();
            assert_eq!(m.ai_provider, None, "a model still to download is not selected yet");
            assert_eq!(m.asr.active.as_deref(), Some(NEMOTRON_ID));
            let ids: Vec<String> = app.downloads.status().into_iter().map(|j| j.id).collect();
            assert!(ids.contains(&summarize::local::LOCAL_MODEL_ID.to_string()));
            assert!(ids.contains(&NEMOTRON_ID.to_string()));
        }
    }

    #[test]
    fn apply_persists_settings_even_when_enqueue_fails() {
        let (app, _tmp) = test_app();
        // A directory at the queue file's path makes every enqueue fail.
        let queue_path = crate::downloads::queue_path(&app.profile.settings_path());
        std::fs::create_dir_all(&queue_path).unwrap();
        assert!(apply_on(&app, &builtin_plan()).is_err());
        let m = app.machine_settings();
        assert_eq!(m.asr.active.as_deref(), Some(NEMOTRON_ID));
    }

    #[test]
    fn apply_selects_a_builtin_model_already_installed() {
        let (app, _tmp) = test_app();
        let plan = OnboardingPlan {
            ai: AiPlan::Builtin { capability: Capability::Fast, installed: true },
            transcription: AsrPlan::Nemotron { installed: true },
            profile_dir: "/profile".into(),
        };
        apply_on(&app, &plan).unwrap();
        assert_eq!(app.machine_settings().ai_provider, Some(Some(crate::state::ProviderId::Local)));
        assert!(app.downloads.status().is_empty());
    }

    #[test]
    fn apply_twice_queues_nothing_new_and_changes_nothing() {
        let (app, _tmp) = test_app();
        apply_on(&app, &builtin_plan()).unwrap();
        let first = app.downloads.status().len();
        let settings = app.machine_settings();
        apply_on(&app, &builtin_plan()).unwrap();
        assert_eq!(app.downloads.status().len(), first);
        let again = app.machine_settings();
        assert_eq!(again.ai_provider, settings.ai_provider);
        assert_eq!(again.asr.active, settings.asr.active);
    }

    #[test]
    fn apply_without_ai_queues_the_engine_alone() {
        let (app, _tmp) = test_app();
        let plan = OnboardingPlan {
            ai: AiPlan::None { reason: UNSUPPORTED_REASON.into() },
            transcription: AsrPlan::Nemotron { installed: false },
            profile_dir: "/profile".into(),
        };
        apply_on(&app, &plan).unwrap();
        assert_eq!(app.machine_settings().ai_provider, None);
        let ids: Vec<String> = app.downloads.status().into_iter().map(|j| j.id).collect();
        assert_eq!(ids, vec![NEMOTRON_ID.to_string()]);
    }

    #[test]
    fn apply_queues_the_os_assets_when_speech_analyzer_is_not_ready() {
        let (app, _tmp) = test_app();
        let plan = OnboardingPlan {
            ai: AiPlan::None { reason: UNSUPPORTED_REASON.into() },
            transcription: AsrPlan::SpeechAnalyzer { ready: false },
            profile_dir: "/profile".into(),
        };
        apply_on(&app, &plan).unwrap();
        assert_eq!(app.machine_settings().asr.active.as_deref(), Some(SA_ID));
        let ids: Vec<String> = app.downloads.status().into_iter().map(|j| j.id).collect();
        assert_eq!(ids, vec![SA_ID.to_string()]);
    }

    #[test]
    fn an_os_with_no_floor_row_of_its_own_falls_back_to_nemotron() {
        assert_eq!(floor_row_of("macos").map(|r| r.id), Some(SA_ID));
        assert_eq!(floor_row_of("windows").map(|r| r.id), Some(NEMOTRON_ID));
        assert_eq!(floor_row_of("freebsd").map(|r| r.id), None);
        assert_eq!(floor_row("freebsd").id, NEMOTRON_ID);
    }

    #[test]
    fn apply_queues_nothing_for_a_ready_engine() {
        let (app, _tmp) = test_app();
        let plan = OnboardingPlan {
            ai: AiPlan::None { reason: UNSUPPORTED_REASON.into() },
            transcription: AsrPlan::SpeechAnalyzer { ready: true },
            profile_dir: "/profile".into(),
        };
        apply_on(&app, &plan).unwrap();
        assert!(app.downloads.status().is_empty());
    }
}

/// What the AI row says when the machine cannot run the built-in model.
pub const UNSUPPORTED_REASON: &str = "needs a graphics card with 6 GB of memory or more";
/// Registry id of the Windows/Linux floor transcription row.
pub const NEMOTRON_ID: &str = providers_local::registry::NEMOTRON_ID;
/// Registry id of the macOS floor transcription row.
pub const SA_ID: &str = "speechanalyzer";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AiPlan {
    Builtin { capability: Capability, installed: bool },
    None { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "engine", rename_all = "snake_case")]
pub enum AsrPlan {
    SpeechAnalyzer { ready: bool },
    Nemotron { installed: bool },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OnboardingPlan {
    pub ai: AiPlan,
    pub transcription: AsrPlan,
    pub profile_dir: String,
}

/// The row this OS names as its floor, or `None` when no row claims it.
fn floor_row_of(os: &str) -> Option<&'static ModelRow> {
    registry::REGISTRY.iter().find(|r| r.floor_on.contains(&os))
}

/// The transcription row this OS ships as its floor.
fn floor_row(os: &str) -> &'static ModelRow {
    floor_row_of(os).unwrap_or_else(|| {
        let row = registry::row(NEMOTRON_ID).expect("registry has the nemotron row");
        log::warn!("no floor transcription row claims os {os}; using {}", row.id);
        row
    })
}

/// The plan with hardware, platform, SpeechAnalyzer readiness and bundled
/// dirs as inputs.
fn plan_on(
    capability: Capability,
    models_dir: &Path,
    os: &str,
    sa_ready: bool,
    bundled: &BundledDirs,
    profile_dir: &str,
) -> OnboardingPlan {
    let ai = match capability {
        Capability::Fast => AiPlan::Builtin {
            capability,
            installed: crate::builtin_model::installed(models_dir),
        },
        Capability::Unsupported => AiPlan::None { reason: UNSUPPORTED_REASON.to_string() },
    };
    let row = floor_row(os);
    let transcription = match row.engine {
        Engine::SpeechAnalyzer => AsrPlan::SpeechAnalyzer { ready: sa_ready },
        _ => AsrPlan::Nemotron {
            installed: installed_for(row, models_dir, sa_ready, bundled).is_some(),
        },
    };
    OnboardingPlan { ai, transcription, profile_dir: profile_dir.to_string() }
}

/// What first-run setup will do on this machine.
pub fn onboarding_plan_impl(app: &AppState) -> OnboardingPlan {
    plan_on(
        crate::builtin_model::capability(app),
        &app.profile.models_dir(),
        std::env::consts::OS,
        providers_local::macos::sa_ready(),
        &BundledDirs::from_env(),
        &app.profile.root().display().to_string(),
    )
}

/// Writes the plan into this machine's settings and queues whatever it still needs to download. Running it again queues nothing new.
fn apply_on(app: &AppState, plan: &OnboardingPlan) -> Result<()> {
    if let AiPlan::Builtin { installed: true, .. } = plan.ai {
        app.set_ai_provider(Some(ProviderId::Local))
            .map_err(|e| AppError::Io(format!("save settings: {e}")))?;
    }
    let asr_id = match plan.transcription {
        AsrPlan::SpeechAnalyzer { .. } => SA_ID,
        AsrPlan::Nemotron { .. } => NEMOTRON_ID,
    };
    app.update_machine_settings(|m| m.asr.active = Some(asr_id.to_string()))
        .map_err(|e| AppError::Io(format!("save settings: {e}")))?;
    match plan.transcription {
        AsrPlan::SpeechAnalyzer { ready } => {
            if !ready && !app.downloads.is_pending(SA_ID) {
                app.downloads.enqueue(DownloadKind::SaAssets, SA_ID)?;
            }
        }
        AsrPlan::Nemotron { installed } => {
            if !installed && !app.downloads.is_pending(NEMOTRON_ID) {
                app.downloads.enqueue(DownloadKind::Asr, NEMOTRON_ID)?;
            }
        }
    }
    if let AiPlan::Builtin { installed, .. } = plan.ai {
        if !installed && !app.downloads.is_pending(LOCAL_MODEL_ID) {
            app.downloads.enqueue(DownloadKind::Llm, LOCAL_MODEL_ID)?;
        }
    }
    Ok(())
}

/// Applies the plan and returns it.
pub fn onboarding_apply_impl(app: &AppState) -> Result<OnboardingPlan> {
    let plan = onboarding_plan_impl(app);
    apply_on(app, &plan)?;
    Ok(plan)
}

/// Marks first-run setup finished: the wizard's "Start using Daisy" button.
pub fn onboarding_finish_impl() -> Result<()> {
    crate::bootstrap::Onboarding::mark_complete().map_err(|e| AppError::Io(e.to_string()))
}
