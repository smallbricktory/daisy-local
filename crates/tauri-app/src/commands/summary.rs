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
    let p = session_dir(app, sid).join("manifest.json");
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

fn read_transcript_md(app: &AppState, sid: &str) -> Result<String> {
    syncsafe::read_to_string(session_dir(app, sid).join("transcript.md")).map_err(|_| {
        AppError::Config(format!(
            "session {sid}: no transcript.md — run transcribe + dedup first"
        ))
    })
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
        .or_else(|| vault.and_then(|c| c.base_url.clone()))
        .unwrap_or_else(|| d.base_url.into());
    // A bare-host LM Studio/Ollama base URL gets `/v1` appended.
    let base_url = providers_http::normalize_compat_base(provider.as_str(), &base_url);
    let model = model_override
        .or_else(|| vault.and_then(|c| c.model.clone()))
        .unwrap_or_else(|| d.model.into());
    Ok((api_key, base_url, model))
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
) -> Result<Box<dyn summarize::chat::ChatCompleter>> {
    if provider == ProviderId::DaisyGateway {
        let c = gateway.ok_or_else(|| {
            AppError::Config("Daisy Cloud isn't set up — select it in Settings → Providers.".into())
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
    )
        .map_err(|e| AppError::Config(format!("chat completer: {e}")))
}

/// Map an LLM error to an `AppError`. A gateway `404` becomes
/// `GatewayNotEntitled`; any other error becomes a `Config` error labeled
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

/// Build the gateway credentials for `task` from the decrypted vault keys and
/// the install record. Errors if Daisy Cloud isn't set up or unlicensed.
pub(crate) fn gateway_creds_from_keys(
    keys: &crate::state::DecryptedKeys,
    task: &str,
) -> Result<summarize::gateway::GatewayCreds> {
    let seed = crate::state::install_seed(keys).ok_or_else(|| {
        AppError::Config("Daisy Cloud isn't set up — select it in Settings → Providers.".into())
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

/// Resolve the user's configured AI provider into a chat completer, pulling
/// its vault creds. Returns an error when no provider is configured.
pub(crate) fn build_ai_completer(
    settings: &crate::settings::Settings,
    vs: &VaultState,
    task: &str,
) -> Result<Box<dyn summarize::chat::ChatCompleter>> {
    let provider = settings.default_summary_provider.ok_or_else(|| {
        AppError::Config("No AI provider configured. Pick one in Settings → Providers.".into())
    })?;
    let (provider_cfg, gateway) = {
        let g = vs.keys.lock().unwrap();
        let keys = g.as_ref().ok_or(AppError::VaultLocked)?;
        let cfg = keys.providers.get(&provider).cloned();
        let gw = if provider == ProviderId::DaisyGateway {
            Some(gateway_creds_from_keys(keys, task)?)
        } else {
            None
        };
        (cfg, gw)
    };
    build_chat_completer_for(provider, provider_cfg.as_ref(), gateway)
}

fn build_summarizer(
    provider: ProviderId,
    model_override: Option<String>,
    base_url_override: Option<String>,
    vault: Option<&ProviderConfig>,
    gateway: Option<summarize::gateway::GatewayCreds>,
) -> Result<Box<dyn Summarizer>> {
    if provider == ProviderId::DaisyGateway {
        let c = gateway.ok_or_else(|| {
            AppError::Config("Daisy Cloud isn't set up — select it in Settings → Providers.".into())
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
        ))),
        ProviderId::LmStudio => Ok(Box::new(summarize::openai_compat::OpenAICompatSummarizer::new(
            "lm_studio",
            base_url,
            api_key,
            model,
        ))),
        ProviderId::Ollama => Ok(Box::new(summarize::openai_compat::OpenAICompatSummarizer::new(
            "ollama",
            base_url,
            None,
            model,
        ))),
        ProviderId::Groq => Ok(Box::new(summarize::openai_compat::OpenAICompatSummarizer::new(
            "groq",
            base_url,
            Some(require_key(provider, api_key)?),
            model,
        ))),
        ProviderId::DaisyGateway => unreachable!("DaisyGateway handled before this match"),
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
    match syncsafe::read(session_dir(app, sid).join("summary.json")) {
        Ok(b) => Ok(Some(serde_json::from_slice(&b).map_err(|e| {
            AppError::Config(format!("parse summary.json: {e}"))
        })?)),
        Err(_) => Ok(None),
    }
}

fn write_summary(app: &AppState, sid: &str, s: &SessionSummary) -> Result<()> {
    let d = session_dir(app, sid);
    let jp = d.join("summary.json");
    let jt = jp.with_extension("json.tmp");
    syncsafe::write(&jt, serde_json::to_vec_pretty(s)?)?;
    syncsafe::rename(&jt, &jp)?;
    if !s.user_edited {
        let mp = d.join("summary.md");
        let mt = mp.with_extension("md.tmp");
        syncsafe::write(&mt, s.markdown.as_bytes())?;
        syncsafe::rename(&mt, &mp)?;
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
    let transcript_md = read_transcript_md(app, &req.session_id)?;
    let notes_md = crate::commands::meeting::session_notes_load_impl(app, &req.session_id)?;
    let provider_id = req
        .provider
        .or_else(|| {
            crate::settings::Settings::load_or_default(&app.profile.settings_path())
                .default_summary_provider
        })
        .ok_or_else(|| AppError::Config(
            "No AI provider configured. Pick one in Settings → Providers, or copy the transcript and paste it into any LLM yourself.".into(),
        ))?;

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
    let (provider_cfg, gateway_creds): (
        Option<ProviderConfig>,
        Option<summarize::gateway::GatewayCreds>,
    ) = {
        let g = vs.keys.lock().unwrap();
        let keys = g.as_ref().ok_or(AppError::VaultLocked)?;
        let cfg = keys.providers.get(&provider_id).cloned();
        let gw = if provider_id == ProviderId::DaisyGateway {
            Some(gateway_creds_from_keys(keys, "summary")?)
        } else {
            None
        };
        (cfg, gw)
    };

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
        let settings_path = app.profile.root().join("settings.json");
        let settings = crate::settings::Settings::load_or_default(&settings_path);
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
            if existing.source_inputs_hash == new_hash {
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
    )?;
    let summary = summarizer
        .summarize(&input, now_unix())
        .map_err(|e| map_gateway_err(provider_id, "summarize", e))?;
    write_summary(app, &req.session_id, &summary)?;
    Ok(summary)
}

pub fn summary_save_edit_impl(app: &AppState, sid: &str, markdown: &str) -> Result<()> {
    let mut s = summary_load_impl(app, sid)?
        .ok_or_else(|| AppError::Config("no summary to edit — generate one first".into()))?;
    s.markdown = markdown.to_string();
    s.user_edited = true;
    // write_summary skips summary.md when user_edited=true; the edit is
    // written to summary.md here.
    write_summary(app, sid, &s)?;
    let mp = session_dir(app, sid).join("summary.md");
    let mt = mp.with_extension("md.tmp");
    syncsafe::write(&mt, markdown.as_bytes())?;
    syncsafe::rename(&mt, &mp)?;
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

#[derive(serde::Serialize)]
pub struct SummaryProviderStatus {
    pub state: &'static str, // "Configured" | "Missing" | "VaultLocked" | "Unreachable" | "None"
    pub provider: Option<crate::state::ProviderId>,
    pub hint: Option<String>,
}

pub fn summary_provider_status_impl(
    provider: crate::state::ProviderId,
    keys: Option<&crate::state::DecryptedKeys>,
) -> SummaryProviderStatus {
    use crate::state::ProviderId::*;
    let keys = match keys {
        Some(k) => k,
        None => {
            return SummaryProviderStatus {
                state: "VaultLocked",
                provider: Some(provider),
                hint: Some("Unlock the vault to check provider status.".into()),
            };
        }
    };

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
                    hint: Some(format!("Add an API key for {provider} in Settings → Providers.")),
                }
            }
        }
        LmStudio | Ollama => {
            let has_url = cfg
                .and_then(|c| c.base_url.as_ref())
                .map(|s| !s.is_empty())
                .unwrap_or(false);
            if has_url {
                SummaryProviderStatus { state: "Configured", provider: Some(provider), hint: None }
            } else {
                SummaryProviderStatus {
                    state: "Missing",
                    provider: Some(provider),
                    hint: Some(format!("Set a base URL for {provider} in Settings → Providers.")),
                }
            }
        }
        // Daisy Cloud has no api key or base url; always "Configured" once
        // selected. Entitlement is checked server-side at call time.
        DaisyGateway => SummaryProviderStatus {
            state: "Configured",
            provider: Some(provider),
            hint: None,
        },
    }
}

pub fn summary_provider_status_impl_from_state(
    app: &crate::state::AppState,
    vs: &crate::state::VaultState,
) -> SummaryProviderStatus {
    let settings = crate::settings::Settings::load_or_default(&app.profile.settings_path());
    let provider = match settings.default_summary_provider {
        Some(p) => p,
        None => return SummaryProviderStatus {
            state: "None",
            provider: None,
            hint: Some("No AI provider selected. Pick one in Settings → Providers, or use the Copy buttons to paste into an LLM yourself.".into()),
        },
    };
    let vault = vs.keys.lock().unwrap();
    summary_provider_status_impl(provider, vault.as_ref())
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
    use crate::state::{DecryptedKeys, ProviderConfig, ProviderId};

    fn cfg_with_key(k: &str) -> ProviderConfig {
        ProviderConfig {
            api_key: Some(k.into()),
            model: None,
            base_url: None,
        }
    }

    fn cfg_with_url(u: &str) -> ProviderConfig {
        ProviderConfig {
            api_key: None,
            model: None,
            base_url: Some(u.into()),
        }
    }

    #[test]
    fn vault_locked_returns_vault_locked() {
        let s = summary_provider_status_impl(ProviderId::Anthropic, None);
        assert_eq!(s.state, "VaultLocked");
    }

    #[test]
    fn anthropic_without_key_returns_missing() {
        let keys = DecryptedKeys::default();
        let s = summary_provider_status_impl(ProviderId::Anthropic, Some(&keys));
        assert_eq!(s.state, "Missing");
    }

    #[test]
    fn anthropic_with_key_returns_configured() {
        let mut keys = DecryptedKeys::default();
        keys.providers.insert(ProviderId::Anthropic, cfg_with_key("sk-x"));
        let s = summary_provider_status_impl(ProviderId::Anthropic, Some(&keys));
        assert_eq!(s.state, "Configured");
    }

    #[test]
    fn lm_studio_without_endpoint_returns_missing() {
        let keys = DecryptedKeys::default();
        let s = summary_provider_status_impl(ProviderId::LmStudio, Some(&keys));
        assert_eq!(s.state, "Missing");
    }

    #[test]
    fn lm_studio_with_endpoint_returns_configured() {
        let mut keys = DecryptedKeys::default();
        keys.providers.insert(ProviderId::LmStudio, cfg_with_url("http://localhost:1234/v1"));
        let s = summary_provider_status_impl(ProviderId::LmStudio, Some(&keys));
        assert_eq!(s.state, "Configured");
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
            sent_integration_ids: vec![], cluster_sides: vec![], interrupted: false,
            denoise_applied: None,
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
            ProviderConfig { api_key: None, model: None, base_url: Some(server_url.to_string()) },
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
