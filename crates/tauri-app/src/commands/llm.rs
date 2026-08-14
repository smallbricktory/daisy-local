//! One provider + credential resolver for every LLM feature (summary,
//! chapters, analysis, polish, live chat, Q&A), plus the shared mapping
//! from free-text transport failures to user-facing errors.

use crate::error::{AppError, Result};
use crate::state::{AppState, ProviderConfig, ProviderId, VaultState};

/// A resolved provider: which one, its vault entry (if any), and signed
/// gateway credentials when the provider is Daisy Cloud.
pub struct Resolved {
    pub provider: ProviderId,
    pub cfg: Option<ProviderConfig>,
    pub gateway: Option<summarize::gateway::GatewayCreds>,
}

impl Resolved {
    /// Build the structured-JSON chat completer for this provider.
    pub fn chat_completer(&self) -> Result<Box<dyn summarize::chat::ChatCompleter>> {
        crate::commands::summary::build_chat_completer_for(
            self.provider,
            self.cfg.as_ref(),
            self.gateway.clone(),
        )
    }
}

/// Resolve which provider a feature call uses and pull its credentials from
/// the vault. `provider_override` wins over
/// `settings.default_summary_provider`; `task` labels gateway requests
/// (`X-Daisy-Task`). A locked vault is always a hard [`AppError::VaultLocked`].
pub fn resolve_provider(
    app: &AppState,
    vs: &VaultState,
    provider_override: Option<ProviderId>,
    task: &str,
) -> Result<Resolved> {
    let provider = match provider_override {
        Some(p) => p,
        None => crate::settings::Settings::load_or_default(&app.profile.settings_path())
            .default_summary_provider
            .ok_or_else(|| {
                AppError::Config(
                    "No AI provider configured. Pick one in Settings → Providers.".into(),
                )
            })?,
    };
    let g = vs.keys.lock().unwrap();
    let keys = g.as_ref().ok_or(AppError::VaultLocked)?;
    let cfg = keys.providers.get(&provider).cloned();
    let gateway = if provider == ProviderId::DaisyGateway {
        Some(crate::commands::summary::gateway_creds_from_keys(keys, task)?)
    } else {
        None
    };
    Ok(Resolved { provider, cfg, gateway })
}

/// Map a free-text transport failure to a user-facing error: gateway 404 →
/// [`AppError::GatewayNotEntitled`]; connection failures name what couldn't
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
