// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { useState } from 'react';
import { tauri, errStr, type ProviderConfigInput, type ProviderListEntry } from '../tauri';
import { Field } from './Field';
import { PROVIDER_DEFAULTS, isProviderConfigured } from '../lib/providerRows';
import { Select } from './Select';

interface Props {
  provider: ProviderListEntry;
  onSaved: () => void;
  /** Whether this provider is the AI provider in use. */
  enabled?: boolean;
  /** Makes this provider the one in use; the row offers Enable once configured. */
  onEnable?: () => void;
}

// Editing is a two-step flow: (1) enter key/base URL, test it, fetch the
// model list; (2) pick a model and save. The vault is already unlocked here
// (this screen is only reachable past the unlock screen); saving re-encrypts
// with the passphrase stashed at unlock — the user never re-types it.
type Step = 'closed' | 'key' | 'model';

export function ProviderEditor({ provider, onSaved, enabled = false, onEnable }: Props) {
  const defaults = PROVIDER_DEFAULTS[provider.name] ?? { model: '', baseUrl: null, needsKey: false };
  // LM Studio / Ollama (and any custom OpenAI-compat endpoint we ship with
  // needsKey=false) don't authenticate. The "active" signal for them is a
  // saved model + a reachable URL, not the presence of a key.
  const keyless = !defaults.needsKey;
  const active = isProviderConfigured(provider);

  const [step, setStep] = useState<Step>('closed');
  const [apiKey, setApiKey] = useState('');
  const [baseUrl, setBaseUrl] = useState(provider.base_url ?? '');
  const [models, setModels] = useState<string[]>([]);
  const [model, setModel] = useState(provider.model ?? '');
  // Context length of a local endpoint, as typed; blank clears the stored
  // value. Only LM Studio / Ollama show the field.
  const localContext = provider.name === 'lm_studio' || provider.name === 'ollama';
  const [contextTokens, setContextTokens] = useState(
    provider.context_tokens != null ? String(provider.context_tokens) : '',
  );
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // The row stays mounted across saves, so the fields take the stored values
  // at the moment the editor opens.
  function open() {
    setApiKey('');
    setBaseUrl(provider.base_url ?? '');
    setModels([]);
    setModel(provider.model ?? '');
    setContextTokens(provider.context_tokens != null ? String(provider.context_tokens) : '');
    setError(null);
    setStep('key');
  }

  function close() {
    setStep('closed');
    setApiKey('');
    setBaseUrl(provider.base_url ?? '');
    setModels([]);
    setModel(provider.model ?? '');
    setContextTokens(provider.context_tokens != null ? String(provider.context_tokens) : '');
    setBusy(false);
    setError(null);
  }

  // LM Studio reports the context length the model is loaded with; a blank
  // field takes it. Ollama reports nothing; the field stays as typed.
  async function prefillContext(chosen: string) {
    if (provider.name !== 'lm_studio' || contextTokens.length > 0 || chosen.length === 0) return;
    try {
      const n = await tauri.probeContextLength(provider.name, baseUrl.length > 0 ? baseUrl : null, chosen);
      if (n != null && n > 0) setContextTokens(String(n));
    } catch {
      // The field stays blank; the default context applies until it is set.
    }
  }

  async function testAndListModels() {
    setBusy(true);
    setError(null);
    try {
      const list = await tauri.listProviderModels(
        provider.name,
        apiKey.length > 0 ? apiKey : null,
        baseUrl.length > 0 ? baseUrl : null,
      );
      setModels(list);
      // Preselects the stored model when it is still offered, else the first.
      const prev = provider.model;
      const chosen = prev && list.includes(prev) ? prev : list[0] ?? prev ?? '';
      setModel(chosen);
      setStep('model');
      await prefillContext(chosen);
    } catch (e: unknown) {
      setError(`${errStr(e)}\n(check the API key and base URL; for local servers make sure it's running)`);
    } finally {
      setBusy(false);
    }
  }

  async function save() {
    setBusy(true);
    setError(null);
    try {
      const ctx = Number.parseInt(contextTokens, 10);
      const config: ProviderConfigInput = {
        api_key: apiKey.length > 0 ? apiKey : null, // null = keep existing
        model: model.length > 0 ? model : null,
        base_url: baseUrl.length > 0 ? baseUrl : null,
        // 0 clears a stored value; null leaves cloud providers untouched.
        context_tokens: localContext ? (Number.isFinite(ctx) && ctx > 0 ? ctx : 0) : null,
      };
      await tauri.setProvider(provider.name, config);
      close();
      onSaved();
    } catch (e: unknown) {
      setError(errStr(e));
    } finally {
      setBusy(false);
    }
  }

  async function clearKey() {
    setBusy(true);
    setError(null);
    try {
      // Empty string = explicit "clear the key"; null means "keep existing".
      await tauri.setProvider(provider.name, {
        api_key: '',
        model: provider.model,
        base_url: provider.base_url,
        context_tokens: null,
      });
      close();
      onSaved();
    } catch (e: unknown) {
      setError(errStr(e));
    } finally {
      setBusy(false);
    }
  }

  // ---- collapsed row --------------------------------------------------------
  if (step === 'closed') {
    return (
      <div className="list-row list-row--stack">
        <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center', gap: 12 }}>
          <div style={{ minWidth: 0 }}>
            <div className="list-row__name">
              {active && (
                <span className="list-row__on" aria-label="configured" title="configured">✓</span>
              )}
              <span>{provider.name}</span>
              {keyless && (
                <span className="meta list-row__aside">· keyless</span>
              )}
            </div>
            <div className="meta" data-testid={`provider-meta-${provider.name}`}>
              {provider.model ? `model: ${provider.model}` : `default model: ${defaults.model}`}
              {provider.base_url ? ` · ${provider.base_url}` : (defaults.baseUrl ? ` · ${defaults.baseUrl}` : '')}
              {provider.context_tokens != null ? ` · ctx ${provider.context_tokens}` : ''}
            </div>
          </div>
          <div style={{ display: 'flex', gap: 8, alignItems: 'center' }}>
            {onEnable && active && enabled && (
              <span className="meta--success asr-model__active" data-testid={`provider-enabled-${provider.name}`}>● Enabled</span>
            )}
            {onEnable && active && !enabled && (
              <button
                data-testid={`provider-enable-${provider.name}`}
                className="btn"
                style={{ whiteSpace: 'nowrap' }}
                onClick={onEnable}
              >
                Enable
              </button>
            )}
            <button
              data-testid={`provider-configure-${provider.name}`}
              className="btn"
              style={{ whiteSpace: 'nowrap' }}
              onClick={open}
            >
              {active ? 'Edit' : 'Configure'}
            </button>
          </div>
        </div>
      </div>
    );
  }

  // ---- step 1: key + base URL ----------------------------------------------
  if (step === 'key') {
    return (
      <div className="editor-card">
        <div className="editor-card__title">{provider.name} — step 1 of 2</div>
        {!keyless && (
          <Field label={provider.has_key ? 'API key (leave blank to use the stored key)' : 'API key'}>
            <input
              data-testid="provider-key"
              type="password" value={apiKey} disabled={busy}
              onChange={(e) => setApiKey(e.target.value)}
              placeholder={provider.has_key ? '•••••••• (using stored key)' : 'paste key here'}
              style={{ display: 'block', width: '100%' }}
            />
          </Field>
        )}
        <Field label={`Base URL (default: ${defaults.baseUrl ?? '—'})`}>
          <input
            data-testid="provider-base-url"
            type="text" value={baseUrl} disabled={busy}
            onChange={(e) => setBaseUrl(e.target.value)}
            placeholder={defaults.baseUrl ?? ''}
            style={{ display: 'block', width: '100%' }}
          />
        </Field>
        {keyless && (
          <p className="meta" style={{ fontSize: 'var(--t-small)', marginTop: 8 }}>
            {provider.name} is keyless — Daisy lists models at <code>{`{base}/v1/models`}</code> and
            probes <code>{`{base}/v1/chat/completions`}</code> to confirm summaries will work. Use the
            OpenAI-compatible server base (e.g. <code>http://localhost:1234/v1</code>), not the
            <code>/api/v1</code> REST API. No credentials are stored.
          </p>
        )}
        <div style={{ display: 'flex', gap: 8, marginTop: 16 }}>
          <button
            data-testid="provider-test"
            onClick={testAndListModels}
            disabled={busy || (defaults.needsKey && !provider.has_key && apiKey.length === 0)}
            className="btn btn--primary"
          >
            {busy ? 'Testing…' : (keyless ? 'Test connection & list models →' : 'Test key & list models →')}
          </button>
          {!keyless && provider.has_key && (
            <button data-testid="provider-clear-key" onClick={clearKey} disabled={busy} className="btn btn--danger">
              Clear key
            </button>
          )}
          <button data-testid="provider-cancel" onClick={close} disabled={busy} className="btn">Cancel</button>
        </div>
        {error && <p className="meta meta--danger" style={{ marginTop: 8, whiteSpace: 'pre-wrap' }}>{error}</p>}
      </div>
    );
  }

  // ---- step 2: pick model + save -------------------------------------------
  return (
    <div className="editor-card">
      <div className="editor-card__title">{provider.name} — step 2 of 2</div>
      <Field label={`Model (${models.length} available)`}>
        {models.length > 0 ? (
          <Select
            testId="provider-model"
            label="Model"
            className="field__control"
            value={model}
            disabled={busy}
            onChange={setModel}
            options={[
              ...models.map((m) => ({ value: m, label: m })),
              ...(model && !models.includes(model) ? [{ value: model, label: `${model} (custom)` }] : []),
            ]}
          />
        ) : (
          <input data-testid="provider-model-text" type="text" value={model} disabled={busy} onChange={(e) => setModel(e.target.value)} placeholder={defaults.model} style={{ display: 'block', width: '100%' }} />
        )}
        {models.length > 0 && (
          <input
            data-testid="provider-model-custom"
            type="text" disabled={busy}
            value={models.includes(model) ? '' : model}
            onChange={(e) => {
              const v = e.target.value;
              setModel(v.length > 0 ? v : (provider.model && models.includes(provider.model) ? provider.model : (models[0] ?? '')));
            }}
            placeholder="…or type a custom model name"
            style={{ display: 'block', width: '100%', marginTop: 6 }}
          />
        )}
      </Field>
      {localContext && (
        <Field label="Context length (tokens)">
          <input
            data-testid="provider-context-tokens"
            type="number" min={1} step={1} inputMode="numeric"
            value={contextTokens} disabled={busy}
            onChange={(e) => setContextTokens(e.target.value.replace(/[^0-9]/g, ''))}
            placeholder="4096"
            style={{ display: 'block', width: '100%' }}
          />
        </Field>
      )}
      <div style={{ display: 'flex', gap: 8, marginTop: 16, flexWrap: 'wrap', alignItems: 'center' }}>
        <button data-testid="provider-save" onClick={save} disabled={busy} className="btn btn--primary">{busy ? 'Saving…' : 'Save'}</button>
        <button data-testid="provider-back" onClick={() => { setStep('key'); setError(null); }} disabled={busy} className="btn">← Back</button>
        <button data-testid="provider-cancel-2" onClick={close} disabled={busy} className="btn">Cancel</button>
      </div>
      {error && <p className="meta meta--danger" style={{ marginTop: 8, whiteSpace: 'pre-wrap' }}>{error}</p>}
    </div>
  );
}

