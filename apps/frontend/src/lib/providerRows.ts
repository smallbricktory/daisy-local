// Builds the provider rows shown in Settings → Providers. The backend's
// `list_providers` returns vault-configured providers plus a synthesized
// zero-config Daisy Cloud entry. That list is merged with a known-providers
// allowlist: every selectable provider gets a row before a key is added,
// and a synthesized provider is never silently dropped.
//
// Backend mirror: crates/tauri-app/src/commands/lifecycle.rs `list_providers_impl`.
import type { ProviderId, ProviderListEntry } from '../tauri';

// Mirror of `crates/summarize/src/defaults.rs` (the backend's fallback when
// no model is stored) — keep in lock-step.
export const PROVIDER_DEFAULTS: Partial<Record<ProviderId, { model: string; baseUrl: string | null; needsKey: boolean }>> = {
  groq:      { model: 'llama-3.3-70b-versatile',    baseUrl: 'https://api.groq.com/openai/v1', needsKey: true },
  openai:    { model: 'gpt-4o-mini',                baseUrl: 'https://api.openai.com/v1',      needsKey: true },
  anthropic: { model: 'claude-sonnet-4-6',          baseUrl: 'https://api.anthropic.com/v1',   needsKey: true },
  lm_studio: { model: 'local-model',                baseUrl: 'http://localhost:1234/v1',       needsKey: false },
  ollama:    { model: 'llama3.1',                   baseUrl: 'http://localhost:11434/v1',      needsKey: false },
};

/**
 * Whether a provider is usable as-is: has a key, or is keyless with a model
 * saved. Daisy Cloud is zero-config — its presence in the list (entitled
 * license) is the whole requirement.
 */
export function isProviderConfigured(entry: ProviderListEntry): boolean {
  if (entry.name === 'daisy_gateway') return true;
  const keyless = !(PROVIDER_DEFAULTS[entry.name]?.needsKey ?? false);
  return entry.has_key || (keyless && !!entry.model);
}

// Providers that always get a row in Settings → Providers, in display order.
// Daisy Cloud (daisy_gateway) is NOT listed here: the backend synthesizes its
// entry only when the license stamp carries the daisy_cloud entitlement, and
// the merge below appends any backend provider not in this list.
export const KEY_PROVIDERS: ProviderId[] = [
  'groq',
  'openai',
  'anthropic',
  'lm_studio',
  'ollama',
];

/**
 * Merge the backend provider list with the known-providers allowlist.
 *
 * For each known provider, the backend entry is used when present (carries
 * has_key / model / base_url), else an empty row is synthesized. Any backend
 * provider not in the allowlist is appended.
 */
export function buildProviderRows(backend: ProviderListEntry[]): ProviderListEntry[] {
  const rows: ProviderListEntry[] = KEY_PROVIDERS.map((name) => {
    const found = backend.find((p) => p.name === name);
    if (found) return found;
    return { name, has_key: false, model: null, base_url: null };
  });
  for (const p of backend) {
    if (!KEY_PROVIDERS.includes(p.name)) rows.push(p);
  }
  return rows;
}
