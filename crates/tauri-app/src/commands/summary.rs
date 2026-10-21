// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Summary commands: generate (hash-skip), load, save user edit, regenerate.
//! Builds a Summarizer from the requested provider (default: settings.default_summary_provider).

use crate::error::{AppError, Result};
use crate::state::{AppState, ProviderConfig, ProviderId, VaultState};
use recording::manifest::{Attendee, AttendeeRole, SessionManifest};
use serde::Deserialize;
use std::path::PathBuf;
use summarize::{AttendeeRef, SessionSummary, SpeakerRole, Summarizer, SummaryInput, TagPromptRef};

use crate::now_unix;

fn session_dir(app: &AppState, sid: &str) -> PathBuf {
    app.profile.session_path(sid)
}

pub(crate) fn load_manifest(app: &AppState, sid: &str) -> Result<SessionManifest> {
    let root = session_dir(app, sid);
    let p = root.join("manifest.json");
    let m: SessionManifest = serde_json::from_slice(
        &syncsafe::read(&p).map_err(|e| AppError::Config(format!("read manifest: {e}")))?,
    )
    .map_err(|e| AppError::Config(format!("parse manifest: {e}")))?;
    if !m.schema_is_supported() {
        return Err(AppError::Config(format!(
            "session {sid}: manifest schema v{} unsupported",
            m.schema_version
        )));
    }
    Ok(m)
}

pub(crate) fn attendee_to_role(a: &Attendee) -> (String, SpeakerRole) {
    (
        a.display_name.clone(),
        match a.role {
            AttendeeRole::Self_ => SpeakerRole::Me,
            AttendeeRole::Other => SpeakerRole::Them,
        },
    )
}

/// Resolve `(api_key, base_url, model)` for a summary provider, applying the
/// precedence: request override → vault entry → built-in default. Key is None
/// for local providers that don't authenticate.
pub(crate) fn resolve_summary_creds(
    provider: ProviderId,
    model_override: Option<String>,
    base_url_override: Option<String>,
    vault: Option<&ProviderConfig>,
) -> Result<(Option<String>, String, String)> {
    let d = summarize::defaults::for_provider(provider.as_str()).ok_or_else(|| {
        AppError::Config(format!(
            "{provider} isn't a summary provider. Try: anthropic, openai, lm_studio, ollama"
        ))
    })?;
    let vault_key = vault.and_then(|c| c.api_key.clone()).filter(|s| !s.is_empty());
    let env_key = if d.env_key.is_empty() {
        None
    } else {
        std::env::var(d.env_key).ok().filter(|s| !s.is_empty())
    };
    let api_key = vault_key.or(env_key);
    let base_url = base_url_override
        .filter(|s| !s.is_empty())
        .or_else(|| vault.and_then(|c| c.base_url.clone()).filter(|s| !s.is_empty()))
        .unwrap_or_else(|| d.base_url.into());
    // A bare-host LM Studio/Ollama base URL gets `/v1` appended.
    let base_url = providers_http::normalize_compat_base(provider.as_str(), &base_url);
    let model = model_override
        .or_else(|| vault.and_then(|c| c.model.clone()))
        .unwrap_or_else(|| d.model.into());
    Ok((api_key, base_url, model))
}

/// The context budget for a local endpoint: the user's entered context
/// length, or the default when blank. Cloud providers have none.
pub(crate) fn context_budget(
    provider: ProviderId,
    vault: Option<&ProviderConfig>,
) -> Option<summarize::ContextBudget> {
    match provider {
        ProviderId::LmStudio | ProviderId::Ollama => {
            Some(summarize::ContextBudget::new(vault.and_then(|c| c.context_tokens)))
        }
        ProviderId::Local => Some(summarize::ContextBudget::new(Some(summarize::local::LOCAL_CONTEXT_TOKENS))),
        _ => None,
    }
}

fn require_key(provider: ProviderId, key: Option<String>) -> Result<String> {
    key.ok_or_else(|| {
        let env = summarize::defaults::for_provider(provider.as_str()).map(|d| d.env_key).unwrap_or("");
        AppError::Config(format!("{env} not set and no vault entry"))
    })
}

/// Build a provider-agnostic [`summarize::chat::ChatCompleter`] for the given
/// summary provider, resolving creds the same way as `build_summarizer`. Cloud
/// providers (anthropic/openai/groq) require a key; local ones (lm_studio/
/// ollama) don't.
pub(crate) fn build_chat_completer_for(
    provider: ProviderId,
    vault: Option<&ProviderConfig>,
    gateway: Option<summarize::gateway::GatewayCreds>,
    local: Option<&std::sync::Arc<summarize::local::LocalChat>>,
) -> Result<Box<dyn summarize::chat::ChatCompleter>> {
    if provider == ProviderId::Local {
        let l = local.ok_or_else(|| {
            let hint = crate::builtin_model::download_hint();
            AppError::Config(format!("Built-in model isn't downloaded. {hint}"))
        })?;
        let budget = context_budget(provider, vault).expect("the built-in model has a budget");
        return Ok(Box::new(summarize::local::LocalCompleter::new(
            l.clone(),
            budget.output_reserve(),
        )));
    }
    if provider == ProviderId::DaisyGateway {
        let c = gateway.ok_or_else(|| {
            AppError::Config("Daisy Cloud isn't set up — select it in Settings → Recordings.".into())
        })?;
        return Ok(Box::new(summarize::chat::DaisyGatewayChat::new(
            c.install_id,
            c.license,
            c.seed,
            c.task,
        )));
    }
    let (api_key, base_url, model) = resolve_summary_creds(provider, None, None, vault)?;
    let key = match provider {
        ProviderId::Anthropic | ProviderId::Openai | ProviderId::Groq => {
            Some(require_key(provider, api_key)?)
        }
        _ => api_key,
    };
    let response_format =
        summarize::state_provider::ResponseFormat::for_provider(provider.as_str());
    summarize::chat::build_chat_completer(
        provider.chat_provider(),
        key,
        base_url,
        model,
        response_format,
        context_budget(provider, vault).map(|b| b.output_reserve()),
    )
    .map_err(|e| AppError::Config(format!("chat completer: {e}")))
}

/// Map an LLM error to an `AppError`. A `404` from the signed-request
/// provider becomes `GatewayNotEntitled`; any other error becomes a `Config` error labeled
/// with `ctx`.
pub(crate) fn map_gateway_err(
    provider: ProviderId,
    ctx: &str,
    e: summarize::SummarizeError,
) -> AppError {
    if provider == ProviderId::DaisyGateway {
        if let summarize::SummarizeError::BadStatus { status: 404, .. } = e {
            return AppError::GatewayNotEntitled;
        }
    }
    AppError::Config(format!("{ctx}: {e}"))
}

/// Build the signed-request credentials for `task` from the decrypted vault
/// keys and the install record. Errors if the provider isn't set up or unlicensed.
pub(crate) fn gateway_creds_from_keys(
    keys: &crate::state::DecryptedKeys,
    task: &str,
) -> Result<summarize::gateway::GatewayCreds> {
    let seed: [u8; 32] = crate::state::install_seed(keys)
        .and_then(|s| <[u8; 32]>::try_from(s).ok())
        .ok_or_else(|| {
            AppError::Config("Daisy Cloud isn't set up — select it in Settings → Recordings.".into())
        })?;
    let rec = crate::commands::license::InstallRecord::load_or_create()
        .map_err(|e| AppError::Config(format!("install record: {e}")))?;
    let license = rec
        .license_key
        .clone()
        .ok_or_else(|| AppError::Config("Daisy Cloud needs an active license.".into()))?;
    Ok(summarize::gateway::GatewayCreds {
        install_id: rec.install_id,
        license,
        seed,
        task: task.to_string(),
    })
}

fn build_summarizer(
    provider: ProviderId,
    model_override: Option<String>,
    base_url_override: Option<String>,
    vault: Option<&ProviderConfig>,
    gateway: Option<summarize::gateway::GatewayCreds>,
    local: Option<&std::sync::Arc<summarize::local::LocalChat>>,
) -> Result<Box<dyn Summarizer>> {
    if provider == ProviderId::Local {
        let completer = build_chat_completer_for(provider, vault, None, local)?;
        let budget = context_budget(provider, vault).expect("the built-in model has a budget");
        return Ok(Box::new(
            summarize::openai_compat::OpenAICompatSummarizer::with_completer("local", summarize::local::LOCAL_MODEL_ID.into(), completer)
                .with_budget(budget),
        ));
    }
    if provider == ProviderId::DaisyGateway {
        let c = gateway.ok_or_else(|| {
            AppError::Config("Daisy Cloud isn't set up — select it in Settings → Recordings.".into())
        })?;
        let completer: Box<dyn summarize::chat::ChatCompleter> = Box::new(
            summarize::chat::DaisyGatewayChat::new(c.install_id, c.license, c.seed, c.task),
        );
        return Ok(Box::new(
            summarize::openai_compat::OpenAICompatSummarizer::with_completer(
                "daisy_gateway",
                "daisy".into(),
                completer,
            ),
        ));
    }
    let (api_key, base_url, model) =
        resolve_summary_creds(provider, model_override, base_url_override, vault)?;
    match provider {
        ProviderId::Anthropic => Ok(Box::new(summarize::anthropic::AnthropicSummarizer::with_config(
            require_key(provider, api_key)?,
            base_url,
            model,
        ))),
        ProviderId::Openai => Ok(Box::new(summarize::openai_compat::OpenAICompatSummarizer::new(
            "openai",
            base_url,
            Some(require_key(provider, api_key)?),
            model,
            None,
        ))),
        ProviderId::LmStudio => Ok(Box::new(summarize::openai_compat::OpenAICompatSummarizer::new(
            "lm_studio",
            base_url,
            api_key,
            model,
            context_budget(provider, vault),
        ))),
        ProviderId::Ollama => Ok(Box::new(summarize::openai_compat::OpenAICompatSummarizer::new(
            "ollama",
            base_url,
            None,
            model,
            context_budget(provider, vault),
        ))),
        ProviderId::Groq => Ok(Box::new(summarize::openai_compat::OpenAICompatSummarizer::new(
            "groq",
            base_url,
            Some(require_key(provider, api_key)?),
            model,
            None,
        ))),
        ProviderId::DaisyGateway => unreachable!("DaisyGateway handled before this match"),
        ProviderId::Local => unreachable!("Local handled before this match"),
    }
}

#[derive(Debug, Deserialize)]
pub struct SummaryGenerateRequest {
    pub session_id: String,
    pub provider: Option<ProviderId>,
    pub model: Option<String>,
    pub force: Option<bool>,
    /// Summary Style to apply. `None` resolves the global default (settings →
    /// Daisy built-in). See `commands::prompts::resolve_prompt`.
    #[serde(default)]
    pub prompt_id: Option<String>,
}

pub fn summary_load_impl(app: &AppState, sid: &str) -> Result<Option<SessionSummary>> {
    let path = session_dir(app, sid).join("summary.json");
    let Ok(bytes) = syncsafe::read(&path) else {
        return Ok(None);
    };
    let summary = SessionSummary::decode(&bytes)
        .map_err(|e| AppError::Config(format!("parse summary.json: {e}")))?;
    // A file that is not v2 is rewritten in the v2 wire shape.
    if !SessionSummary::is_v2_bytes(&bytes) {
        syncsafe::write_atomic(&path, summary.encode())?;
    }
    Ok(Some(summary))
}

pub(crate) fn write_summary(app: &AppState, sid: &str, s: &SessionSummary) -> Result<()> {
    let d = session_dir(app, sid);
    syncsafe::write_atomic(&d.join("summary.json"), s.encode())?;
    if !s.user_edited {
        syncsafe::write_atomic(&d.join("summary.md"), s.markdown.as_bytes())?;
    }
    Ok(())
}

pub fn summary_generate_impl(
    app: &AppState,
    vs: &VaultState,
    req: SummaryGenerateRequest,
) -> Result<SessionSummary> {
    let force = req.force.unwrap_or(false);
    let manifest = load_manifest(app, &req.session_id)?;
    let transcript_md = crate::commands::session_store::load_transcript_md(
        &session_dir(app, &req.session_id),
        crate::commands::session_store::TranscriptSource::RequireTranscript,
    )
    .ok_or_else(|| {
        AppError::Config(format!(
            "session {}: no transcript.md — run transcribe + dedup first",
            req.session_id
        ))
    })?;
    let notes_md = crate::commands::meeting::session_notes_load_impl(app, &req.session_id)?;
    let settings = crate::settings::Settings::load_or_default(&app.profile.settings_path());
    let resolved = crate::commands::llm::resolve_optional(app, vs, req.provider, Some(&settings), "summary")?
        .ok_or_else(|| AppError::Config(format!(
            "No AI provider configured. Set one up in {} — or copy the transcript and paste it into any LLM yourself.",
            crate::settings::providers_hint(),
        )))?;
    let provider_id = resolved.provider;

    // Tag prompts + vocabulary come from plaintext tags.json; provider config
    // comes from the vault. A tag contributes if it has prose or vocabulary;
    // tuple = (name, prompt, terms).
    let tags_file = crate::commands::tags::load_tags_file(app)?;
    let tag_prompts_owned: Vec<(String, String, String)> = manifest
        .tag_ids
        .iter()
        .filter_map(|id| tags_file.tags.iter().find(|t| &t.id == id))
        .filter_map(|t| {
            let prompt = t.prompt_md.clone().unwrap_or_default();
            let terms = t
                .vocab_md
                .as_deref()
                .map(|v| crate::commands::transcribe_priming::parse_terms(v).join(", "))
                .unwrap_or_default();
            if prompt.is_empty() && terms.is_empty() {
                None
            } else {
                Some((t.name.clone(), prompt, terms))
            }
        })
        .collect();
    let (provider_cfg, gateway_creds, local_paths) = (resolved.cfg, resolved.gateway, resolved.local);

    let tag_refs: Vec<TagPromptRef> = tag_prompts_owned
        .iter()
        .map(|(n, p, terms)| TagPromptRef {
            name: n,
            prompt_md: p,
            terms,
        })
        .collect();
    let mut att_owned: Vec<(String, SpeakerRole)> =
        manifest.attendees.iter().map(attendee_to_role).collect();
    if !att_owned.iter().any(|(_, r)| matches!(r, SpeakerRole::Me)) {
        if let Some(name) = settings.user_display_name.as_ref().filter(|s| !s.trim().is_empty()) {
            att_owned.insert(0, (name.trim().to_string(), SpeakerRole::Me));
        }
    }
    let att_refs: Vec<AttendeeRef> = att_owned
        .iter()
        .map(|(n, r)| AttendeeRef {
            display_name: n,
            role: *r,
        })
        .collect();
    let prompt = crate::commands::prompts::resolve_prompt(app, req.prompt_id.as_deref())?;
    let input = SummaryInput {
        session_id: &req.session_id,
        title: manifest.title.as_deref(),
        attendees: &att_refs,
        user_notes_md: &notes_md,
        transcript_md: &transcript_md,
        tag_prompts: &tag_refs,
        style_envelope: prompt.output,
        style_directive: &prompt.directive_md,
    };

    let model_for_hash = req
        .model
        .clone()
        .or_else(|| provider_cfg.as_ref().and_then(|c| c.model.clone()))
        .unwrap_or_else(|| {
            summarize::defaults::for_provider(provider_id.as_str())
                .map(|d| d.model.to_string())
                .unwrap_or_else(|| "local-model".into())
        });
    let new_hash =
        summarize::hash::source_inputs_hash(&input, provider_id.as_str(), &model_for_hash);
    if !force {
        if let Some(existing) = summary_load_impl(app, &req.session_id)? {
            if existing.source_inputs_hash.as_deref() == Some(new_hash.as_str()) {
                return Ok(existing);
            }
        }
    }
    let summarizer = build_summarizer(
        provider_id,
        req.model.clone(),
        provider_cfg.as_ref().and_then(|c| c.base_url.clone()),
        provider_cfg.as_ref(),
        gateway_creds,
        local_paths.as_ref(),
    )?;
    let mut summary = summarizer
        .summarize(&input, now_unix())
        .map_err(|e| map_gateway_err(provider_id, "summarize", e))?;
    // Who authored it: the sync identity this desktop hands a phone.
    summary.produced_by.device = crate::sync::identity::load_or_create(app, vs)
        .map(|id| id.device_id)
        .unwrap_or_default();
    write_summary(app, &req.session_id, &summary)?;
    // A re-summarize replaces the file the chapters were folded into.
    if let Some(ch) = crate::commands::chapters::load_session_chapters_impl(app, &req.session_id)? {
        crate::commands::chapters::fold_chapters(&session_dir(app, &req.session_id), &ch.chapters)?;
        summary.chapters = summary_load_impl(app, &req.session_id)?
            .map(|s| s.chapters)
            .unwrap_or_default();
    }
    Ok(summary)
}

pub fn summary_save_edit_impl(app: &AppState, sid: &str, markdown: &str) -> Result<()> {
    let mut s = summary_load_impl(app, sid)?
        .unwrap_or_else(|| SessionSummary::hand_written(sid, "user", markdown, now_unix()));
    s.markdown = markdown.to_string();
    s.user_edited = true;
    // write_summary skips summary.md when user_edited=true; the edit is
    // written to summary.md here.
    write_summary(app, sid, &s)?;
    let mp = session_dir(app, sid).join("summary.md");
    syncsafe::write_atomic(&mp, markdown.as_bytes())?;
    Ok(())
}

/// Force-regenerate. Overwrites a user-edited summary.md; the frontend
/// confirms with the user before calling.
pub fn summary_regenerate_impl(
    app: &AppState,
    vs: &VaultState,
    sid: &str,
    prompt_id: Option<String>,
) -> Result<SessionSummary> {
    log::info!("summary regenerate: starting for {sid}");
    let r = summary_generate_impl(
        app,
        vs,
        SummaryGenerateRequest {
            session_id: sid.to_string(),
            provider: None,
            model: None,
            force: Some(true),
            prompt_id,
        },
    );
    match &r {
        Ok(_) => log::info!("summary regenerate: done for {sid}"),
        Err(e) => log::warn!("summary regenerate: failed for {sid}: {e}"),
    }
    r
}

#[derive(Clone, serde::Serialize)]
pub struct SummaryProviderStatus {
    pub state: &'static str, // "Configured" | "Missing" | "Installing" | "Unsupported" | "VaultLocked" | "Unreachable" | "None"
    pub provider: Option<crate::state::ProviderId>,
    pub hint: Option<String>,
}

/// The AI provider status every gate reads: this machine's choice (`None`
/// when there is none), the vault's keys (`None` while it is locked), whether
/// the built-in model is installed and runnable here, and whether its
/// download is queued.
pub fn provider_status(
    choice: Option<crate::state::ProviderId>,
    keys: Option<&crate::state::DecryptedKeys>,
    local: crate::builtin_model::LocalReadiness,
    installing: bool,
) -> SummaryProviderStatus {
    use crate::builtin_model::Capability;
    use crate::state::ProviderId::*;
    let still_downloading = || Some("Daisy's model is still downloading.".to_string());
    let provider = match choice {
        Some(p) => p,
        None if keys.is_some() && installing => {
            return SummaryProviderStatus { state: "Installing", provider: Some(Local), hint: still_downloading() };
        }
        None => {
            return SummaryProviderStatus {
                state: "None",
                provider: None,
                hint: Some(format!(
                    "No AI provider selected. Set one up in {} — or use the Copy buttons to paste into an LLM yourself.",
                    crate::settings::providers_hint()
                )),
            };
        }
    };
    // A locked vault stops every provider, the built-in model included.
    let Some(keys) = keys else {
        return SummaryProviderStatus {
            state: "VaultLocked",
            provider: Some(provider),
            hint: Some("Unlock the vault to use AI.".into()),
        };
    };
    // The built-in model needs no vault entry: its state is the file and the
    // hardware.
    if provider == Local {
        return match (local.capability, local.installed) {
            (Capability::Unsupported, _) => SummaryProviderStatus { state: "Unsupported", provider: Some(provider), hint: Some(Capability::Unsupported.line().into()) },
            (_, false) if installing => SummaryProviderStatus { state: "Installing", provider: Some(provider), hint: still_downloading() },
            (_, false) => SummaryProviderStatus { state: "Missing", provider: Some(provider), hint: Some(crate::builtin_model::download_hint()) },
            (_, true) => SummaryProviderStatus { state: "Configured", provider: Some(provider), hint: None },
        };
    }

    let cfg = keys.providers.get(&provider);

    match provider {
        Anthropic | Openai | Groq => {
            let has_key = cfg
                .and_then(|c| c.api_key.as_ref())
                .map(|s| !s.is_empty())
                .unwrap_or(false);
            if has_key {
                SummaryProviderStatus { state: "Configured", provider: Some(provider), hint: None }
            } else {
                SummaryProviderStatus {
                    state: "Missing",
                    provider: Some(provider),
                    hint: Some(format!(
                        "Add an API key for {provider} in {}.",
                        crate::settings::providers_hint()
                    )),
                }
            }
        }
        // No credentials, and a saved base URL is optional: the summarize
        // path falls back to the provider's well-known localhost default
        // (resolve_summary_creds). Reachability is a call-time question.
        LmStudio | Ollama => SummaryProviderStatus {
            state: "Configured",
            provider: Some(provider),
            hint: None,
        },
        // No api key or base url; always "Configured" once selected.
        // Entitlement is checked server-side at call time.
        DaisyGateway => SummaryProviderStatus {
            state: "Configured",
            provider: Some(provider),
            hint: None,
        },
        Local => unreachable!("Local handled above"),
    }
}

#[cfg(test)]
mod gateway_err_tests {
    use super::*;
    use crate::error::AppError;
    use crate::state::ProviderId;
    use summarize::SummarizeError;

    #[test]
    fn gateway_404_maps_to_not_entitled() {
        let e = SummarizeError::BadStatus { status: 404, body: "x".into() };
        assert!(matches!(
            map_gateway_err(ProviderId::DaisyGateway, "summarize", e),
            AppError::GatewayNotEntitled
        ));
    }

    #[test]
    fn non_gateway_404_stays_config() {
        let e = SummarizeError::BadStatus { status: 404, body: "x".into() };
        assert!(matches!(
            map_gateway_err(ProviderId::Openai, "summarize", e),
            AppError::Config(_)
        ));
    }

    #[test]
    fn gateway_non_404_stays_config() {
        let e = SummarizeError::BadStatus { status: 500, body: "x".into() };
        assert!(matches!(
            map_gateway_err(ProviderId::DaisyGateway, "summarize", e),
            AppError::Config(_)
        ));
    }
}

#[cfg(test)]
mod status_tests {
    use super::*;
    use crate::builtin_model::{Capability, LocalReadiness};
    use crate::state::{DecryptedKeys, ProviderConfig, ProviderId};

    fn summary_provider_status_impl(
        p: ProviderId,
        keys: Option<&DecryptedKeys>,
        local: &LocalReadiness,
        installing: bool,
    ) -> SummaryProviderStatus {
        provider_status(Some(p), keys, *local, installing)
    }

    fn cfg_with_key(k: &str) -> ProviderConfig {
        ProviderConfig {
            api_key: Some(k.into()),
            model: None,
            base_url: None,
            context_tokens: None,
        }
    }

    fn cfg_with_url(u: &str) -> ProviderConfig {
        ProviderConfig {
            api_key: None,
            model: None,
            base_url: Some(u.into()),
            context_tokens: None,
        }
    }

    #[test]
    fn vault_locked_returns_vault_locked() {
        let s = summary_provider_status_impl(ProviderId::Anthropic, None, &LocalReadiness::default(), false);
        assert_eq!(s.state, "VaultLocked");
    }

    #[test]
    fn anthropic_without_key_returns_missing() {
        let keys = DecryptedKeys::default();
        let s = summary_provider_status_impl(ProviderId::Anthropic, Some(&keys), &LocalReadiness::default(), false);
        assert_eq!(s.state, "Missing");
    }

    #[test]
    fn anthropic_with_key_returns_configured() {
        let mut keys = DecryptedKeys::default();
        keys.providers.insert(ProviderId::Anthropic, cfg_with_key("sk-x"));
        let s = summary_provider_status_impl(ProviderId::Anthropic, Some(&keys), &LocalReadiness::default(), false);
        assert_eq!(s.state, "Configured");
    }

    #[test]
    fn lm_studio_without_endpoint_is_configured_via_default_url() {
        let keys = DecryptedKeys::default();
        let s = summary_provider_status_impl(ProviderId::LmStudio, Some(&keys), &LocalReadiness::default(), false);
        assert_eq!(s.state, "Configured");
        let (_, base, _) = resolve_summary_creds(ProviderId::LmStudio, None, None, None).unwrap();
        assert_eq!(base, "http://localhost:1234/v1");
    }

    #[test]
    fn empty_saved_base_url_falls_back_to_the_default() {
        let cfg = cfg_with_url("");
        let (_, base, _) =
            resolve_summary_creds(ProviderId::Ollama, None, None, Some(&cfg)).unwrap();
        assert_eq!(base, "http://localhost:11434/v1");
    }

    #[test]
    fn lm_studio_with_endpoint_returns_configured() {
        let mut keys = DecryptedKeys::default();
        keys.providers.insert(ProviderId::LmStudio, cfg_with_url("http://localhost:1234/v1"));
        let s = summary_provider_status_impl(ProviderId::LmStudio, Some(&keys), &LocalReadiness::default(), false);
        assert_eq!(s.state, "Configured");
    }

    #[test]
    fn local_installed_and_capable_is_configured() {
        let keys = DecryptedKeys::default();
        let s = summary_provider_status_impl(
            ProviderId::Local,
            Some(&keys),
            &LocalReadiness { installed: true, capability: Capability::Fast },
            false,
        );
        assert_eq!(s.state, "Configured");
    }

    #[test]
    fn a_locked_vault_stops_the_built_in_model_too() {
        let s = summary_provider_status_impl(
            ProviderId::Local,
            None,
            &LocalReadiness { installed: true, capability: Capability::Fast },
            false,
        );
        assert_eq!(s.state, "VaultLocked");
    }

    #[test]
    fn local_missing_file_asks_for_the_download() {
        let keys = DecryptedKeys::default();
        let s = summary_provider_status_impl(
            ProviderId::Local,
            Some(&keys),
            &LocalReadiness { installed: false, capability: Capability::Fast },
            false,
        );
        assert_eq!(s.state, "Missing");
        assert!(s.hint.unwrap().contains("Download"));
    }

    #[test]
    fn no_choice_reads_installing_while_the_model_downloads_and_none_otherwise() {
        let keys = DecryptedKeys::default();
        let local = LocalReadiness { installed: false, capability: Capability::Fast };
        let s = provider_status(None, Some(&keys), local, true);
        assert_eq!((s.state, s.provider), ("Installing", Some(ProviderId::Local)));
        assert_eq!(provider_status(None, Some(&keys), local, false).state, "None");
        assert_eq!(provider_status(None, None, local, true).state, "None", "a locked vault shows no download in progress");
    }

    #[test]
    fn local_unsupported_hardware_wins_over_installed() {
        let keys = DecryptedKeys::default();
        let s = summary_provider_status_impl(
            ProviderId::Local,
            Some(&keys),
            &LocalReadiness { installed: true, capability: Capability::Unsupported },
            false,
        );
        assert_eq!(s.state, "Unsupported");
        assert_eq!(s.hint.as_deref(), Some(Capability::Unsupported.line()));
    }
}

#[cfg(test)]
mod generate_tests {
    use super::*;
    use crate::state::VaultState;
    use recording::manifest::{AecMode, Attendee, AttendeeRole, SessionManifest};

    fn app() -> (AppState, tempfile::TempDir) {
        let tmp = tempfile::tempdir().unwrap();
        let profile = crate::profile::ProfileDir::at(tmp.path()).unwrap();
        (AppState::new(profile), tmp)
    }

    fn write_session(app: &AppState, sid: &str) {
        let dir = app.profile.session_path(sid);
        syncsafe::create_dir_all(&dir).unwrap();
        let m = SessionManifest {
            recording_kind: None,
            origin_device: None,
            extra: Default::default(),
            schema_version: 2, session_id: sid.into(), created_at_unix_seconds: 0,
            sample_rate: 16000, channels: 1, mic_source_id: 1,
            mic_source_node_name: "m".into(), mic_source_description: "m".into(),
            system_source_id: 2, system_source_node_name: "s".into(),
            system_source_description: "s".into(), aec_mode: AecMode::Disabled,
            chunks: vec![], finalized_at_unix_seconds: Some(1), title: Some("Pilot sync".into()),
            meeting_id: "mid".into(), tag_ids: vec![], notes_md_relative: None,
            attendees: vec![Attendee { display_name: "Mira".into(), role: AttendeeRole::Other }],
            calendar: None, recording_segments: vec![],
            speaker_map: vec![], language: None, diarization_unavailable: false,
            single_local_speaker: true,
            expected_speakers: None,
            sent_integration_ids: vec![], cluster_sides: vec![], mixed_clusters: vec![], cluster_sample_spans: vec![], sample_spans_on_audio_timeline: false, speaker_separation: None, diarize_count: None, diarization_id: None, interrupted: false,
            denoise_applied: None,
            dismissed_suggested_tag_ids: vec![],
        };
        syncsafe::write(dir.join("manifest.json"), serde_json::to_vec(&m).unwrap()).unwrap();
        syncsafe::write(dir.join("transcript.md"), "**Mira**: trimmed the pilot scope").unwrap();
    }

    fn setup_provider(app: &AppState, server_url: &str) -> VaultState {
        let sp = app.profile.settings_path();
        let mut settings = crate::settings::Settings::load_or_default(&sp);
        settings.default_summary_provider = Some(ProviderId::LmStudio);
        settings.save(&sp).unwrap();
        let vs = VaultState::default();
        let mut keys = crate::state::DecryptedKeys::default();
        keys.providers.insert(
            ProviderId::LmStudio,
            ProviderConfig { api_key: None, model: None, base_url: Some(server_url.to_string()), context_tokens: None },
        );
        *vs.keys.lock().unwrap() = Some(keys);
        vs
    }

    fn classic_body() -> String {
        let content = serde_json::json!({
            "tldr": "Scope trimmed.",
            "action_items": [{"text": "send checklist", "owner": "you", "due": null}],
            "decisions": ["exporter out"], "open_questions": [], "key_topics": []
        }).to_string();
        serde_json::json!({"choices": [{"message": {"content": content}}]}).to_string()
    }

    fn sectioned_body() -> String {
        let content = serde_json::json!({
            "exec_summary": "Focused sync.",
            "sections": [{"heading": "Scope", "bullets": false, "content": ["The pilot list was trimmed."]}],
            "action_items": []
        }).to_string();
        serde_json::json!({"choices": [{"message": {"content": content}}]}).to_string()
    }

    #[test]
    fn generate_writes_summary_and_hash_skip_avoids_a_second_llm_call() {
        let (app, _t) = app();
        write_session(&app, "s1");
        let mut server = mockito::Server::new();
        // expect EXACTLY one hit — the second generate must hash-skip.
        let m = server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_body(classic_body())
            .expect(1)
            .create();
        let vs = setup_provider(&app, &server.url());

        let req = || SummaryGenerateRequest {
            session_id: "s1".into(), provider: None, model: None, force: None, prompt_id: None,
        };
        let first = summary_generate_impl(&app, &vs, req()).unwrap();
        assert!(first.markdown.contains("**TL;DR.** Scope trimmed."));
        assert!(first.markdown.contains("- send checklist"));
        let dir = app.profile.session_path("s1");
        assert!(dir.join("summary.json").is_file());
        assert!(dir.join("summary.md").is_file());

        // Same inputs -> same hash -> served from disk, no HTTP.
        let second = summary_generate_impl(&app, &vs, req()).unwrap();
        assert_eq!(second.source_inputs_hash, first.source_inputs_hash);
        m.assert();
    }

    #[test]
    fn prompt_id_switches_the_envelope_and_invalidates_the_cache() {
        let (app, _t) = app();
        write_session(&app, "s1");
        let mut server = mockito::Server::new();
        server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_body(sectioned_body())
            .expect(1)
            .create();
        let vs = setup_provider(&app, &server.url());

        let out = summary_generate_impl(&app, &vs, SummaryGenerateRequest {
            session_id: "s1".into(), provider: None, model: None, force: None,
            prompt_id: Some("builtin:zoom".into()),
        })
        .unwrap();
        // Sectioned rendering, not the classic TL;DR shape.
        assert!(out.markdown.contains("**Summary.** Focused sync."), "{}", out.markdown);
        assert!(out.markdown.contains("## Scope"));
        // exec_summary is folded into structured.tldr.
        assert_eq!(out.structured.tldr, "Focused sync.");
    }
}
