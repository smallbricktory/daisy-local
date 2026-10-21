// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! One provider + credential resolver for every LLM feature (summary,
//! chapters, analysis, polish, live chat, Q&A), plus the shared mapping
//! from free-text transport failures to user-facing errors.

use crate::error::{AppError, Result};
use crate::state::{AppState, ProviderConfig, ProviderId, VaultState};

/// A resolved provider: which one, its vault entry (if any), and signed
/// credentials for the signed-request provider.
pub struct Resolved {
    pub provider: ProviderId,
    pub cfg: Option<ProviderConfig>,
    pub gateway: Option<summarize::gateway::GatewayCreds>,
    /// The process-wide built-in-model chat when the provider is `Local`.
    pub local: Option<std::sync::Arc<summarize::local::LocalChat>>,
}

impl Resolved {
    /// The context budget of a local endpoint; `None` for cloud providers.
    pub fn context_budget(&self) -> Option<summarize::ContextBudget> {
        crate::commands::summary::context_budget(self.provider, self.cfg.as_ref())
    }

    /// Build the structured-JSON chat completer for this provider.
    pub fn chat_completer(&self) -> Result<Box<dyn summarize::chat::ChatCompleter>> {
        crate::commands::summary::build_chat_completer_for(
            self.provider,
            self.cfg.as_ref(),
            self.gateway.clone(),
            self.local.as_ref(),
        )
    }
}

/// Resolve which provider a feature call uses and pull its credentials from
/// the vault. `provider_override` wins over this machine's provider
/// ([`AppState::ai_provider`]); `task` labels signed requests
/// (`X-Daisy-Task`). A locked vault is a hard [`AppError::VaultLocked`] for
/// every provider, the built-in model included.
pub fn resolve_provider(
    app: &AppState,
    vs: &VaultState,
    provider_override: Option<ProviderId>,
    task: &str,
) -> Result<Resolved> {
    resolve_optional(app, vs, provider_override, None, task)?.ok_or_else(|| {
        AppError::Config(format!(
            "No AI provider configured. Set one up in {}.",
            crate::settings::providers_hint()
        ))
    })
}

/// [`resolve_provider`] with no configured provider reported as `None` rather
/// than an error, for features that skip quietly. `profile` is this profile's
/// settings when the caller already holds them loaded.
pub fn resolve_optional(
    app: &AppState,
    vs: &VaultState,
    provider_override: Option<ProviderId>,
    profile: Option<&crate::settings::Settings>,
    task: &str,
) -> Result<Option<Resolved>> {
    let Some(provider) = provider_override.or_else(|| app.ai_provider_in(profile)) else {
        return Ok(None);
    };
    // The built-in model has no vault entry; its owner hands out the chat.
    if provider == ProviderId::Local {
        let local = Some(crate::builtin_model::chat(app, vs)?);
        return Ok(Some(Resolved { provider, cfg: None, gateway: None, local }));
    }
    let g = vs.keys.lock().unwrap();
    let keys = g.as_ref().ok_or(AppError::VaultLocked)?;
    let cfg = keys.providers.get(&provider).cloned();
    let gateway = if provider == ProviderId::DaisyGateway {
        Some(crate::commands::summary::gateway_creds_from_keys(keys, task)?)
    } else {
        None
    };
    Ok(Some(Resolved { provider, cfg, gateway, local: None }))
}

/// Map a free-text transport failure to a user-facing error: a 404 from the
/// signed-request provider → [`AppError::GatewayNotEntitled`]; connection failures name what couldn't
/// be reached; anything else carries the friendly HTTP one-liner.
pub(crate) fn map_send_failure(
    gateway: bool,
    feature: &str,
    f: summarize::http::SendFailure,
) -> AppError {
    use summarize::http::SendFailure;
    match f {
        SendFailure::Transport(e) if gateway => {
            AppError::Provider(format!("{feature} failed: couldn't reach Daisy Cloud. {e}"))
        }
        SendFailure::Transport(e) => {
            AppError::Provider(format!("{feature} failed: couldn't reach the AI provider. {e}"))
        }
        SendFailure::Status { status: 404, .. } if gateway => AppError::GatewayNotEntitled,
        SendFailure::Status { status, endpoint, raw } => AppError::Provider(
            summarize::error::friendly_http_error(feature, &endpoint, status, &raw),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::DecryptedKeys;
    use summarize::http::SendFailure;

    fn app() -> (AppState, tempfile::TempDir) {
        let tmp = tempfile::tempdir().unwrap();
        let profile = crate::profile::ProfileDir::at(tmp.path()).unwrap();
        (AppState::new(profile), tmp)
    }

    #[test]
    fn locked_vault_is_a_hard_error_even_with_override() {
        let (app, _t) = app();
        let vs = VaultState::default();
        let e = resolve_provider(&app, &vs, Some(ProviderId::Ollama), "summary")
            .err()
            .expect("locked vault must error");
        assert!(matches!(e, AppError::VaultLocked));
    }

    #[test]
    fn no_provider_configured_errors() {
        let (app, _t) = app();
        let vs = VaultState::default();
        *vs.keys.lock().unwrap() = Some(DecryptedKeys::default());
        let e = resolve_provider(&app, &vs, None, "summary")
            .err()
            .expect("missing provider must error");
        assert!(matches!(e, AppError::Config(m) if m.contains("No AI provider configured")));
    }

    #[test]
    fn override_resolves_vault_entry() {
        let (app, _t) = app();
        let vs = VaultState::default();
        let mut keys = DecryptedKeys::default();
        keys.providers.insert(
            ProviderId::LmStudio,
            ProviderConfig {
                api_key: None,
                model: Some("m1".into()),
                base_url: Some("http://localhost:1234/v1".into()),
                context_tokens: None,
            },
        );
        *vs.keys.lock().unwrap() = Some(keys);
        let r = resolve_provider(&app, &vs, Some(ProviderId::LmStudio), "ask").unwrap();
        assert_eq!(r.provider, ProviderId::LmStudio);
        assert_eq!(r.cfg.as_ref().and_then(|c| c.model.as_deref()), Some("m1"));
        assert!(r.gateway.is_none());
    }

    #[test]
    fn gateway_404_maps_to_not_entitled_only_for_gateway() {
        let status = |s: u16| SendFailure::Status {
            status: s,
            endpoint: "/api/gateway/v1/chat/completions".into(),
            raw: String::new(),
        };
        assert!(matches!(
            map_send_failure(true, "Q&A", status(404)),
            AppError::GatewayNotEntitled
        ));
        assert!(matches!(
            map_send_failure(false, "Q&A", status(404)),
            AppError::Provider(_)
        ));
        assert!(matches!(
            map_send_failure(true, "Q&A", status(500)),
            AppError::Provider(_)
        ));
    }
}
