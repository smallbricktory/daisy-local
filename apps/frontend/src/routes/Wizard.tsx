// Setup wizard. Two modes:
//   * first-run: Welcome → name + profile → vault → AI provider → done.
//   * reconfigure: launched from Settings; re-runs the mic picker only.
//
// Re-runnable any time. Transcription + speaker labels are always on-device;
// the only first-run choices are who you are + where data lives, how the vault
// is secured (passphrase vs trust-this-machine), and which AI provider (if any)
// powers summaries.

import { useEffect, useRef, useState } from 'react';
import { open } from '@tauri-apps/plugin-dialog';
import {
  tauri,
  errStr,
  envProfileDir,
  providerLabel,
  type AudioSourceInfo,
  type BootstrapStatus,
  type ProviderId,
  type ProviderListEntry,
  type LiveCaptionsStatus,
  type Settings as AppSettings,
} from '../tauri';
import { PassphraseInput } from '../components/PassphraseInput';
import { ProviderEditor } from '../components/ProviderEditor';
import { MicLevel } from '../components/MicLevel';
import { revalidateAiProviderStatus } from '../lib/aiProviderStatus';
import { showGatewayNoticeIfNeeded } from '../lib/gatewayNotice';
import { STEP_ORDER, STEP_LABELS, type WizardAnswers } from '../lib/wizard-graph';

const HELP_API_KEY_URL = 'https://www.daisylocal.app/help/getting-an-api-key';
const HELP_URL = 'https://www.daisylocal.app/help';
const FAQ_URL = 'https://www.daisylocal.app/faq';

type Mode = 'first-run' | 'reconfigure';

interface Props {
  mode: Mode;
  onComplete: () => void;
  onCancel?: () => void;
}

type Step =
  | 'loading'
  | 'welcome'
  | 'intro'           // name + profile path
  | 'vault'           // trust-this-machine vs passphrase
  | 'finalizing-vault'
  | 'ai-provider'
  | 'microphone'      // reconfigure entry point (and its only step)
  | 'benchmark';      // final step (first-run): speed check → live captions

type Answers = WizardAnswers;

const DEFAULT_ANSWERS: Answers = {
  selectedMicId: null,
};

/** "Skip rest" is available on first-run only once the vault exists. */
function canSkipRest(step: Step, mode: Mode): boolean {
  if (step === 'loading') return false;
  if (mode === 'reconfigure') return false; // single step
  // First-run: only the AI step is skippable (the mic step is the finish).
  return step === 'ai-provider';
}

export function Wizard({ mode, onComplete, onCancel }: Props) {
  const [step, setStep] = useState<Step>('loading');
  const [history, setHistory] = useState<Step[]>([]);
  const [bs, setBs] = useState<BootstrapStatus | null>(null);
  const [settings, setSettings] = useState<AppSettings | null>(null);
  const [ans, setAns] = useState<Answers>(DEFAULT_ANSWERS);
  const [err, setErr] = useState<string | null>(null);
  const [envDir, setEnvDir] = useState<string | null>(null);

  useEffect(() => {
    (async () => {
      try {
        const [s, envD] = await Promise.all([tauri.bootstrapStatus(), envProfileDir()]);
        setBs(s);
        setEnvDir(envD);
        if (mode === 'reconfigure') {
          const cur = await tauri.readSettings();
          setSettings(cur);
          setStep('microphone');
        } else {
          setStep('welcome');
        }
      } catch (e) {
        setErr(errStr(e));
      }
    })();
  }, [mode]);

  function advance(to: Step) {
    setHistory((h) => [...h, step]);
    setErr(null);
    setStep(to);
  }

  function goBack() {
    setHistory((h) => {
      if (h.length === 0) return h;
      const prev = h[h.length - 1];
      setStep(prev);
      setErr(null);
      return h.slice(0, -1);
    });
  }

  // --- step handlers ---

  async function saveIntro(name: string, profileDir: string, autoUpdate: boolean) {
    setErr(null);
    try {
      await tauri.bootstrapSet(profileDir);
      const cur = await tauri.readSettings();
      await tauri.writeSettings({
        ...cur,
        user_display_name: name.trim() || null,
        auto_update_check: autoUpdate,
      });
      // The chosen profile may already hold a vault (reinstall / synced
      // profile); creating one fails with "vault already exists". Skips
      // straight to completion; the app routes to the unlock screen.
      const vs = await tauri.vaultStatus();
      if (vs.vault_exists) {
        onComplete();
        return;
      }
      advance('vault');
    } catch (e) { setErr(errStr(e)); }
  }

  /** Passphrase mode — encrypt the vault under a user passphrase. */
  async function setPass(passphrase: string) {
    setErr(null);
    advance('finalizing-vault');
    try {
      await tauri.initVault(passphrase);
      setStep('ai-provider');
    } catch (e) {
      // Vault already there (e.g. profile switched mid-flow) → bail to the
      // app's unlock screen.
      if (errStr(e).includes('vault already exists')) { onComplete(); return; }
      setErr(errStr(e));
      setHistory((h) => h.slice(0, -1));
      setStep('vault');
    }
  }

  /** Trust-this-machine mode — vault key derived from the machine ID,
   *  auto-unlocks on launch. No passphrase, no prompt, no warning. */
  async function trustMachine() {
    setErr(null);
    advance('finalizing-vault');
    try {
      await tauri.initVaultMachineMode();
      setStep('ai-provider');
    } catch (e) {
      if (errStr(e).includes('vault already exists')) { onComplete(); return; }
      setErr(errStr(e));
      setHistory((h) => h.slice(0, -1));
      setStep('vault');
    }
  }

  // (mic step also doubles as the first-run closing screen — see render)

  function skipRest() {
    onComplete();
  }

  async function saveMicrophone(micId: number | null) {
    setErr(null);
    try {
      if (micId !== null) {
        const cur = await tauri.readSettings();
        await tauri.writeSettings({ ...cur, default_mic_source_id: micId });
      }
      // First-run continues to the speed check; reconfigure is mic-only.
      if (mode === 'first-run') {
        advance('benchmark');
      } else {
        onComplete();
      }
    } catch (e) { setErr(errStr(e)); }
  }

  // --- render ---

  if (step === 'loading' || (mode === 'first-run' && !bs)) {
    return <p className="meta" style={{ padding: 32 }}>Loading…</p>;
  }

  const progress = stepProgress(step, mode);
  const stepLabel = STEP_LABELS[step] ?? '';

  return (
    <div className="hero wizard-step">
      <div style={{ display: 'flex', alignItems: 'center', gap: 12, marginBottom: 12 }}>
        <div className="wizard-step__logo" aria-hidden="true" style={{ width: 32, height: 32, flex: '0 0 auto' }} />
        <div style={{ flex: 1, minWidth: 0 }}>
          <StepHeader idx={progress.idx} total={progress.total} label={stepLabel} />
        </div>
        {history.length > 0 && step !== 'finalizing-vault' && (
          <button onClick={goBack} className="btn" title="Go back to the previous step.">
            ← Back
          </button>
        )}
        {canSkipRest(step, mode) && (
          <button onClick={skipRest} className="btn" title="Skip the remaining setup. Configure later in Settings.">
            Skip →
          </button>
        )}
        {mode === 'reconfigure' && onCancel && (
          <button className="btn" onClick={onCancel} title="Close the wizard and return to Settings.">
            ← Settings
          </button>
        )}
      </div>

      {step === 'welcome' && (
        <Surface>
          <WelcomeStep onContinue={() => advance('intro')} />
        </Surface>
      )}

      {step === 'intro' && bs && (
        <Surface>
          <IntroStep
            defaultPath={envDir ?? bs.platform_default}
            envOverride={envDir}
            initialName={settings?.user_display_name ?? ''}
            onSubmit={saveIntro}
          />
        </Surface>
      )}

      {step === 'vault' && (
        <Surface>
          <VaultStep onTrustMachine={() => void trustMachine()} onPassphrase={setPass} />
        </Surface>
      )}

      {step === 'finalizing-vault' && (
        <Surface><p className="meta">Creating vault… (Argon2id, ~1 second)</p></Surface>
      )}

      {step === 'ai-provider' && (
        <Surface>
          <AiProviderStep onContinue={() => advance('microphone')} />
        </Surface>
      )}

      {step === 'microphone' && (
        <Surface>
          <MicStep
            initial={ans.selectedMicId ?? settings?.default_mic_source_id ?? null}
            onSelect={(id) => setAns((a) => ({ ...a, selectedMicId: id }))}
            onSubmit={(id) => saveMicrophone(id)}
            submitLabel={mode === 'first-run' ? 'Continue' : 'Use this microphone'}
          />
        </Surface>
      )}

      {step === 'benchmark' && (
        <Surface>
          <BenchmarkStep onFinish={onComplete} />
          <ClosingNote />
        </Surface>
      )}

      {err && <p className="meta" style={{ color: 'var(--danger)', marginTop: 16 }}>{err}</p>}
    </div>
  );
}

// ---- subcomponents ----

function WelcomeStep({ onContinue }: { onContinue: () => void }) {
  return (
    <div style={{ maxWidth: 520 }}>
      <h1 className="h1" style={{ marginTop: 0 }}>Welcome to Daisy</h1>
      <p style={{ fontSize: 15, lineHeight: 1.6, color: 'var(--ink)' }}>
        Daisy records, transcribes, and summarizes your meetings — entirely on your
        own machine. No bot joins the call, and your recordings never leave your
        computer unless you choose a cloud AI provider.
      </p>
      <p className="meta" style={{ fontSize: 13, marginTop: 12 }}>
        Setup takes about a minute: tell Daisy your name, pick where to store your
        data, secure your keys, optionally connect an AI provider for summaries, and
        choose your microphone.
      </p>
      <button className="btn btn--primary" style={{ marginTop: 20 }} onClick={onContinue}>
        Get started
      </button>
    </div>
  );
}

/** Vault screen — two clear choices, no warnings. Trust-this-machine commits
 *  immediately (no passphrase); "encrypt" reveals the passphrase form. */
function VaultStep({
  onTrustMachine, onPassphrase,
}: {
  onTrustMachine: () => void;
  onPassphrase: (pass: string) => void;
}) {
  const [choice, setChoice] = useState<null | 'encrypt'>(null);

  if (choice === 'encrypt') {
    return (
      <div style={{ maxWidth: 460 }}>
        <h2 className="h2" style={{ marginTop: 0 }}>Choose a passphrase</h2>
        <p className="meta" style={{ marginBottom: 12 }}>
          Encrypts your API keys + voiceprints on disk. Daisy asks for it each
          launch. Minimum 22 characters — there is no recovery if you forget it.
        </p>
        <PassphraseInput minChars={22} onSubmit={onPassphrase} submitLabel="Create vault" />
        <button
          type="button"
          className="btn"
          style={{ marginTop: 14 }}
          onClick={() => setChoice(null)}
        >
          ← Back
        </button>
      </div>
    );
  }

  return (
    <div>
      <h2 className="h2" style={{ marginTop: 0 }}>How should Daisy secure your keys?</h2>
      <p className="meta" style={{ marginBottom: 16 }}>
        Your API keys and voiceprints live in an encrypted vault. Pick how it&rsquo;s unlocked.
      </p>
      <div style={{ display: 'flex', gap: 16, flexWrap: 'wrap' }}>
        <button
          type="button"
          className="vault-card"
          onClick={() => setChoice('encrypt')}
          style={vaultCardStyle}
          onMouseEnter={(e) => { e.currentTarget.style.borderColor = 'var(--indigo)'; }}
          onMouseLeave={(e) => { e.currentTarget.style.borderColor = 'var(--frost-deep)'; }}
        >
          <div style={{ fontWeight: 700, fontSize: 15, marginBottom: 6 }}>Passphrase (recommended)</div>
          <div className="meta" style={{ fontSize: 13, lineHeight: 1.5 }}>
            Daisy asks for it each launch. Only you can unlock the vault —
            required if your meetings involve regulated data (e.g. PHI).
          </div>
        </button>
        <button
          type="button"
          className="vault-card"
          onClick={onTrustMachine}
          style={vaultCardStyle}
          onMouseEnter={(e) => { e.currentTarget.style.borderColor = 'var(--indigo)'; }}
          onMouseLeave={(e) => { e.currentTarget.style.borderColor = 'var(--frost-deep)'; }}
        >
          <div style={{ fontWeight: 700, fontSize: 15, marginBottom: 6 }}>I trust this machine</div>
          <div className="meta" style={{ fontSize: 13, lineHeight: 1.5 }}>
            No passphrase — Daisy unlocks automatically. Anyone who can sign in
            to this computer can open the vault.
          </div>
        </button>
      </div>
    </div>
  );
}

const vaultCardStyle: React.CSSProperties = {
  flex: '1 1 220px',
  minWidth: 220,
  // Top-aligns content; a native <button> centers its content vertically
  // within the stretched row height.
  display: 'flex',
  flexDirection: 'column',
  justifyContent: 'flex-start',
  textAlign: 'left',
  padding: '16px 18px',
  border: '1px solid var(--frost-deep)',
  borderRadius: 10,
  background: 'var(--cream-pure)',
  cursor: 'pointer',
  fontFamily: 'inherit',
  transition: 'border-color 140ms ease',
};

/** First-run AI provider step — same configure flow as Settings → Providers.
 *  Pick a provider, then enter the key inline (ProviderEditor); or skip and
 *  use the copy-paste path. */
function AiProviderStep({ onContinue }: { onContinue: () => void }) {
  const [providers, setProviders] = useState<ProviderListEntry[] | null>(null);
  // Daisy Cloud last — present only when the backend lists it (license stamp
  // carries the daisy_cloud entitlement).
  const OPTIONS: ProviderId[] = [
    'anthropic', 'openai', 'groq', 'lm_studio', 'ollama',
    ...(providers?.some((e) => e.name === 'daisy_gateway') ? (['daisy_gateway'] as ProviderId[]) : []),
  ];
  const [chosen, setChosen] = useState<ProviderId | null>(null);
  const [err, setErr] = useState<string | null>(null);

  const reload = () => tauri.listProviders().then(setProviders).catch((e) => setErr(errStr(e)));
  useEffect(() => { void reload(); }, []);

  async function pick(p: ProviderId) {
    setErr(null);
    try {
      // Daisy Cloud is zero-config: registers the per-install keypair. No
      // ProviderEditor renders for it (see below).
      if (p === 'daisy_gateway') {
        await tauri.registerGateway();
        const cur = await tauri.readSettings();
        await tauri.writeSettings({ ...cur, default_summary_provider: p });
        revalidateAiProviderStatus();
        setChosen(p);
        return;
      }
      const cur = await tauri.readSettings();
      await tauri.writeSettings({ ...cur, default_summary_provider: p });
      // A fresh vault has no providers; listProviders() is empty and the
      // editor below ('chosen && chosenEntry') needs an entry. Registers a
      // keyless stub (with its real roles from the backend), then reloads.
      // No-op when the provider is already present.
      if (!providers?.some((e) => e.name === p)) {
        await tauri.setProvider(p, { api_key: null, model: null, base_url: null });
        await reload();
      }
      revalidateAiProviderStatus();
      setChosen(p);
    } catch (e) {
      if (!showGatewayNoticeIfNeeded(e)) setErr(errStr(e));
    }
  }

  async function skip() {
    setErr(null);
    try {
      const cur = await tauri.readSettings();
      await tauri.writeSettings({ ...cur, default_summary_provider: null });
      revalidateAiProviderStatus();
      onContinue();
    } catch (e) { setErr(errStr(e)); }
  }

  const chosenEntry = providers?.find((p) => p.name === chosen);

  return (
    <div style={{ maxWidth: 520 }}>
      <h2 className="h2" style={{ marginTop: 0 }}>Connect an AI provider</h2>
      <p className="meta" style={{ marginBottom: 6 }}>
        Summaries, ask-your-meetings search, chapters, and analysis run through an
        LLM provider of your choice. Transcription is always on-device.
      </p>
      <p className="meta" style={{ marginBottom: 16, fontSize: 12 }}>
        Need a key?{' '}
        <button
          type="button"
          onClick={() => { void tauri.openExternal(HELP_API_KEY_URL); }}
          className="btn-link"
        >
          How to get an API key ↗
        </button>
      </p>

      <div style={{ display: 'flex', gap: 8, flexWrap: 'wrap', marginBottom: 16 }}>
        {OPTIONS.map((p) => (
          <button
            key={p}
            className={p === chosen ? 'btn btn--primary' : 'btn'}
            onClick={() => void pick(p)}
          >
            {providerLabel(p)}
          </button>
        ))}
      </div>

      {chosen === 'daisy_gateway' && (
        <p className="meta" style={{ fontSize: 13, marginBottom: 16 }}>
          Daisy Cloud is zero-config — no API key or model to set. Requires a license
          that includes Daisy Cloud (internal use only).
        </p>
      )}
      {chosen && chosen !== 'daisy_gateway' && chosenEntry && (
        <div style={{ border: '1px solid var(--frost-deep)', borderRadius: 8, padding: '4px 12px', marginBottom: 16 }}>
          <ProviderEditor provider={chosenEntry} onSaved={() => void reload()} />
        </div>
      )}
      {chosen && chosen !== 'daisy_gateway' && !chosenEntry && providers != null && (
        <p className="meta" style={{ color: 'var(--danger)' }}>Couldn&rsquo;t load {chosen} — configure it later in Settings.</p>
      )}

      {err && (
        <p className="meta" style={{ color: 'var(--danger)' }}>
          {err}{' '}
          <a href="#" onClick={(e) => { e.preventDefault(); setErr(null); void reload(); }}>Try again</a>
        </p>
      )}

      <div style={{ display: 'flex', gap: 8, marginTop: 8 }}>
        <button className="btn btn--primary" disabled={!chosen} onClick={onContinue}>
          Continue
        </button>
        <button className="btn" onClick={() => void skip()}>
          Skip — I&rsquo;ll use copy-paste
        </button>
      </div>
    </div>
  );
}

function Surface({ title, children }: { title?: string; children: React.ReactNode }) {
  return (
    <section className="surface" style={{ marginTop: 24 }}>
      {title && <h2 className="h2">{title}</h2>}
      {children}
    </section>
  );
}

function StepHeader({ idx, total, label }: { idx: number; total: number; label: string }) {
  return (
    <h2
      className="h2"
      aria-label="Setup progress"
      style={{ margin: 0, fontSize: 16 }}
    >
      <span style={{ color: 'var(--iron)' }}>Step {idx + 1} / {total}</span>
      <span style={{ margin: '0 8px', color: 'var(--iron)' }}>—</span>
      {label}
    </h2>
  );
}

function IntroStep({
  defaultPath, envOverride, initialName, onSubmit,
}: {
  defaultPath: string;
  envOverride: string | null;
  initialName: string;
  onSubmit: (name: string, profileDir: string, autoUpdate: boolean) => void;
}) {
  const [name, setName] = useState(initialName);
  const [autoUpdate, setAutoUpdate] = useState(true);
  const [useCustom, setUseCustom] = useState(false);
  const [customPath, setCustomPath] = useState<string | null>(null);
  const path = envOverride ?? (useCustom ? (customPath ?? '') : defaultPath);
  const pathValid = path.trim().length > 0;

  async function browse() {
    const selected = await open({ directory: true, multiple: false });
    if (typeof selected === 'string' && selected.length > 0) {
      setCustomPath(selected);
      setUseCustom(true);
    }
  }

  return (
    <div>
      <label style={{ display: 'block', fontSize: 13, color: 'var(--muted)', marginBottom: 4 }}>
        What is your name?
      </label>
      <input
        type="text" value={name} onChange={(e) => setName(e.target.value)}
        placeholder="e.g. Daisy" autoFocus
        style={{ display: 'block', width: 320, marginBottom: 10 }}
      />
      <p className="meta" style={{ fontSize: 12, marginBottom: 14 }}>
        Used to address you as &ldquo;you&rdquo; in meeting summaries instead of by name.
        Editable later in Settings → Profile.
      </p>

      <label style={{ display: 'block', fontSize: 13, color: 'var(--muted)', marginBottom: 4 }}>
        Where should Daisy save your data?
      </label>
      {envOverride ? (
        <div>
          <code style={{ fontSize: 13 }}>{envOverride}</code>
          <p className="meta" style={{ fontSize: 12, marginTop: 4, color: 'var(--indigo-deep)' }}>
            (set via DAISY_PROFILE_DIR)
          </p>
        </div>
      ) : (
        <div>
          <label style={{ display: 'flex', gap: 8, alignItems: 'center' }}>
            <input type="radio" checked={!useCustom} onChange={() => setUseCustom(false)} />
            <span>Use default: <code>{defaultPath}</code></span>
          </label>
          <label style={{ display: 'flex', gap: 8, alignItems: 'center', marginTop: 8 }}>
            <input type="radio" checked={useCustom} onChange={() => setUseCustom(true)} />
            <span>Custom location (e.g. Dropbox / iCloud Drive / OneDrive for cross-device sync)</span>
          </label>
          {useCustom && (
            <div style={{ marginTop: 8, display: 'flex', gap: 8, alignItems: 'center' }}>
              {customPath ? (
                <>
                  <code style={{ flex: 1, fontSize: 13, wordBreak: 'break-all' }}>{customPath}</code>
                  <button onClick={browse} className="btn">Change…</button>
                </>
              ) : (
                <button onClick={browse} className="btn">Browse…</button>
              )}
            </div>
          )}
        </div>
      )}

      <label style={{ display: 'flex', gap: 8, alignItems: 'center', fontSize: 13, marginTop: 16 }}>
        <input type="checkbox" checked={autoUpdate} onChange={(e) => setAutoUpdate(e.target.checked)} />
        <span>Check for new versions automatically (notify-only — never auto-installs)</span>
      </label>

      <div style={{ marginTop: 16 }}>
        <button className="btn btn--primary" disabled={!pathValid} onClick={() => onSubmit(name, path.trim(), autoUpdate)}>
          Continue
        </button>
      </div>
    </div>
  );
}

function MicStep({
  initial, onSelect, onSubmit, submitLabel = 'Use this microphone',
}: {
  initial: number | null;
  onSelect: (id: number | null) => void;
  onSubmit: (id: number | null) => void;
  submitLabel?: string;
}) {
  const [mics, setMics] = useState<AudioSourceInfo[] | null>(null);
  const [selected, setSelected] = useState<number | null>(initial);

  useEffect(() => {
    tauri.listAudioSources()
      .then((sources) => {
        const m = sources.filter((s) => s.kind === 'mic');
        setMics(m);
        if (selected === null && m.length > 0) {
          setSelected(m[0].id);
          onSelect(m[0].id);
        }
      })
      .catch(() => setMics([]));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  function handleChange(id: number) {
    setSelected(id);
    onSelect(id);
  }

  if (mics == null) return <p className="meta">Detecting devices…</p>;
  if (mics.length === 0) {
    return (
      <>
        <p className="meta">No microphones detected. Configure later in Settings.</p>
        <button className="btn btn--primary" style={{ marginTop: 12 }} onClick={() => onSubmit(null)}>{submitLabel}</button>
      </>
    );
  }
  return (
    <>
      <select
        value={selected ?? ''}
        onChange={(e) => handleChange(Number(e.target.value))}
        style={{ display: 'block', width: 320, marginBottom: 12 }}
      >
        {mics.map((m) => <option key={m.id} value={m.id}>{m.description}</option>)}
      </select>
      <div style={{ display: 'flex', alignItems: 'center', gap: 8, marginBottom: 16 }}>
        <span style={{ fontSize: 12, color: 'var(--muted)' }}>Detected level:</span>
        <MicLevel tauriSourceId={selected} width={200} height={12} />
      </div>
      <button className="btn btn--primary" onClick={() => onSubmit(selected)}>{submitLabel}</button>
    </>
  );
}

/** Final first-run step: measures local transcription speed, stores this
 *  machine's live-captions verdict, and shows the result. */
function BenchmarkStep({ onFinish }: { onFinish: () => void }) {
  const [status, setStatus] = useState<LiveCaptionsStatus | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const ran = useRef(false);
  useEffect(() => {
    if (ran.current) return;
    ran.current = true;
    tauri.runLiveCaptionsBench().then(setStatus).catch((e) => setErr(errStr(e)));
  }, []);

  return (
    <div style={{ maxWidth: 520 }}>
      <h2 className="h2" style={{ marginTop: 0 }}>Checking transcription speed</h2>
      {!status && !err && (
        <p className="meta" style={{ fontSize: 14 }}>
          Measuring how fast this machine transcribes (about a minute on
          slower hardware). This decides whether live captions run during
          recordings here.
        </p>
      )}
      {status && (
        <div>
          <p style={{ fontSize: 15, margin: 0 }}>
            <strong>{status.bench_xrt?.toFixed(1) ?? '?'}× realtime</strong> on {status.machine} —
            live captions <strong>{status.enabled ? 'on' : 'off'}</strong>.
          </p>
          <p className="meta" style={{ fontSize: 13, marginTop: 8 }}>
            {status.enabled
              ? 'Words appear as people talk. You can change this any time in Settings → Recordings.'
              : 'This machine transcribes after you stop recording instead — the full transcript still arrives every time. Override any time in Settings → Recordings.'}
          </p>
        </div>
      )}
      {err && (
        <p className="meta" style={{ color: 'var(--danger)', fontSize: 13 }}>
          Speed check failed: {err} — live captions fall back to hardware
          detection; re-run it from Settings → Recordings.
        </p>
      )}
      <button
        className="btn btn--primary"
        style={{ marginTop: 20 }}
        disabled={!status && !err}
        onClick={onFinish}
      >
        Start using Daisy
      </button>
    </div>
  );
}

/** First-run send-off shown under the benchmark on the final step. */
function ClosingNote() {
  const link = (url: string, text: string) => (
    <button
      type="button"
      onClick={() => { void tauri.openExternal(url); }}
      className="btn-link"
    >
      {text}
    </button>
  );
  return (
    <div style={{ marginTop: 20, paddingTop: 16, borderTop: '1px solid var(--frost-deep)', maxWidth: 460 }}>
      <p style={{ fontSize: 14, lineHeight: 1.6, color: 'var(--ink)', margin: 0 }}>
        That&rsquo;s it — you&rsquo;re ready to record. Hit <strong>Record</strong> on your
        next call and Daisy takes it from there.
      </p>
      <p className="meta" style={{ fontSize: 13, marginTop: 10 }}>
        Stuck or curious? {link(HELP_URL, 'Help ↗')} · {link(FAQ_URL, 'FAQ ↗')}
      </p>
    </div>
  );
}

// ---- helpers ----

function stepProgress(step: Step, mode: Mode): { idx: number; total: number } {
  if (mode === 'reconfigure') {
    // Reconfigure shows only the microphone step.
    return { idx: 0, total: 1 };
  }
  const parent: Step = step === 'finalizing-vault' ? 'vault' : step;
  const idx = Math.max(0, STEP_ORDER.indexOf(parent as never));
  return { idx, total: STEP_ORDER.length };
}
