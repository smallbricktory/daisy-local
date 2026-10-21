// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { useAudioSourcesChanged } from '../lib/useAudioSourcesChanged';
import { setMachine, setSettings, useMachineSettings, useSettings } from '../lib/settingsStore';
// Setup wizard. Two modes:
//   * first-run: Welcome → name + profile → microphone → the setup plan.
//   * reconfigure: launched from Settings; re-runs the mic picker only.
//
// Re-runnable any time. The vault is created silently with the machine key,
// and the plan page shows which AI and transcription engines Daisy chose.

import { useEffect, useRef, useState } from 'react';
import {
  tauri,
  errStr,
  envProfileDir,
  type AudioSourceInfo,
  type BootstrapStatus,
} from '../tauri';
import { MicLevel } from '../components/MicLevel';
import { PlanStep } from './wizard/PlanStep';
import { STEP_ORDER, STEP_LABELS, type WizardStep, type WizardAnswers } from '../lib/wizard-graph';
import { Select } from '../components/Select';

type Mode = 'first-run' | 'reconfigure';

interface Props {
  mode: Mode;
  onComplete: () => void;
  onCancel?: () => void;
}

type Answers = WizardAnswers;

const DEFAULT_ANSWERS: Answers = {
  selectedMicId: null,
};

export function Wizard({ mode, onComplete, onCancel }: Props) {
  const [step, setStep] = useState<WizardStep>('loading');
  const [history, setHistory] = useState<WizardStep[]>([]);
  const [bs, setBs] = useState<BootstrapStatus | null>(null);
  const settings = useSettings();
  const machine = useMachineSettings();
  const machineMic = machine?.default_mic_source_id ?? null;
  const [ans, setAns] = useState<Answers>(DEFAULT_ANSWERS);
  const [err, setErr] = useState<string | null>(null);
  const [envDir, setEnvDir] = useState<string | null>(null);
  // Whether this mount's saveIntro already created the vault.
  const createdVault = useRef(false);

  useEffect(() => {
    (async () => {
      try {
        const [s, envD] = await Promise.all([tauri.bootstrapStatus(), envProfileDir()]);
        setBs(s);
        setEnvDir(envD);
        if (mode === 'reconfigure') {
          setStep('microphone');
        } else {
          // Starts the hardware probe for the plan step.
          tauri.onboardingPlan().catch(() => {});
          setStep('welcome');
        }
      } catch (e) {
        setErr(errStr(e));
      }
    })();
  }, [mode]);

  function advance(to: WizardStep) {
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
      await setSettings({ user_display_name: name.trim() || null, auto_update_check: autoUpdate });
      // A chosen profile that already holds a vault routes to the unlock screen,
      // unless this run created it; then setup continues.
      const vs = await tauri.vaultStatus();
      if (vs.vault_exists) {
        if (createdVault.current) { advance('microphone'); return; }
        onComplete();
        return;
      }
      await tauri.initVaultMachineMode();
      createdVault.current = true;
      advance('microphone');
    } catch (e) {
      if (errStr(e).includes('vault already exists')) {
        if (createdVault.current) { advance('microphone'); return; }
        onComplete();
        return;
      }
      setErr(errStr(e));
    }
  }

  async function saveMicrophone(micId: number | null) {
    setErr(null);
    try {
      if (micId !== null) await setMachine({ default_mic_source_id: micId });
      // First-run continues to the plan step; reconfigure is mic-only.
      if (mode === 'first-run') {
        advance('names');
      } else {
        onComplete();
      }
    } catch (e) { setErr(errStr(e)); }
  }

  async function saveNames(on: boolean) {
    setErr(null);
    try {
      await setMachine({ live_speaker_names: on });
      advance('plan');
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
        {history.length > 0 && (
          <button data-testid="wizard-back" onClick={goBack} className="btn" title="Go back to the previous step.">
            ← Back
          </button>
        )}
        {mode === 'reconfigure' && onCancel && (
          <button data-testid="wizard-to-settings" className="btn" onClick={onCancel} title="Close the wizard and return to Settings.">
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

      {step === 'microphone' && (
        <Surface>
          <MicStep
            initial={ans.selectedMicId ?? machineMic ?? null}
            onSelect={(id) => setAns((a) => ({ ...a, selectedMicId: id }))}
            onSubmit={(id) => saveMicrophone(id)}
            submitLabel={mode === 'first-run' ? 'Continue' : 'Use this microphone'}
          />
        </Surface>
      )}

      {step === 'names' && (
        <Surface>
          <NamesStep initial={machine?.live_speaker_names ?? true} onSubmit={saveNames} />
        </Surface>
      )}

      {step === 'plan' && (
        <Surface>
          <PlanStep onFinish={onComplete} />
        </Surface>
      )}

      {err && <p className="meta meta--danger" style={{ marginTop: 16 }}>{err}</p>}
    </div>
  );
}

// ---- subcomponents ----

function WelcomeStep({ onContinue }: { onContinue: () => void }) {
  return (
    <div style={{ maxWidth: 520 }}>
      <h1 className="h1" style={{ marginTop: 0 }}>Welcome to Daisy</h1>
      <p className="lede-text">
        Daisy records, transcribes, and summarizes your meetings — entirely on your
        own machine. No bot joins the call, and your recordings never leave your
        computer unless you choose a cloud AI provider.
      </p>
      <p className="meta" style={{ fontSize: 'var(--t-ui)', marginTop: 12 }}>
        Setup takes about a minute: tell Daisy your name, pick where to store your
        data, and choose your microphone. Daisy picks the rest and shows you what
        it chose.
      </p>
      <button data-testid="wizard-get-started" className="btn btn--primary" style={{ marginTop: 20 }} onClick={onContinue}>
        Get started
      </button>
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
      style={{ margin: 0, fontSize: 'var(--t-read)' }}
    >
      <span className="wizard-step__count">Step {idx + 1} / {total}</span>
      <span className="wizard-step__count" style={{ margin: '0 8px' }}>—</span>
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
    const selected = await tauri.pickDirectory('profile-new');
    if (typeof selected === 'string' && selected.length > 0) {
      setCustomPath(selected);
      setUseCustom(true);
    }
  }

  return (
    <div>
      <label className="field-label">
        What is your name?
      </label>
      <input
        data-testid="wizard-intro-name"
        type="text" value={name} onChange={(e) => setName(e.target.value)}
        placeholder="e.g. Daisy" autoFocus
        style={{ display: 'block', width: 320, marginBottom: 10 }}
      />
      <p className="meta" style={{ fontSize: 'var(--t-small)', marginBottom: 14 }}>
        Used to address you as &ldquo;you&rdquo; in meeting summaries instead of by name.
        Editable later in Settings → Profile.
      </p>

      <label className="field-label">
        Where should Daisy save your data?
      </label>
      {envOverride ? (
        <div>
          <code style={{ fontSize: 'var(--t-ui)', overflowWrap: 'anywhere' }}>{envOverride}</code>
          <p className="meta gold-link" style={{ fontSize: 'var(--t-small)', marginTop: 4 }}>
            (set via DAISY_PROFILE_DIR)
          </p>
        </div>
      ) : (
        <div>
          <label style={{ display: 'flex', gap: 8, alignItems: 'center' }}>
            <input type="radio" data-testid="wizard-intro-path-default" checked={!useCustom} onChange={() => setUseCustom(false)} />
            <span style={{ minWidth: 0, overflowWrap: 'anywhere' }}>Use default: <code>{defaultPath}</code></span>
          </label>
          <label style={{ display: 'flex', gap: 8, alignItems: 'center', marginTop: 8 }}>
            <input type="radio" data-testid="wizard-intro-path-custom" checked={useCustom} onChange={() => setUseCustom(true)} />
            <span>Custom location (e.g. Dropbox / iCloud Drive / OneDrive for cross-device sync)</span>
          </label>
          {useCustom && (
            <div style={{ marginTop: 8, display: 'flex', gap: 8, alignItems: 'center' }}>
              {customPath ? (
                <>
                  <code style={{ flex: 1, fontSize: 'var(--t-ui)', wordBreak: 'break-all' }}>{customPath}</code>
                  <button data-testid="wizard-intro-path-change" onClick={browse} className="btn">Change…</button>
                </>
              ) : (
                <button data-testid="wizard-intro-path-browse" onClick={browse} className="btn">Browse…</button>
              )}
            </div>
          )}
        </div>
      )}

      <label style={{ display: 'flex', gap: 8, alignItems: 'center', fontSize: 'var(--t-ui)', marginTop: 16 }}>
        <input type="checkbox" data-testid="wizard-intro-auto-update" checked={autoUpdate} onChange={(e) => setAutoUpdate(e.target.checked)} />
        <span>Check for new versions automatically (notify-only — never auto-installs)</span>
      </label>

      <div style={{ marginTop: 16 }}>
        <button data-testid="wizard-intro-continue" className="btn btn--primary" disabled={!pathValid} onClick={() => onSubmit(name, path.trim(), autoUpdate)}>
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

  const loadMics = () => {
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
  };
  useAudioSourcesChanged(loadMics);

  useEffect(() => {
    loadMics();
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
        <button data-testid="wizard-mic-continue" className="btn btn--primary" style={{ marginTop: 12 }} onClick={() => onSubmit(null)}>{submitLabel}</button>
      </>
    );
  }
  return (
    <>
      <Select
        testId="wizard-mic-select"
        label="Microphone"
        className="wizard-mic"
        value={selected === null || selected === undefined ? '' : String(selected)}
        onChange={(v) => handleChange(Number(v))}
        options={mics.map((m) => ({ value: String(m.id), label: m.description }))}
      />
      <div style={{ display: 'flex', alignItems: 'center', gap: 8, marginBottom: 16 }}>
        <span className="meta" style={{ fontSize: 'var(--t-small)' }}>Detected level:</span>
        <MicLevel tauriSourceId={selected} width={200} height={12} />
      </div>
      <button data-testid="wizard-mic-continue" className="btn btn--primary" onClick={() => onSubmit(selected)}>{submitLabel}</button>
    </>
  );
}

function NamesStep({ initial, onSubmit }: { initial: boolean; onSubmit: (on: boolean) => void }) {
  const [on, setOn] = useState(initial);
  return (
    <div style={{ maxWidth: 520 }}>
      <p className="lede-text" style={{ marginTop: 0 }}>Would you like live speaker names?</p>
      <p className="meta" style={{ fontSize: 'var(--t-ui)', marginTop: 8 }}>
        Names appear above the live captions. They take more memory and get better as you use
        Daisy more. Most people should turn this on; turn it off if your system feels slow.
        You can change it any time in Settings → Recordings.
      </p>
      <label style={{ display: 'flex', gap: 8, alignItems: 'center', marginTop: 16 }}>
        <input type="radio" name="live-speaker-names" data-testid="wizard-names-on" checked={on} onChange={() => setOn(true)} />
        <span>Turn on live speaker names (recommended)</span>
      </label>
      <label style={{ display: 'flex', gap: 8, alignItems: 'center', marginTop: 8 }}>
        <input type="radio" name="live-speaker-names" data-testid="wizard-names-off" checked={!on} onChange={() => setOn(false)} />
        <span>Turn off live speaker names</span>
      </label>
      <button data-testid="wizard-names-continue" className="btn btn--primary" style={{ marginTop: 20 }} onClick={() => onSubmit(on)}>
        Continue
      </button>
    </div>
  );
}

// ---- helpers ----

function stepProgress(step: WizardStep, mode: Mode): { idx: number; total: number } {
  if (mode === 'reconfigure') {
    // Reconfigure shows only the microphone step.
    return { idx: 0, total: 1 };
  }
  const idx = Math.max(0, STEP_ORDER.indexOf(step));
  return { idx, total: STEP_ORDER.length };
}
