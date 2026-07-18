import { useState } from 'react';
import { tauri, errStr, type ProviderConfigInput, type ProviderListEntry } from '../tauri';
import { PROVIDER_DEFAULTS, isProviderConfigured } from '../lib/providerRows';

interface Props {
  provider: ProviderListEntry;
  onSaved: () => void;
}

// Editing is a two-step flow: (1) enter key/base URL, test it, fetch the
// model list; (2) pick a model and save. The vault is already unlocked here
// (this screen is only reachable past the unlock screen); saving re-encrypts
// with the passphrase stashed at unlock — the user never re-types it.
type Step = 'closed' | 'key' | 'model';

export function ProviderEditor({ provider, onSaved }: Props) {
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
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  function close() {
    setStep('closed');
    setApiKey('');
    setBaseUrl(provider.base_url ?? '');
    setModels([]);
    setModel(provider.model ?? '');
    setBusy(false);
    setError(null);
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
      setModel(prev && list.includes(prev) ? prev : list[0] ?? prev ?? '');
      setStep('model');
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
      const config: ProviderConfigInput = {
        api_key: apiKey.length > 0 ? apiKey : null, // null = keep existing
        model: model.length > 0 ? model : null,
        base_url: baseUrl.length > 0 ? baseUrl : null,
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
      <div style={{ padding: 'var(--space-2) 0', borderBottom: 'var(--rule)' }}>
        <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center', gap: 12 }}>
          <div style={{ minWidth: 0 }}>
            <div style={{ fontWeight: 600, display: 'flex', alignItems: 'center', gap: 6 }}>
              {active && (
                <span aria-label="configured" title="configured" style={{ color: '#1E8E3E', fontWeight: 700 }}>✓</span>
              )}
              <span>{provider.name}</span>
              {keyless && (
                <span className="meta" style={{ fontSize: 11, fontWeight: 400, color: 'var(--iron)' }}>· keyless</span>
              )}
            </div>
            <div className="meta">
              {provider.model ? `model: ${provider.model}` : `default model: ${defaults.model}`}
              {provider.base_url ? ` · ${provider.base_url}` : (defaults.baseUrl ? ` · ${defaults.baseUrl}` : '')}
            </div>
          </div>
          <button
            className={active ? 'btn' : 'btn btn--primary'}
            style={{ whiteSpace: 'nowrap' }}
            onClick={() => setStep('key')}
          >
            {active ? 'Edit' : 'Configure'}
          </button>
        </div>
      </div>
    );
  }

  // ---- step 1: key + base URL ----------------------------------------------
  if (step === 'key') {
    return (
      <div style={{ padding: 'var(--space-3) var(--space-2)', borderBottom: 'var(--rule)', background: 'var(--tint)' }}>
        <div style={{ fontWeight: 600, marginBottom: 8 }}>{provider.name} — step 1 of 2</div>
        {!keyless && (
          <Field label={provider.has_key ? 'API key (leave blank to use the stored key)' : 'API key'}>
            <input
              type="password" value={apiKey} disabled={busy}
              onChange={(e) => setApiKey(e.target.value)}
              placeholder={provider.has_key ? '•••••••• (using stored key)' : 'paste key here'}
              style={{ display: 'block', width: '100%' }}
            />
          </Field>
        )}
        <Field label={`Base URL (default: ${defaults.baseUrl ?? '—'})`}>
          <input
            type="text" value={baseUrl} disabled={busy}
            onChange={(e) => setBaseUrl(e.target.value)}
            placeholder={defaults.baseUrl ?? ''}
            style={{ display: 'block', width: '100%' }}
          />
        </Field>
        {keyless && (
          <p className="meta" style={{ fontSize: 12, marginTop: 8 }}>
            {provider.name} is keyless — Daisy lists models at <code>{`{base}/v1/models`}</code> and
            probes <code>{`{base}/v1/chat/completions`}</code> to confirm summaries will work. Use the
            OpenAI-compatible server base (e.g. <code>http://localhost:1234/v1</code>), not the
            <code>/api/v1</code> REST API. No credentials are stored.
          </p>
        )}
        <div style={{ display: 'flex', gap: 8, marginTop: 16 }}>
          <button
            onClick={testAndListModels}
            disabled={busy || (defaults.needsKey && !provider.has_key && apiKey.length === 0)}
            className="btn btn--primary"
          >
            {busy ? 'Testing…' : (keyless ? 'Test connection & list models →' : 'Test key & list models →')}
          </button>
          {!keyless && provider.has_key && (
            <button onClick={clearKey} disabled={busy} className="btn" style={{ color: 'var(--danger)', borderColor: 'var(--danger)' }}>
              Clear key
            </button>
          )}
          <button onClick={close} disabled={busy} className="btn">Cancel</button>
        </div>
        {error && <p className="meta" style={{ color: 'var(--danger)', marginTop: 8, whiteSpace: 'pre-wrap' }}>{error}</p>}
      </div>
    );
  }

  // ---- step 2: pick model + save -------------------------------------------
  return (
    <div style={{ padding: 'var(--space-3) var(--space-2)', borderBottom: 'var(--rule)', background: 'var(--tint)' }}>
      <div style={{ fontWeight: 600, marginBottom: 8 }}>{provider.name} — step 2 of 2</div>
      <Field label={`Model (${models.length} available)`}>
        {models.length > 0 ? (
          <select value={model} disabled={busy} onChange={(e) => setModel(e.target.value)} style={{ display: 'block', width: '100%' }}>
            {models.map((m) => <option key={m} value={m}>{m}</option>)}
            {model && !models.includes(model) && <option value={model}>{model} (custom)</option>}
          </select>
        ) : (
          <input type="text" value={model} disabled={busy} onChange={(e) => setModel(e.target.value)} placeholder={defaults.model} style={{ display: 'block', width: '100%' }} />
        )}
        {models.length > 0 && (
          <input
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
      <div style={{ display: 'flex', gap: 8, marginTop: 16, flexWrap: 'wrap', alignItems: 'center' }}>
        <button onClick={save} disabled={busy} className="btn btn--primary">{busy ? 'Saving…' : 'Save'}</button>
        <button onClick={() => { setStep('key'); setError(null); }} disabled={busy} className="btn">← Back</button>
        <button onClick={close} disabled={busy} className="btn">Cancel</button>
      </div>
      {error && <p className="meta" style={{ color: 'var(--danger)', marginTop: 8, whiteSpace: 'pre-wrap' }}>{error}</p>}
    </div>
  );
}

function Field({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div style={{ marginTop: 12 }}>
      <label style={{ display: 'block', fontSize: 13, color: 'var(--muted)', marginBottom: 4 }}>{label}</label>
      {children}
    </div>
  );
}

