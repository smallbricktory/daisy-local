import { useEffect, useState, useSyncExternalStore } from 'react';
import { PRICING_URL } from '../lib/trial-banner';
import { open } from '@tauri-apps/plugin-dialog';
import { ConfirmDialog } from '../components/ConfirmDialog';
import { confirm } from '../lib/confirm';
import { revalidateAiProviderStatus } from '../lib/aiProviderStatus';
import { pushToast, updateToast } from '../lib/toastStore';
import { showGatewayNotice } from '../lib/gatewayNotice';
import { FieldError, FieldOk } from '../components/FieldError';
import { MarkdownView } from '../components/MarkdownView';
import { LEGAL_LAST_UPDATED, TOS_MARKDOWN, PRIVACY_MARKDOWN } from '../legalContent';
import {
  subscribeRecJob,
  getRecJobState,
  startDeleteAll,
} from '../recordingsJob';
import {
  tauri,
  errStr,
  formatBytes,
  providerLabel,
  type AecModeOverride,
  type AudioSourceInfo,
  type BootstrapStatus,
  type CalendarSubscription,
  type IntegrationPublic,
  type LicenseStatus,
  type LiveCaptionsChoice,
  type LiveCaptionsStatus,
  type ProviderId,
  type ProviderListEntry,
  type RecordingsStats,
  type Settings as SettingsT,
  type SpeechLevelInfo,
  type Prompt,
  type Tag,
  type UpsertIntegration,
  type WebhookAuthInput,
} from '../tauri';
import { MicLevel } from '../components/MicLevel';
import { McpSettings } from '../components/McpSettings';
import { WhisperModels } from '../components/WhisperModels';
import { ProviderEditor } from '../components/ProviderEditor';
import { TagChip } from '../components/tags/TagChip';
import { TagColorPicker } from '../components/tags/TagColorPicker';
import { buildProviderRows, isProviderConfigured } from '../lib/providerRows';
import { getZoom, subscribeZoom, setZoom } from '../lib/uiZoomController';
import { stepZoom, ZOOM_MIN, ZOOM_MAX, ZOOM_DEFAULT } from '../lib/uiZoom';
import { RUST_LICENSES, FRONTEND_PACKAGES } from '../licenses.generated';
import type { VoiceprintView } from '../tauri';

interface Props {
  onLocked: () => void;
  onLaunchWizard: () => void;
  /** Notifies App.tsx after the user adds / deletes a calendar
   *  subscription; App refreshes the `hasCalendar` state that
   *  drives the Calendar nav entry. */
  onCalendarsChanged?: () => void;
  /** Section to open on mount (e.g. deep-link from the trial banner's
   *  "Activate license" → the License UI lives in "About"). Ignored if it
   *  isn't a known section name. */
  initialSection?: string;
  /** Called after the license is activated/deactivated; App refreshes the
   *  trial banner. */
  onLicenseChanged?: () => void;
}

const SECTIONS = ['Recordings', 'Tags', 'Prompts', 'Providers', 'Integrations', 'Storage', 'Voiceprints', 'Calendars', 'MCP', 'Profile', 'About'] as const;

// Selects an AI provider, registering the Daisy Cloud install keypair first
// when the gateway is chosen. On failure, shows the Daisy Cloud notice and
// leaves the selection unchanged.
async function selectAiProvider(
  value: ProviderId | null,
  apply: (p: ProviderId | null) => void,
): Promise<void> {
  if (value === 'daisy_gateway') {
    try {
      await tauri.registerGateway();
    } catch {
      // Daisy Cloud is internal-only: any enable failure (not entitled, no
      // license, offline registration) gets the same informational notice.
      showGatewayNotice();
      return;
    }
  }
  apply(value);
}

// Per-machine live-captions control: Auto follows this machine's stored
// speed-check verdict (hardware detection until one exists); On/Off override.
function LiveCaptionsRow() {
  const [status, setStatus] = useState<LiveCaptionsStatus | null>(null);
  const [benching, setBenching] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  useEffect(() => { tauri.liveCaptionsStatus().then(setStatus).catch((e) => setErr(errStr(e))); }, []);

  async function setChoice(choice: LiveCaptionsChoice) {
    setErr(null);
    try { setStatus(await tauri.setLiveCaptionsChoice(choice)); } catch (e) { setErr(errStr(e)); }
  }
  async function runBench() {
    setErr(null);
    setBenching(true);
    try { setStatus(await tauri.runLiveCaptionsBench()); } catch (e) { setErr(errStr(e)); }
    finally { setBenching(false); }
  }

  if (!status) return err ? <p className="meta" style={{ color: 'var(--danger)' }}>{err}</p> : <p className="meta">Loading…</p>;

  const stateLine = status.source === 'override'
    ? `${status.enabled ? 'On' : 'Off'} — forced by DAISY_LIVE_CAPTIONS`
    : status.source === 'manual'
      ? `${status.enabled ? 'On' : 'Off'} — your setting for ${status.machine}`
      : status.source === 'bench'
        ? `${status.enabled ? 'On' : 'Off'} — measured ${status.bench_xrt?.toFixed(1)}× realtime on ${status.machine}`
        : `${status.enabled ? 'On' : 'Off'} — hardware default (no speed check yet on ${status.machine})`;

  return (
    <div>
      <div style={{ display: 'flex', gap: 8, alignItems: 'center', flexWrap: 'wrap' }}>
        <select
          value={status.choice}
          disabled={benching}
          onChange={(e) => void setChoice(e.target.value as LiveCaptionsChoice)}
          style={{ display: 'block', width: 320 }}
        >
          <option value="auto">Auto — by speed check on this machine (recommended)</option>
          <option value="on">On</option>
          <option value="off">Off — transcript only at finalize</option>
        </select>
        <button className="btn" disabled={benching} onClick={() => void runBench()}>
          {benching ? 'Running speed check… (~1 min)' : 'Run speed check'}
        </button>
      </div>
      <p style={{ margin: '8px 0 0', fontSize: 14 }}>{stateLine}</p>
      <p className="meta" style={{ fontSize: 12, marginTop: 6, lineHeight: 1.5 }}>
        Saved per machine, so one profile can run captions on your desktop and
        skip them on a slower laptop. The full transcript is always produced
        when you stop, either way.
      </p>
      {err && <p className="meta" style={{ color: 'var(--danger)', marginTop: 6 }}>{err}</p>}
    </div>
  );
}

// Whole-UI zoom control. Reads/writes the shared zoom store (also driven by
// the Cmd/Ctrl +/-/0 shortcuts); the percentage stays live regardless of
// source.
function TextSizeField() {
  const zoom = useSyncExternalStore(subscribeZoom, getZoom);
  const pct = Math.round(zoom * 100);
  const btn: React.CSSProperties = { minWidth: 34, fontWeight: 700 };
  return (
    <Field label="Text size">
      <div style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
        <button
          className="btn" style={btn} aria-label="Decrease text size"
          disabled={zoom <= ZOOM_MIN} onClick={() => setZoom(stepZoom(getZoom(), 'out'))}
        >A−</button>
        <span style={{ minWidth: 52, textAlign: 'center', fontVariantNumeric: 'tabular-nums' }}>{pct}%</span>
        <button
          className="btn" style={btn} aria-label="Increase text size"
          disabled={zoom >= ZOOM_MAX} onClick={() => setZoom(stepZoom(getZoom(), 'in'))}
        >A+</button>
        <button
          className="btn" disabled={zoom === ZOOM_DEFAULT}
          onClick={() => setZoom(ZOOM_DEFAULT)}
        >Reset</button>
      </div>
    </Field>
  );
}

type SectionName = (typeof SECTIONS)[number];

export function Settings({ onLocked, onLaunchWizard, onCalendarsChanged, initialSection, onLicenseChanged }: Props) {
  const [section, setSection] = useState<SectionName>(
    SECTIONS.includes(initialSection as SectionName) ? (initialSection as SectionName) : 'Recordings',
  );
  // Deep-link from elsewhere (e.g. the trial banner's "Activate license" →
  // About). The useState initializer runs only on first mount; this effect
  // applies a changed initialSection while Settings is already open.
  useEffect(() => {
    if (initialSection && SECTIONS.includes(initialSection as SectionName)) {
      setSection(initialSection as SectionName);
    }
  }, [initialSection]);
  const [settings, setSettings] = useState<SettingsT | null>(null);
  const [bs, setBs] = useState<BootstrapStatus | null>(null);
  const [sources, setSources] = useState<AudioSourceInfo[]>([]);
  const [providers, setProviders] = useState<ProviderListEntry[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [moving, setMoving] = useState(false);

  async function refresh() {
    try {
      const [s, b, src, p] = await Promise.all([
        tauri.readSettings(),
        tauri.bootstrapStatus(),
        tauri.listAudioSources(),
        tauri.listProviders(),
      ]);
      setSettings(s);
      setBs(b);
      setSources(src);
      setProviders(p);
    } catch (e: unknown) {
      setError(String((e as { message?: unknown })?.message ?? e));
    }
  }

  useEffect(() => { refresh(); }, []);

  // Resets scroll on section change. The outer <main> is the scrolling
  // container (workspace.css `.main { overflow-y: auto }`); the browser keeps
  // the previous scrollTop across tab switches.
  useEffect(() => {
    const main = document.querySelector('.main') as HTMLElement | null;
    if (main) main.scrollTo({ top: 0, left: 0, behavior: 'instant' as ScrollBehavior });
  }, [section]);

  async function update<K extends keyof SettingsT>(key: K, value: SettingsT[K]) {
    if (!settings) return;
    const next = { ...settings, [key]: value };
    setSettings(next);
    try {
      await tauri.writeSettings(next);
      // Provider / key changes flip AI-feature gating banners app-wide.
      if (key === 'default_summary_provider') revalidateAiProviderStatus();
    } catch (e: unknown) {
      setError(String((e as { message?: unknown })?.message ?? e));
    }
  }

  async function lock() {
    await tauri.lockVault();
    onLocked();
  }

  async function handleMoveProfile() {
    const picked = await open({ directory: true });
    if (!picked) return;
    if (typeof picked !== 'string') return;
    const currentPath = bs?.profile_dir ?? '(unknown)';
    const confirmed = await confirm({
      title: `Move all data to ${picked}?`,
      body: `Daisy will restart into the new location. Old data at ${currentPath} will remain in place until you delete it manually.`,
      confirmLabel: 'Move profile', danger: true,
    });
    if (!confirmed) return;
    setMoving(true);
    try {
      // Restarts the app on success — the catch only sees failures.
      await tauri.moveProfile(picked);
    } catch (e: unknown) {
      setError(String((e as { message?: unknown })?.message ?? e));
      setMoving(false);
    }
  }

  async function handleSwitchProfile() {
    const picked = await open({ directory: true, multiple: false });
    if (typeof picked !== 'string') return;
    try {
      const probe = await tauri.probeProfileDir(picked);
      if (probe.is_current) {
        setError('That is already the current profile directory.');
        return;
      }
      const currentPath = bs?.profile_dir ?? '(unknown)';
      const confirmed = await confirm(
        probe.has_profile
          ? {
              title: 'Switch to this profile?',
              body: `Daisy will restart into ${picked}. If its vault uses a passphrase, you'll need it to unlock. Your current profile stays intact at ${currentPath}.`,
              confirmLabel: 'Switch & restart', danger: false,
            }
          : {
              title: probe.empty ? 'Create a new profile here?' : 'Folder is not a Daisy profile — create one here?',
              body: `Daisy will restart and set up a fresh profile in ${picked}. Your current profile stays intact at ${currentPath}.`,
              confirmLabel: 'Create & restart', danger: false,
            },
      );
      if (!confirmed) return;
      // Restarts the app on success — the catch only sees failures.
      await tauri.switchProfile(picked);
    } catch (e: unknown) {
      setError(String((e as { message?: unknown })?.message ?? e));
    }
  }

  // Merges known providers with whatever the vault returned: rows appear for
  // providers without a key, including the zero-config Daisy Cloud entry the
  // backend synthesizes (see lib/providerRows).
  const providerRows: ProviderListEntry[] = buildProviderRows(providers);

  if (!settings || !bs) return <p className="meta">Loading…</p>;

  return (
    <div style={{ display: 'flex', gap: 28, alignItems: 'flex-start', padding: 32 }}>
      {/* top matches the container's 32px padding so the nav pins at its
          resting position instead of creeping up as content scrolls. */}
      <nav style={{ flexShrink: 0, width: 150, position: 'sticky', top: 32, paddingTop: 4 }}>
        <h1 className="h1 h1--sticky">Settings</h1>
        {SECTIONS.map((s) => (
          <button
            key={s}
            className="settings-tab"
            onClick={() => setSection(s)}
            style={{
              display: 'block', width: '100%', textAlign: 'left', padding: '7px 10px', marginBottom: 2,
              border: 'none',
              borderRadius: 6, cursor: 'pointer', fontSize: 14,
              background: section === s ? 'var(--tint)' : 'transparent',
              color: section === s ? 'var(--graphite)' : 'inherit',
              fontWeight: section === s ? 600 : 400,
            }}
          >
            {s}
          </button>
        ))}
      </nav>

      <div style={{ flex: 1, minWidth: 0, maxWidth: 720 }}>
        {section === 'Recordings' && <RecordingsSection settings={settings} sources={sources} providers={providers} update={update} />}
        {section === 'Tags' && <TagsSection />}
        {section === 'Prompts' && settings && <PromptsSection settings={settings} update={update} />}
        {section === 'Providers' && (
          <ProvidersSection
            providerRows={providerRows}
            onSaved={() => { refresh(); revalidateAiProviderStatus(); }}
            defaultSummary={settings.default_summary_provider}
            onDefaultSummaryChange={(p) => update('default_summary_provider', p)}
          />
        )}
        {section === 'Integrations' && <IntegrationsSection />}
        {section === 'Storage' && <StorageSection />}
        {section === 'Profile' && (
          <ProfileSection
            profileDir={bs.profile_dir}
            envOverride={bs.env_override}
            moving={moving}
            onSwitch={handleSwitchProfile}
            onMove={handleMoveProfile}
            onLock={lock}
            onLaunchWizard={onLaunchWizard}
            userDisplayName={settings.user_display_name ?? ''}
            onUserDisplayNameChange={(v) => update('user_display_name', v.trim() ? v.trim() : null)}
          />
        )}
        {section === 'Voiceprints' && (
          <VoiceprintsSection
            diarizer={settings.diarizer ?? 'kmeans'}
            onDiarizerChange={(v) => void update('diarizer', v)}
          />
        )}
        {section === 'Calendars' && <CalendarsSection onChanged={onCalendarsChanged} />}
        {section === 'MCP' && <McpSettings settings={settings} update={update} />}
        {section === 'About' && <AboutSection onLicenseChanged={onLicenseChanged} />}

        {error && <p className="meta" style={{ color: 'var(--danger)', marginTop: 16 }}>{error}</p>}
      </div>
    </div>
  );
}

// ============================================================================
// Recordings
// ============================================================================
function RecordingsSection({
  settings, sources, providers, update,
}: {
  settings: SettingsT;
  sources: AudioSourceInfo[];
  providers: ProviderListEntry[];
  update: <K extends keyof SettingsT>(key: K, value: SettingsT[K]) => void;
}) {
  // Only providers that are actually usable (key saved, or keyless with a
  // model). A configured-then-unconfigured current selection stays listed so
  // the select shows the truth instead of a blank.
  const summaryOptions: ProviderId[] = providers.filter(isProviderConfigured).map((p) => p.name);
  const current = settings.default_summary_provider;
  if (current && !summaryOptions.includes(current)) summaryOptions.push(current);

  return (
    <section>
      <h2 className="h2">Recordings</h2>

      <Field label="Default microphone">
        <div style={{ display: 'flex', alignItems: 'center', gap: 10, flexWrap: 'wrap' }}>
          <select
            value={settings.default_mic_source_id ?? ''}
            onChange={(e) => update('default_mic_source_id', e.target.value ? Number(e.target.value) : null)}
            style={{ display: 'block', width: 320 }}
          >
            <option value="">— none selected —</option>
            {sources.filter((s) => s.kind === 'mic').map((s) => (
              <option key={s.id} value={s.id}>{s.description} (id {s.id})</option>
            ))}
          </select>
          {settings.default_mic_source_id !== null && settings.default_mic_source_id !== undefined && (
            // The tauriSourceId path uses the Tauri-bridged mic level events;
            // getUserMedia is silent on Linux/WebKitGTK. The wizard's
            // mic-picker uses the same prop. MicLevel resolves any Web Audio
            // deviceId itself on Chromium hosts.
            <MicLevel
              tauriSourceId={settings.default_mic_source_id}
              width={120}
              height={8}
            />
          )}
        </div>
      </Field>

      <Field label="Speech level (advanced)">
        <SpeechLevelBlock sources={sources} defaultMicId={settings.default_mic_source_id ?? null} />
      </Field>

      <Field label="AI provider (summary, Q&A, chapters, analysis, polish)">
        <select
          value={settings.default_summary_provider ?? ''}
          onChange={(e) => void selectAiProvider(
            (e.target.value || null) as ProviderId | null,
            (p) => update('default_summary_provider', p),
          )}
          style={{ display: 'block', width: 320 }}
        >
          <option value="">None — use copy-paste with provided prompt</option>
          {summaryOptions.map((p) => <option key={p} value={p}>{providerLabel(p)}</option>)}
        </select>
        <p className="meta" style={{ fontSize: 12, marginTop: 6, lineHeight: 1.5 }}>
          AI features need a provider key. With "None" selected, the Copy buttons on
          Transcript and Summary include a prompt you can paste into ChatGPT, Claude,
          or any other LLM yourself.
        </p>
      </Field>

      <TextSizeField />

      <Field label="Live captions">
        <LiveCaptionsRow />
      </Field>

      <Field label="Echo cancellation (AEC)">
        <select
          value={settings.aec_mode_override}
          onChange={(e) => update('aec_mode_override', e.target.value as AecModeOverride)}
          style={{ display: 'block', width: 320 }}
        >
          <option value="auto">Auto — cancel echo only when needed</option>
          <option value="always">Always — force echo cancellation</option>
          <option value="never">Never — skip echo cancellation</option>
        </select>
      </Field>

      <Field label="Noise suppression">
        <label style={{ display: 'flex', gap: 8, alignItems: 'center', fontSize: 14 }}>
          <input
            type="checkbox"
            checked={settings.denoise_enabled}
            onChange={(e) => update('denoise_enabled', e.target.checked)}
          />
          <span>Clean up background noise in saved audio</span>
        </label>
      </Field>

      <Field label="Updates">
        <label style={{ display: 'flex', gap: 8, alignItems: 'center', fontSize: 14 }}>
          <input type="checkbox" checked={settings.auto_update_check} onChange={(e) => update('auto_update_check', e.target.checked)} />
          <span>Check for new versions automatically</span>
        </label>
      </Field>

      <Field label="Diagnostics">
        <label style={{ display: 'flex', gap: 8, alignItems: 'center', fontSize: 14 }}>
          <span>Debug logging</span>
          <select
            value={settings.debug_level ?? 'off'}
            onChange={(e) => update('debug_level', e.target.value as 'off' | 'basic' | 'full')}
          >
            <option value="off">Off</option>
            <option value="basic">Basic</option>
            <option value="full">Full (live-Whisper + audio tracing)</option>
          </select>
        </label>
        <button
          type="button"
          className="btn"
          style={{ marginTop: 8 }}
          onClick={() => { void tauri.openLogsDir(); }}
        >
          Open logs folder
        </button>
      </Field>

    </section>
  );
}

// Per-device speech level: shows the learned/calibrated/override level for a
// selected input, with a speak-to-calibrate flow and a manual dB override.
function SpeechLevelBlock({ sources, defaultMicId }: {
  sources: AudioSourceInfo[];
  defaultMicId: number | null;
}) {
  const mics = sources.filter((s) => s.kind === 'mic');
  const [selId, setSelId] = useState<number | null>(defaultMicId ?? mics[0]?.id ?? null);
  // Follow the default-mic picker above.
  useEffect(() => { if (defaultMicId != null) setSelId(defaultMicId); }, [defaultMicId]);
  const sel = mics.find((m) => m.id === selId) ?? null;
  const [levels, setLevels] = useState<SpeechLevelInfo[]>([]);
  const [calibrating, setCalibrating] = useState(false);
  const [calError, setCalError] = useState<string | null>(null);
  // Slider position while dragging; persisted on release and cleared once the
  // refreshed list is in (clearing earlier snaps the thumb to the stale value).
  const [sliderDbfs, setSliderDbfs] = useState<number | null>(null);

  const refresh = () => tauri.speechLevelsList().then(setLevels).catch(() => {});
  useEffect(() => { void refresh(); }, []);

  const info = sel ? levels.find((l) => l.device === sel.description) ?? null : null;
  const overrideOn = info?.override_dbfs != null;

  const sourceLabel =
    info?.source === 'override' ? 'manual override'
    : info?.source === 'calibration' ? 'calibrated'
    : info?.source === 'learned' ? `learned from ${info.samples} meeting${info.samples === 1 ? '' : 's'}`
    : null;

  const calibrate = async () => {
    if (!sel) return;
    setCalError(null);
    setCalibrating(true);
    try {
      await tauri.calibrateSpeechLevel(sel.id, sel.description);
      refresh();
    } catch (e) {
      setCalError(errStr(e));
    } finally {
      setCalibrating(false);
    }
  };

  return (
    <div style={{ display: 'flex', flexDirection: 'column', gap: 10 }}>
      <div style={{ display: 'flex', alignItems: 'center', gap: 10, flexWrap: 'wrap' }}>
        <select
          value={selId ?? ''}
          onChange={(e) => setSelId(e.target.value ? Number(e.target.value) : null)}
          style={{ display: 'block', width: 320 }}
        >
          {mics.map((s) => (
            <option key={s.id} value={s.id}>{s.description}</option>
          ))}
        </select>
        {sel && calibrating && <MicLevel tauriSourceId={sel.id} width={120} height={8} />}
      </div>
      <p className="meta" style={{ fontSize: 12, margin: 0 }}>
        {info?.effective_dbfs != null
          ? (
            <>
              Speech level:{' '}
              <span style={{ fontFamily: 'var(--font-mono)' }}>
                {info.effective_dbfs.toFixed(1)} dBFS
              </span>
              {sourceLabel ? ` (${sourceLabel})` : ''}
            </>
          )
          : 'No data yet for this microphone — it learns automatically from meetings, or calibrate below.'}
      </p>
      <div style={{ display: 'flex', alignItems: 'center', gap: 10 }}>
        <button type="button" className="btn" onClick={() => void calibrate()} disabled={!sel || calibrating}>
          {calibrating ? 'Listening… read a sentence aloud' : 'Calibrate by speaking'}
        </button>
      </div>
      {calError && <p className="meta" style={{ color: 'var(--danger)', margin: 0 }}>{calError}</p>}
      <label style={{ display: 'flex', alignItems: 'center', gap: 8, fontSize: 13 }}>
        <input
          type="checkbox"
          checked={overrideOn}
          disabled={!sel}
          onChange={(e) => {
            if (!sel) return;
            const v = e.target.checked
              ? Math.round((info?.effective_dbfs ?? -20) * 10) / 10
              : null;
            void tauri.speechLevelSetOverride(sel.description, v).then(refresh);
          }}
        />
        Manual override
      </label>
      {overrideOn && sel && (
        <div style={{ display: 'flex', alignItems: 'center', gap: 10 }}>
          <input
            type="range" min={-50} max={0} step={0.1}
            value={sliderDbfs ?? info?.override_dbfs ?? -20}
            onChange={(e) => setSliderDbfs(Number(e.target.value))}
            onPointerUp={(e) => {
              void tauri.speechLevelSetOverride(
                sel.description,
                Number((e.target as HTMLInputElement).value),
              ).then(refresh).then(() => setSliderDbfs(null));
            }}
            style={{ width: 220 }}
          />
          <span style={{ fontFamily: 'var(--font-mono)', fontSize: 12 }}>
            {(sliderDbfs ?? info?.override_dbfs ?? -20).toFixed(1)} dBFS
          </span>
        </div>
      )}
      <p className="meta" style={{ fontSize: 12, margin: 0, lineHeight: 1.5 }}>
        Daisy separates your voice from speaker bleed using this level. Auto is
        right for almost everyone; calibrate if your captions miss quiet speech
        or pick up the other side as you.
      </p>
    </div>
  );
}

// ============================================================================
// Providers (API keys + Local Whisper)
// ============================================================================
function ProvidersSection({
  providerRows, onSaved, defaultSummary, onDefaultSummaryChange,
}: {
  providerRows: ProviderListEntry[];
  onSaved: () => void;
  defaultSummary: ProviderId | null;
  onDefaultSummaryChange: (p: ProviderId | null) => void;
}) {
  return (
    <section>
      <h2 className="h2">Providers</h2>
      <p className="meta" style={{ fontSize: 13, marginBottom: 12 }}>
        AI features (summary, Q&amp;A, chapters, analysis, transcript polish)
        need a provider key. Transcription runs on-device. Keys stored encrypted
        in <code>&lt;profile&gt;/keys.vault.json</code>.
      </p>
      <p className="meta" style={{ fontSize: 13, marginBottom: 12 }}>
        Pick "None" to skip — the Copy buttons on Transcript / Summary include
        a prompt you can paste into ChatGPT, Claude, or any LLM yourself.
      </p>

      <Field label="AI provider">
        <select
          value={defaultSummary ?? ''}
          style={{ display: 'block', width: 320 }}
          onChange={(e) => void selectAiProvider((e.target.value || null) as ProviderId | null, onDefaultSummaryChange)}
        >
          <option value="">None — use copy-paste</option>
          {providerRows.map((p) => (
            <option key={p.name} value={p.name}>{providerLabel(p.name)}</option>
          ))}
        </select>
      </Field>

      {(() => {
        if (defaultSummary == null) return null;
        // Daisy Cloud is zero-config — no key/model editor.
        if (defaultSummary === 'daisy_gateway') {
          return (
            <p className="meta" style={{ fontSize: 13 }}>
              Daisy Cloud is zero-config — no API key or model to set. Requires a license
              that includes Daisy Cloud (internal use only).
            </p>
          );
        }
        const sel = providerRows.find((p) => p.name === defaultSummary);
        if (!sel) {
          return (
            <p className="meta" style={{ fontSize: 13 }}>
              No matching provider configured yet.
            </p>
          );
        }
        return <ProviderEditor key={`s-${sel.name}`} provider={sel} onSaved={onSaved} />;
      })()}

      {/* Advanced — collapsed by default. */}
      <div
        style={{
          marginTop: 28,
          padding: '14px 16px',
          border: '1px solid var(--frost-deep)',
          borderRadius: 10,
          background: 'var(--cream-pure)',
        }}
      >
        <h3 className="h2" style={{ marginTop: 0, marginBottom: 2, fontSize: 18 }}>Advanced</h3>

        <details style={{ marginTop: 16 }}>
          <summary style={{ cursor: 'pointer', fontWeight: 600, fontSize: 14 }}>
            Local transcription model
          </summary>
          <WhisperModels />
        </details>
      </div>
    </section>
  );
}

// ============================================================================
// Integrations (outbound webhook destinations)
// ============================================================================

function payloadSummaryStr(p: { summary: boolean; notes: boolean; transcript: boolean }): string {
  const on = [p.summary && 'summary', p.notes && 'notes', p.transcript && 'transcript'].filter(Boolean);
  return on.length ? on.join(', ') : 'nothing';
}

function IntegrationsSection() {
  const [items, setItems] = useState<IntegrationPublic[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  // `editing` = the integration being edited, or 'new', or null.
  const [editing, setEditing] = useState<IntegrationPublic | 'new' | null>(null);
  const [pendingDelete, setPendingDelete] = useState<{ id: string; name: string } | null>(null);
  const [deleting, setDeleting] = useState(false);

  const reload = () => {
    setError(null);
    tauri.listIntegrations().then(setItems).catch((e) => setError(String(e)));
  };
  useEffect(() => { reload(); }, []);

  async function performDelete() {
    if (!pendingDelete) return;
    setDeleting(true);
    try {
      await tauri.deleteIntegration(pendingDelete.id);
      setPendingDelete(null);
      reload();
    } catch (e) {
      setError(String(e));
    } finally {
      setDeleting(false);
    }
  }
  function del(id: string, name: string) {
    setPendingDelete({ id, name });
  }

  return (
    <section>
      <h2 className="h2">Integrations</h2>
      <p className="meta" style={{ fontSize: 13 }}>
        Outbound destinations for a meeting’s summary, notes and/or transcript. A webhook receives a single JSON POST — the platform on the other end (Zapier, n8n, your own endpoint…) decides what to do with it. Add as many as you like. Secrets are write-only — once saved, they’re never shown again.
      </p>

      {error && <p className="meta" style={{ color: 'var(--danger)', marginTop: 8 }}>{error}</p>}

      <div style={{ marginTop: 16 }}>
        {(items ?? []).map((it) =>
          editing && editing !== 'new' && editing.id === it.id ? (
            <IntegrationEditor key={it.id} initial={it} onSaved={() => { setEditing(null); reload(); }} onCancel={() => setEditing(null)} />
          ) : (
            <div key={it.id} style={{ display: 'flex', alignItems: 'center', gap: 12, padding: 'var(--space-2) 0', borderBottom: 'var(--rule)' }}>
              <div style={{ minWidth: 0, flex: 1 }}>
                <div style={{ fontWeight: 600, display: 'flex', alignItems: 'center', gap: 8 }}>
                  {!it.enabled && (
                    <span className="meta" style={{ fontSize: 11, border: '1px solid var(--frost-deep)', borderRadius: 3, padding: '0 4px' }}>disabled</span>
                  )}
                  {it.name}
                </div>
                <div className="meta" style={{ fontSize: 12, wordBreak: 'break-all' }}>
                  {it.kind} · {it.webhook_url} · auth: {it.auth_kind === 'header' ? `header ${it.auth_header_name}` : it.auth_kind} · sends: {payloadSummaryStr(it.payloads)}
                </div>
              </div>
              <button className="btn" onClick={() => setEditing(it)}>Edit</button>
              <button className="btn btn--danger" onClick={() => del(it.id, it.name)}>Delete</button>
            </div>
          ),
        )}
        {items && items.length === 0 && editing !== 'new' && <p className="meta" style={{ marginTop: 8 }}>No destinations yet.</p>}
      </div>

      {editing === 'new' ? (
        <IntegrationEditor onSaved={() => { setEditing(null); reload(); }} onCancel={() => setEditing(null)} />
      ) : (
        <button className="btn btn--primary" style={{ marginTop: 16 }} onClick={() => setEditing('new')}>+ Add destination</button>
      )}

      {pendingDelete && (
        <ConfirmDialog
          title={`Delete destination "${pendingDelete.name}"?`}
          body={
            <p style={{ margin: 0 }}>
              This removes the destination from your integrations. The key and URL
              are wiped from the vault. This can&apos;t be undone.
            </p>
          }
          confirmLabel={deleting ? 'Deleting…' : 'Delete'}
          danger
          onCancel={() => !deleting && setPendingDelete(null)}
          onConfirm={() => { void performDelete(); }}
        />
      )}
    </section>
  );
}

type AuthMode = 'keep' | 'none' | 'header' | 'bearer';

function IntegrationEditor({ initial, onSaved, onCancel }: { initial?: IntegrationPublic; onSaved: () => void; onCancel: () => void }) {
  const isEdit = !!initial;
  const [name, setName] = useState(initial?.name ?? '');
  const [url, setUrl] = useState(initial?.webhook_url ?? '');
  const [enabled, setEnabled] = useState(initial?.enabled ?? true);
  const [authMode, setAuthMode] = useState<AuthMode>(isEdit ? 'keep' : 'none');
  const [headerName, setHeaderName] = useState(initial?.auth_header_name ?? '');
  const [headerValue, setHeaderValue] = useState('');
  const [bearer, setBearer] = useState('');
  const [pSummary, setPSummary] = useState(initial?.payloads.summary ?? true);
  const [pNotes, setPNotes] = useState(initial?.payloads.notes ?? false);
  const [pTranscript, setPTranscript] = useState(initial?.payloads.transcript ?? false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function save() {
    setBusy(true);
    setError(null);
    let auth: WebhookAuthInput;
    if (authMode === 'keep') auth = { type: 'keep' };
    else if (authMode === 'none') auth = { type: 'none' };
    else if (authMode === 'header') auth = { type: 'header', name: headerName, value: headerValue };
    else auth = { type: 'bearer', token: bearer };
    const req: UpsertIntegration = {
      id: initial?.id ?? null,
      name,
      enabled,
      webhook_url: url,
      auth,
      payloads: { summary: pSummary, notes: pNotes, transcript: pTranscript },
    };
    try {
      await tauri.upsertIntegration(req);
      onSaved();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div style={{ padding: 'var(--space-3) var(--space-2)', borderBottom: 'var(--rule)', background: 'var(--tint)' }}>
      <div style={{ fontWeight: 600, marginBottom: 8 }}>{isEdit ? `Edit “${initial!.name}”` : 'New destination'}</div>

      <div style={{ marginTop: 8 }}>
        <label className="meta" style={{ fontSize: 13 }}>Name</label>
        <input style={{ display: 'block', width: '100%' }} value={name} disabled={busy} onChange={(e) => setName(e.target.value)} placeholder="e.g. Zapier — meeting notes" />
      </div>
      <div style={{ marginTop: 12 }}>
        <label className="meta" style={{ fontSize: 13 }}>Webhook URL <span style={{ opacity: 0.7 }}>(https://, or http://localhost for local n8n)</span></label>
        <input style={{ display: 'block', width: '100%' }} value={url} disabled={busy} onChange={(e) => setUrl(e.target.value)} placeholder="https://hooks.zapier.com/hooks/catch/…" />
      </div>

      <div style={{ marginTop: 12 }}>
        <label className="meta" style={{ fontSize: 13 }}>Auth</label>
        <div style={{ display: 'flex', flexWrap: 'wrap', gap: 14, marginTop: 4, fontSize: 13 }}>
          {isEdit && <label><input type="radio" checked={authMode === 'keep'} onChange={() => setAuthMode('keep')} /> keep current</label>}
          <label><input type="radio" checked={authMode === 'none'} onChange={() => setAuthMode('none')} /> none</label>
          <label><input type="radio" checked={authMode === 'header'} onChange={() => setAuthMode('header')} /> custom header</label>
          <label><input type="radio" checked={authMode === 'bearer'} onChange={() => setAuthMode('bearer')} /> bearer token</label>
        </div>
        {authMode === 'header' && (
          <div style={{ display: 'flex', gap: 8, marginTop: 8 }}>
            <input style={{ display: 'block', flex: 1 }} value={headerName} disabled={busy} onChange={(e) => setHeaderName(e.target.value)} placeholder="Header name (e.g. X-API-Key)" />
            <input style={{ display: 'block', flex: 2 }} type="password" value={headerValue} disabled={busy} onChange={(e) => setHeaderValue(e.target.value)} placeholder="Header value" />
          </div>
        )}
        {authMode === 'bearer' && (
          <input style={{ display: 'block', width: '100%', marginTop: 8 }} type="password" value={bearer} disabled={busy} onChange={(e) => setBearer(e.target.value)} placeholder="Bearer token" />
        )}
        {isEdit && authMode === 'keep' && initial!.auth_kind !== 'none' && (
          <p className="meta" style={{ fontSize: 12, marginTop: 4 }}>
            Currently using {initial!.auth_kind === 'header' ? `header “${initial!.auth_header_name}”` : 'a bearer token'} — leave this to keep it, or pick another option to replace it.
          </p>
        )}
      </div>

      <div style={{ marginTop: 12 }}>
        <label className="meta" style={{ fontSize: 13 }}>Include in the payload</label>
        <div style={{ display: 'flex', gap: 16, marginTop: 4, fontSize: 13 }}>
          <label><input type="checkbox" checked={pSummary} onChange={(e) => setPSummary(e.target.checked)} /> summary</label>
          <label><input type="checkbox" checked={pNotes} onChange={(e) => setPNotes(e.target.checked)} /> notes</label>
          <label><input type="checkbox" checked={pTranscript} onChange={(e) => setPTranscript(e.target.checked)} /> transcript</label>
        </div>
      </div>

      <div style={{ marginTop: 12, fontSize: 13 }}>
        <label><input type="checkbox" checked={enabled} onChange={(e) => setEnabled(e.target.checked)} /> enabled (appears in “Send to…” on a meeting)</label>
      </div>

      <div style={{ display: 'flex', gap: 8, marginTop: 16 }}>
        <button className="btn btn--primary" disabled={busy || !name.trim() || !url.trim()} onClick={save}>{busy ? 'Saving…' : 'Save'}</button>
        <button className="btn" disabled={busy} onClick={onCancel}>Cancel</button>
      </div>
      {error && <p className="meta" style={{ color: 'var(--danger)', marginTop: 8 }}>{error}</p>}
    </div>
  );
}

// ============================================================================
// Recordings (clear transcribed-or-orphaned audio)
// ============================================================================

function StorageSection() {
  const [stats, setStats] = useState<RecordingsStats | null>(null);
  const [confirmDelete, setConfirmDelete] = useState(false);
  const [confirmText, setConfirmText] = useState('');

  // Job state lives in a module store (recordingsJob.ts); the progress meter
  // and result survive navigating away from this section and back.
  const job = useSyncExternalStore(subscribeRecJob, getRecJobState);
  const { kind: busy, progress, message: msg } = job;

  const reload = () => {
    tauri.recordingsStats().then(setStats).catch(() => { /* keep last known stats */ });
  };
  // Refresh stats on mount and whenever a job finishes (busy -> null).
  useEffect(() => { reload(); }, [busy]);

  function runDelete() {
    setConfirmDelete(false);
    setConfirmText('');
    void startDeleteAll();
  }

  const pct = progress && progress.total > 0 ? Math.round((progress.done / progress.total) * 100) : 0;
  const deletable = stats?.deletable_session_count ?? 0;

  return (
    <section>
      <h2 className="h2">Storage</h2>
      <p className="meta" style={{ fontSize: 13 }}>
        Raw audio for your sessions lives in the profile directory. Moving it elsewhere isn’t supported here — but you can clear out the bulky raw recordings once they’ve been transcribed. Daisy already keeps a compact Opus copy of each meeting, so playback still works afterward.
      </p>

      {stats && (
        <p style={{ fontSize: 14, marginTop: 12 }}>
          <strong>{stats.session_count}</strong> recording{stats.session_count === 1 ? '' : 's'} ·{' '}
          <strong>{formatBytes(stats.wav_bytes)}</strong> of WAVs
          {stats.opus_bytes > 0 && <> · {formatBytes(stats.opus_bytes)} of Opus</>} ·{' '}
          <strong>{deletable}</strong> eligible to clear ({formatBytes(stats.deletable_bytes)})
        </p>
      )}

      {progress !== null && (
        <div style={{ marginTop: 16 }}>
          <div style={{ height: 8, background: 'var(--frost-deep)', borderRadius: 4, overflow: 'hidden' }}>
            <div style={{ height: '100%', width: `${pct}%`, background: 'var(--indigo-deep)', transition: 'width 120ms' }} />
          </div>
          <p className="meta" style={{ fontSize: 12, marginTop: 4 }}>
            Clearing {progress.done}/{progress.total}
            {progress.current ? ` — ${progress.current}` : ''}
          </p>
        </div>
      )}

      {msg && <p className="meta" style={{ fontSize: 13, marginTop: 12 }}>{msg}</p>}

      <div style={{ marginTop: 24, paddingTop: 16, borderTop: '1px solid var(--frost-deep)' }}>
        <h3 className="h3" style={{ color: 'var(--danger)' }}>Clear transcribed / orphaned recordings</h3>
        <p className="meta" style={{ fontSize: 13, marginTop: 4 }}>
          Deletes only the bulky raw WAV chunks for sessions that have already been transcribed, or that have no Library entry. The compact meeting.opus archive, transcripts, summaries, notes and the session record are always kept — so you can still play back and re-transcribe. This can’t be undone.
        </p>
        {!confirmDelete ? (
          <button
            onClick={() => setConfirmDelete(true)}
            disabled={busy !== null || deletable === 0}
            className="btn btn--danger"
            style={{ marginTop: 10 }}
          >
            Clear {deletable} recording{deletable === 1 ? '' : 's'}…
          </button>
        ) : (
          <div style={{ marginTop: 10, padding: 12, border: '1px solid var(--danger)', borderRadius: 6, background: 'var(--tint)' }}>
            <p style={{ fontSize: 13 }}>
              This will permanently delete the raw WAV audio for <strong>{deletable}</strong> session{deletable === 1 ? '' : 's'} and free about <strong>{formatBytes(stats?.deletable_bytes)}</strong>. Playback (meeting.opus), transcripts and summaries are kept. Type <code>DELETE</code> to confirm.
            </p>
            <input
              value={confirmText}
              onChange={(e) => setConfirmText(e.target.value)}
              placeholder="DELETE"
              style={{ marginTop: 8, width: 160 }}
            />
            <div style={{ display: 'flex', gap: 8, marginTop: 10 }}>
              <button onClick={runDelete} disabled={confirmText !== 'DELETE'} className="btn" style={{ color: 'var(--danger)', borderColor: 'var(--danger)' }}>
                Yes, delete the audio
              </button>
              <button onClick={() => { setConfirmDelete(false); setConfirmText(''); }} className="btn">Cancel</button>
            </div>
          </div>
        )}
      </div>
    </section>
  );
}

// ============================================================================
// Profile (+ vault lock)
// ============================================================================
function VoiceprintsSection({ diarizer, onDiarizerChange }: { diarizer: string; onDiarizerChange: (v: string) => void }) {
  const [rows, setRows] = useState<VoiceprintView[] | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const [editing, setEditing] = useState<VoiceprintView | null>(null);
  const [rematching, setRematching] = useState(false);
  const [rematchMsg, setRematchMsg] = useState<string | null>(null);
  const reload = () => {
    setErr(null);
    tauri.listVoiceprints()
      .then((v) => setRows([...v].sort((a, b) => a.display_name.localeCompare(b.display_name, undefined, { sensitivity: 'base' }))))
      .catch((e) => setErr(String((e as { message?: unknown })?.message ?? e)));
  };
  useEffect(() => { reload(); }, []);

  async function remove(v: VoiceprintView) {
    const ok = await confirm({
      title: `Delete voiceprint for ${v.display_name}?`,
      body: `Future meetings won't auto-label this person.`,
      confirmLabel: 'Delete', danger: true,
    });
    if (!ok) return;
    try { await tauri.deleteVoiceprint(v.id); reload(); }
    catch (e) { setErr(String((e as { message?: unknown })?.message ?? e)); }
  }

  async function rematchAll() {
    setRematching(true); setRematchMsg(null); setErr(null);
    try {
      const r = await tauri.rematchAllSessions();
      setRematchMsg(
        `Scanned ${r.sessions_scanned} session${r.sessions_scanned === 1 ? '' : 's'}, ` +
        `labelled ${r.clusters_matched} new speaker${r.clusters_matched === 1 ? '' : 's'}.`,
      );
      reload();
    } catch (e) {
      setErr(String((e as { message?: unknown })?.message ?? e));
    } finally {
      setRematching(false);
    }
  }

  return (
    <section>
      <h2 className="h2">Voiceprints</h2>
      <p className="meta" style={{ fontSize: 13, marginTop: 4, marginBottom: 16 }}>
        Voice embeddings Daisy uses to recognize speakers across meetings. Enrolled from the
        participants tab when you label a speaker — the audio sample never leaves your machine.
        Voiceprints are biometric data; only enroll people you have permission to identify.
      </p>

      <div style={{ marginBottom: 16, display: 'flex', alignItems: 'center', gap: 8 }}>
        <label className="meta" style={{ fontSize: 13 }}>Diarizer:</label>
        <select value={diarizer} onChange={(e) => onDiarizerChange(e.target.value)}>
          <option value="kmeans">k-means (default)</option>
          <option value="speakrs">speakrs (experimental)</option>
        </select>
      </div>

      {err && <p className="meta" style={{ color: 'var(--danger)' }}>{err}</p>}
      {rematchMsg && <p className="meta" style={{ color: 'var(--ok)' }}>{rematchMsg}</p>}
      {rows == null && <p className="meta">Loading…</p>}
      {rows && rows.length === 0 && (
        <p className="meta" style={{ marginTop: 12 }}>
          No voiceprints yet. Open a recording, click a speaker chip, and check
          “Save voiceprint for future meetings.”
        </p>
      )}
      {rows && rows.length > 0 && (
        <div style={{ marginBottom: 12, display: 'flex', alignItems: 'center', gap: 12 }}>
          <button
            className="btn"
            disabled={rematching}
            onClick={() => void rematchAll()}
            title="Re-scan every past meeting for known voiceprints and back-fill speaker labels."
          >
            {rematching ? 'Scanning…' : 'Rematch all sessions'}
          </button>
        </div>
      )}
      {rows && rows.length > 0 && (
        <table style={{ width: '100%', borderCollapse: 'collapse', fontSize: 13 }}>
          <thead>
            <tr style={{ textAlign: 'left', color: 'var(--muted)', borderBottom: '1px solid var(--frost-deep)' }}>
              <th style={{ padding: '6px 12px 6px 0' }}>Name</th>
              <th style={{ padding: '6px 12px' }}>Email</th>
              <th style={{ padding: '6px 12px' }}>Sessions</th>
              <th style={{ padding: '6px 12px' }}>Samples</th>
              <th style={{ padding: '6px 0' }}></th>
            </tr>
          </thead>
          <tbody>
            {rows.map((r) => (
              <tr key={r.id} style={{ borderBottom: '1px solid var(--frost-soft)' }}>
                <td style={{ padding: '8px 12px 8px 0' }}>{r.display_name}</td>
                <td style={{ padding: '8px 12px', color: 'var(--iron)' }}>{r.email ?? '—'}</td>
                <td style={{ padding: '8px 12px', color: 'var(--iron)' }}>{r.session_count}</td>
                <td style={{ padding: '8px 12px', color: 'var(--iron)' }} title="Voice samples in this person's gallery — more samples = more robust matching.">{r.sample_count}</td>
                <td style={{ padding: '8px 0', textAlign: 'right' }}>
                  <button className="btn" onClick={() => setEditing(r)}>Rename</button>{' '}
                  <button className="btn btn--danger" onClick={() => void remove(r)}>Delete</button>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
      {editing && (
        <VoiceprintRenameModal
          row={editing}
          onClose={() => setEditing(null)}
          onSaved={() => { setEditing(null); reload(); }}
        />
      )}
    </section>
  );
}

function VoiceprintRenameModal({
  row, onClose, onSaved,
}: {
  row: VoiceprintView;
  onClose: () => void;
  onSaved: () => void;
}) {
  const [name, setName] = useState(row.display_name);
  const [email, setEmail] = useState(row.email ?? '');
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  async function save() {
    setBusy(true); setErr(null);
    try {
      await tauri.renameVoiceprint(row.id, name, email.trim() || null);
      onSaved();
    } catch (e) {
      setErr(String((e as { message?: unknown })?.message ?? e));
      setBusy(false);
    }
  }
  return (
    <div className="modal-backdrop" onClick={busy ? undefined : onClose}>
      <div className="modal" onClick={(e) => e.stopPropagation()} style={{ maxWidth: 420 }}>
        <h2 className="h2" style={{ marginTop: 0 }}>Rename voiceprint</h2>
        <label style={{ display: 'block', fontSize: 13, color: 'var(--muted)', marginBottom: 4 }}>Name</label>
        <input type="text" autoFocus value={name} disabled={busy}
               onChange={(e) => setName(e.target.value)}
               style={{ width: '100%', marginBottom: 12 }} />
        <label style={{ display: 'block', fontSize: 13, color: 'var(--muted)', marginBottom: 4 }}>Email (optional)</label>
        <input type="email" value={email} disabled={busy}
               onChange={(e) => setEmail(e.target.value)}
               style={{ width: '100%', marginBottom: 12 }} />
        {err && <p className="meta" style={{ color: 'var(--danger)' }}>{err}</p>}
        <div style={{ display: 'flex', gap: 8, justifyContent: 'flex-end' }}>
          <button className="btn" onClick={onClose} disabled={busy}>Cancel</button>
          <button className="btn btn--primary" onClick={() => void save()} disabled={busy || !name.trim()}>
            {busy ? 'Saving…' : 'Save'}
          </button>
        </div>
      </div>
    </div>
  );
}

export function ProfileSection({
  profileDir, envOverride, moving, onMove, onSwitch, onLock, onLaunchWizard, userDisplayName, onUserDisplayNameChange,
}: {
  profileDir: string | null;
  envOverride: string | null;
  moving: boolean;
  onSwitch: () => void;
  onMove: () => void;
  onLock: () => void;
  onLaunchWizard: () => void;
  userDisplayName: string;
  onUserDisplayNameChange: (v: string) => void;
}) {
  const [draft, setDraft] = useState(userDisplayName);
  useEffect(() => { setDraft(userDisplayName); }, [userDisplayName]);
  // When licensed, the name is bound to the licensee (a signed claim in the
  // license) and can't be edited here.
  const [licensed, setLicensed] = useState(false);
  useEffect(() => {
    tauri.licenseStatus().then((s) => setLicensed(s.state === 'licensed')).catch(() => {});
  }, []);
  const dirty = draft.trim() !== (userDisplayName ?? '').trim();
  return (
    <section>
      <h2 className="h2">Profile</h2>
      <div style={{ marginBottom: 16 }}>
        <label style={{ display: 'block', fontSize: 13, color: 'var(--muted)', marginBottom: 4 }}>
          Your name
        </label>
        {licensed ? (
          <>
            <input type="text" value={userDisplayName} readOnly disabled style={{ width: 260, opacity: 0.7 }} />
            <p className="meta" style={{ fontSize: 12, marginTop: 6 }}>
              Set from your license and locked to the licensee. Used so the summarizer
              refers to you as &ldquo;you&rdquo; in TL;DRs and action items.
            </p>
          </>
        ) : (
          <>
            <div style={{ display: 'flex', gap: 8, alignItems: 'center' }}>
              <input
                type="text"
                value={draft}
                onChange={(e) => setDraft(e.target.value)}
                placeholder="e.g. Daisy"
                style={{ width: 260 }}
              />
              <button
                className="btn btn--primary"
                disabled={!dirty}
                onClick={() => onUserDisplayNameChange(draft.trim())}
              >
                {dirty ? 'Save' : 'Saved'}
              </button>
            </div>
            <p className="meta" style={{ fontSize: 12, marginTop: 6 }}>
              Used to identify you in recordings — the summarizer refers to you as &ldquo;you&rdquo;
              in TL;DRs and action items instead of by name.
            </p>
          </>
        )}
      </div>
      <table style={{ width: '100%', fontSize: 14 }}>
        <tbody><Row label="Profile directory" value={envOverride ?? profileDir ?? '—'} /></tbody>
      </table>
      {envOverride && (
        <p className="meta" style={{ fontSize: 11, marginTop: 2 }}>
          Set by the <span style={{ fontFamily: 'var(--font-mono)' }}>DAISY_PROFILE_DIR</span> environment
          variable for this session — overrides the saved location{profileDir ? ` (${profileDir})` : ''}.
        </p>
      )}
      <div style={{ marginTop: 8, display: 'flex', alignItems: 'center', gap: 12 }}>
        <button onClick={() => void tauri.openProfileDir().catch(() => {})} className="btn">Open Profile Directory</button>
        <button onClick={onSwitch} className="btn">Switch profile…</button>
        <button onClick={onMove} disabled={moving} className="btn">{moving ? 'Moving…' : 'Move profile…'}</button>
      </div>

      <div style={{ marginTop: 28, paddingTop: 16, borderTop: '1px solid var(--frost-deep)' }}>
        <h3 className="h3" style={{ margin: '0 0 8px' }}>Vault</h3>
        <VaultSection onLock={onLock} />
      </div>

      <div style={{ marginTop: 28, paddingTop: 16, borderTop: '1px solid var(--frost-deep)' }}>
        <h3 className="h3" style={{ margin: '0 0 8px' }}>Re-run setup wizard</h3>
        <button onClick={onLaunchWizard} className="btn">
          ↻ Open wizard
        </button>
      </div>
    </section>
  );
}

/** Vault management. Shows one of two mutually-exclusive faces by mode:
 *   - machine ("trust this machine"): plain text + "Add a passphrase…". No
 *     lock button and no change-passphrase.
 *   - passphrase: a single card with Change passphrase + Lock now + Drop
 *     passphrase. */
function VaultSection({ onLock }: { onLock: () => void }) {
  const [kind, setKind] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const [msg, setMsg] = useState<string | null>(null);
  const [changing, setChanging] = useState(false);
  const [oldPass, setOldPass] = useState('');
  const [newPass, setNewPass] = useState('');
  const [confirmPass, setConfirmPass] = useState('');
  const [adding, setAdding] = useState(false);
  const [addPass, setAddPass] = useState('');
  const [addConfirm, setAddConfirm] = useState('');

  useEffect(() => { tauri.vaultKind().then(setKind).catch(() => setKind('passphrase')); }, []);
  if (kind == null) return null;

  const flash = (m: string) => { setMsg(m); window.setTimeout(() => setMsg(null), 4000); };

  async function changePassphrase() {
    setErr(null);
    if (newPass.length < 22) { setErr('New passphrase must be at least 22 characters.'); return; }
    if (newPass !== confirmPass) { setErr("New passphrase + confirmation don't match."); return; }
    if (oldPass === newPass) { setErr('New passphrase must differ from the current one.'); return; }
    setBusy(true);
    try {
      await tauri.changeVaultPassphrase(oldPass, newPass);
      setChanging(false); setOldPass(''); setNewPass(''); setConfirmPass('');
      flash('✓ Passphrase changed.');
    } catch (e) { setErr(errStr(e)); } finally { setBusy(false); }
  }

  async function addPassphrase() {
    setErr(null);
    if (addPass.length < 22) { setErr('Passphrase must be at least 22 characters.'); return; }
    if (addPass !== addConfirm) { setErr("Passphrase + confirmation don't match."); return; }
    setBusy(true);
    try {
      const k = await tauri.switchVaultMode(addPass);
      setKind(k); setAdding(false); setAddPass(''); setAddConfirm('');
      flash('✓ Passphrase added — Daisy will ask for it on next launch.');
    } catch (e) { setErr(errStr(e)); } finally { setBusy(false); }
  }

  async function dropPassphrase() {
    const ok = await confirm({
      title: 'Drop the passphrase?',
      body:
        'Your keys are kept — re-encrypted under a key derived from this machine, so anyone ' +
        'who can read your files + run Daisy here can read them. If you reinstall the OS, the ' +
        'vault is unrecoverable.',
      confirmLabel: 'Drop passphrase', danger: true,
    });
    if (!ok) return;
    setErr(null); setBusy(true);
    try {
      const k = await tauri.switchVaultMode(null);
      setKind(k);
      flash('✓ Switched to trust-this-machine — no passphrase needed.');
    } catch (e) { setErr(errStr(e)); } finally { setBusy(false); }
  }

  if (kind === 'machine') {
    return (
      <div>
        <p className="meta" style={{ fontSize: 13, margin: '0 0 10px' }}>
          <strong>Trust this machine.</strong> Your API keys + voiceprints are encrypted under a
          machine-derived key, so Daisy unlocks automatically — no passphrase to enter.
        </p>
        {!adding ? (
          <button className="btn" disabled={busy} onClick={() => { setErr(null); setAdding(true); }}>
            Add a passphrase…
          </button>
        ) : (
          <div style={{ display: 'flex', flexDirection: 'column', gap: 8, maxWidth: 340 }}>
            <input type="password" autoComplete="new-password" placeholder="New passphrase"
              value={addPass} onChange={(e) => setAddPass(e.target.value)} disabled={busy} style={{ width: '100%' }} />
            <input type="password" autoComplete="new-password" placeholder="Confirm passphrase"
              value={addConfirm} onChange={(e) => setAddConfirm(e.target.value)} disabled={busy}
              onKeyDown={(e) => { if (e.key === 'Enter') void addPassphrase(); }} style={{ width: '100%' }} />
            <div style={{ display: 'flex', gap: 8 }}>
              <button className="btn btn--primary" disabled={busy || !addPass} onClick={() => void addPassphrase()}>
                {busy ? 'Migrating…' : 'Add passphrase'}
              </button>
              <button className="btn" disabled={busy} onClick={() => { setAdding(false); setAddPass(''); setAddConfirm(''); setErr(null); }}>Cancel</button>
            </div>
            <p className="meta" style={{ fontSize: 11, margin: 0 }}>
              Min 22 characters. Re-encrypts your existing keys in place — no data loss. No recovery if forgotten.
            </p>
          </div>
        )}
        <FieldError>{err}</FieldError>
        <FieldOk>{msg}</FieldOk>
      </div>
    );
  }

  return (
    <div style={{ padding: 12, border: '1px solid var(--frost-deep)', borderRadius: 8 }}>
      <p className="meta" style={{ fontSize: 13, margin: '0 0 10px' }}>
        <strong>Passphrase-protected.</strong> Daisy asks for your passphrase each launch.
      </p>
      {!changing ? (
        <div style={{ display: 'flex', gap: 8, flexWrap: 'wrap' }}>
          <button className="btn" disabled={busy} onClick={() => { setErr(null); setChanging(true); }}>Change passphrase…</button>
          <button className="btn" disabled={busy} onClick={onLock}>Lock vault now</button>
          <button className="btn" disabled={busy} onClick={() => void dropPassphrase()}>Drop passphrase (trust this machine)…</button>
        </div>
      ) : (
        <div style={{ display: 'flex', flexDirection: 'column', gap: 8, maxWidth: 340 }}>
          <input type="password" autoComplete="current-password" placeholder="Current passphrase"
            value={oldPass} onChange={(e) => setOldPass(e.target.value)} disabled={busy} style={{ width: '100%' }} />
          <input type="password" autoComplete="new-password" placeholder="New passphrase"
            value={newPass} onChange={(e) => setNewPass(e.target.value)} disabled={busy} style={{ width: '100%' }} />
          <input type="password" autoComplete="new-password" placeholder="Confirm new passphrase"
            value={confirmPass} onChange={(e) => setConfirmPass(e.target.value)} disabled={busy}
            onKeyDown={(e) => { if (e.key === 'Enter') void changePassphrase(); }} style={{ width: '100%' }} />
          <div style={{ display: 'flex', gap: 8 }}>
            <button className="btn btn--primary" disabled={busy || !oldPass || !newPass} onClick={() => void changePassphrase()}>
              {busy ? 'Changing…' : 'Save new passphrase'}
            </button>
            <button className="btn" disabled={busy} onClick={() => { setChanging(false); setOldPass(''); setNewPass(''); setConfirmPass(''); setErr(null); }}>Cancel</button>
          </div>
          <p className="meta" style={{ fontSize: 11, margin: 0 }}>
            Min 22 characters. No recovery if you forget it.
          </p>
        </div>
      )}
      <FieldError>{err}</FieldError>
      <FieldOk>{msg}</FieldOk>
    </div>
  );
}


// ============================================================================
// About
// ============================================================================

function CalendarsSection({ onChanged }: { onChanged?: () => void }) {
  const [subs, setSubs] = useState<CalendarSubscription[] | null>(null);
  const [tags, setTags] = useState<Tag[]>([]);
  const [err, setErr] = useState<string | null>(null);
  const [name, setName] = useState('');
  const [url, setUrl] = useState('');
  const [busy, setBusy] = useState(false);
  const [refreshing, setRefreshing] = useState(false);
  const [refreshMsg, setRefreshMsg] = useState<string | null>(null);
  const [editingColorFor, setEditingColorFor] = useState<string | null>(null);

  const reload = () => {
    setErr(null);
    Promise.all([tauri.listCalendarSubscriptions(), tauri.listTags()])
      .then(([s, t]) => { setSubs(s); setTags(t); })
      .catch((e) => setErr(errStr(e)));
  };
  useEffect(() => { reload(); }, []);

  async function add() {
    if (!name.trim() || !url.trim()) return;
    setBusy(true); setErr(null);
    try {
      await tauri.addCalendarSubscription(name.trim(), url.trim());
      setName(''); setUrl('');
      reload();
      onChanged?.();
    } catch (e) {
      setErr(errStr(e));
    } finally {
      setBusy(false);
    }
  }

  async function toggle(s: CalendarSubscription, enabled: boolean) {
    try {
      await tauri.updateCalendarSubscription({ id: s.id, enabled });
      reload();
    } catch (e) {
      setErr(errStr(e));
    }
  }

  async function changeColor(s: CalendarSubscription, color_hex: string) {
    try {
      await tauri.updateCalendarSubscription({ id: s.id, color_hex });
      reload();
    } catch (e) { setErr(errStr(e)); }
  }

  async function changeTag(s: CalendarSubscription, tagIdOrEmpty: string) {
    try {
      // "" = clear; backend treats empty string as the explicit-remove sentinel.
      await tauri.updateCalendarSubscription({ id: s.id, tag_id: tagIdOrEmpty });
      reload();
    } catch (e) { setErr(errStr(e)); }
  }

  async function remove(s: CalendarSubscription) {
    const ok = await confirm({
      title: `Delete subscription "${s.name}"?`,
      body: 'Cached events for this calendar will be cleared immediately. Recordings you already made from its events keep their calendar link — the orphaned reference is harmless.',
      confirmLabel: 'Delete', danger: true,
    });
    if (!ok) return;
    try {
      await tauri.deleteCalendarSubscription(s.id);
      reload();
      onChanged?.();
    } catch (e) {
      setErr(errStr(e));
    }
  }

  async function refresh() {
    setRefreshing(true); setRefreshMsg(null); setErr(null);
    try {
      const r = await tauri.refreshCalendars();
      const errPart = r.errors.length > 0 ? ` (${r.errors.length} error${r.errors.length === 1 ? '' : 's'})` : '';
      setRefreshMsg(`Loaded ${r.events_loaded} event${r.events_loaded === 1 ? '' : 's'} from ${r.subscriptions_scanned} calendar${r.subscriptions_scanned === 1 ? '' : 's'}${errPart}.`);
      if (r.errors.length > 0) setErr(r.errors.join('\n'));
    } catch (e) {
      setErr(errStr(e));
    } finally {
      setRefreshing(false);
    }
  }

  return (
    <section>
      <h2 className="h2">Calendars</h2>
      <p className="meta" style={{ fontSize: 13, marginTop: 4, marginBottom: 16 }}>
        Paste read-only ICS URLs from Google Calendar (Settings → "Secret address in iCal format"),
        Microsoft 365 Outlook (Share → Publish → ICS link), or Apple Calendar (calendar.icloud.com share).
        Daisy fetches them on demand and surfaces upcoming meetings; the next event's attendees are
        suggested when you start recording. URLs are stored locally — nothing leaves your machine
        except the HTTP fetch to your calendar provider.
      </p>
      {err && <p className="meta" style={{ color: 'var(--danger)', whiteSpace: 'pre-wrap' }}>{err}</p>}
      {refreshMsg && <p className="meta" style={{ color: 'var(--ok)' }}>{refreshMsg}</p>}

      <div style={{ marginBottom: 16, padding: 12, border: '1px dashed var(--frost-deep)', borderRadius: 6 }}>
        <label style={{ display: 'block', fontSize: 13, color: 'var(--muted)', marginBottom: 4 }}>Name</label>
        <input
          type="text" value={name} disabled={busy}
          onChange={(e) => setName(e.target.value)}
          placeholder="e.g. Work, Personal"
          style={{ display: 'block', width: 320, marginBottom: 8 }}
        />
        <label style={{ display: 'block', fontSize: 13, color: 'var(--muted)', marginBottom: 4 }}>ICS URL</label>
        <input
          type="url" value={url} disabled={busy}
          onChange={(e) => setUrl(e.target.value)}
          placeholder="https://calendar.google.com/calendar/ical/.../basic.ics"
          style={{ display: 'block', width: '100%', maxWidth: 720, marginBottom: 10 }}
        />
        <button className="btn btn--primary" disabled={busy || !name.trim() || !url.trim()} onClick={() => void add()}>
          {busy ? 'Adding…' : 'Add subscription'}
        </button>
      </div>

      {subs && subs.length > 0 && (
        <div style={{ marginBottom: 12, display: 'flex', alignItems: 'center', gap: 12 }}>
          <button className="btn" disabled={refreshing} onClick={() => void refresh()}>
            {refreshing ? 'Refreshing…' : '↻ Refresh now'}
          </button>
        </div>
      )}

      {subs == null && <p className="meta">Loading…</p>}
      {subs && subs.length === 0 && (
        <p className="meta" style={{ marginTop: 12 }}>No calendars yet. Add one above — Daisy will suggest attendees when you record and remind you just before meetings start.</p>
      )}
      {subs && subs.length > 0 && (
        <table style={{ width: '100%', borderCollapse: 'collapse', fontSize: 13 }}>
          <thead>
            <tr style={{ textAlign: 'left', color: 'var(--muted)', borderBottom: '1px solid var(--frost-deep)' }}>
              <th style={{ padding: '6px 8px 6px 0', width: 60 }}>Enabled</th>
              <th style={{ padding: '6px 8px', width: 36 }}>Color</th>
              <th style={{ padding: '6px 8px' }}>Name</th>
              <th style={{ padding: '6px 8px', width: 160 }}>Auto-tag</th>
              <th style={{ padding: '6px 8px' }}>URL</th>
              <th style={{ padding: '6px 0', width: 70 }}></th>
            </tr>
          </thead>
          <tbody>
            {subs.map((s) => (
              <tr key={s.id} style={{ borderBottom: '1px solid var(--frost-soft)', verticalAlign: 'top' }}>
                <td style={{ padding: '8px 8px 8px 0' }}>
                  <input
                    type="checkbox" checked={s.enabled}
                    onChange={(e) => void toggle(s, e.target.checked)}
                  />
                </td>
                <td style={{ padding: '8px 8px', position: 'relative' }}>
                  <button
                    className="color-swatch"
                    onClick={() => setEditingColorFor(editingColorFor === s.id ? null : s.id)}
                    aria-label="Pick color"
                    title="Pick color"
                    style={{
                      width: 22, height: 22, borderRadius: 6,
                      background: s.color_hex, border: '1px solid var(--frost-deep)',
                      cursor: 'pointer', padding: 0,
                    }}
                  />
                  {editingColorFor === s.id && (
                    <div style={{ position: 'absolute', zIndex: 10, top: 36, left: 0, padding: 8, background: 'var(--cream-pure)', border: '1px solid var(--frost-deep)', borderRadius: 6, boxShadow: '0 6px 18px -8px rgba(0,0,0,0.2)' }}>
                      <TagColorPicker value={s.color_hex} onChange={(hex) => { void changeColor(s, hex); setEditingColorFor(null); }} />
                    </div>
                  )}
                </td>
                <td style={{ padding: '8px 8px' }}>{s.name}</td>
                <td style={{ padding: '8px 8px' }}>
                  <select
                    value={s.tag_id ?? ''}
                    onChange={(e) => void changeTag(s, e.target.value)}
                    style={{ width: '100%' }}
                    title="Recordings started from this calendar's events get auto-tagged with this tag."
                  >
                    <option value="">(no auto-tag)</option>
                    {tags.map((t) => (
                      <option key={t.id} value={t.id}>{t.name}</option>
                    ))}
                  </select>
                </td>
                <td style={{ padding: '8px 8px', color: 'var(--iron)', fontFamily: 'var(--font-mono)', fontSize: 11, overflow: 'hidden', textOverflow: 'ellipsis', maxWidth: 280, whiteSpace: 'nowrap' }} title={s.url}>
                  {hideQuery(s.url)}
                </td>
                <td style={{ padding: '8px 0', textAlign: 'right' }}>
                  <button className="btn btn--danger" onClick={() => void remove(s)}>Delete</button>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </section>
  );
}

function hideQuery(url: string): string {
  // ICS URLs from Google/Microsoft embed a privacy token in the path, not
  // the query string. Shows the origin + a redacted path; the token is not
  // displayed.
  try {
    const u = new URL(url);
    return `${u.origin}${u.pathname.replace(/[^/]+(?=\.ics$|\/$|$)/g, '…')}`;
  } catch {
    return url;
  }
}


function LicenseInfo({ onChanged }: { onChanged?: () => void }) {
  const [status, setStatus] = useState<LicenseStatus | null>(null);
  const [key, setKey] = useState('');
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const [fingerprint, setFingerprint] = useState('');

  const reload = () => { tauri.licenseStatus().then(setStatus).catch(() => setStatus(null)); };
  useEffect(() => { reload(); }, []);

  // A short, stable fingerprint of the license token (SHA-256, first 16
  // hex); the full token is never shown on screen.
  useEffect(() => {
    const tok = status?.state === 'licensed' ? status.key : '';
    if (!tok) { setFingerprint(''); return; }
    crypto.subtle.digest('SHA-256', new TextEncoder().encode(tok))
      .then((buf) => {
        const hex = Array.from(new Uint8Array(buf)).map((b) => b.toString(16).padStart(2, '0')).join('');
        setFingerprint(hex.slice(0, 16));
      })
      .catch(() => setFingerprint(''));
  }, [status]);

  async function activate() {
    if (!key.trim()) return;
    setBusy(true); setErr(null);
    try { await tauri.activateLicense(key.trim()); setKey(''); reload(); onChanged?.(); }
    catch (e) { setErr(errStr(e)); }
    finally { setBusy(false); }
  }
  async function deactivate() {
    const ok = await confirm({
      title: 'Deactivate this device?',
      body: 'It frees one of your 3 seats so you can use the license elsewhere. Your data stays on disk.',
      confirmLabel: 'Deactivate', danger: true,
    });
    if (!ok) return;
    setBusy(true); setErr(null);
    try { await tauri.deactivateLicense(); reload(); onChanged?.(); }
    catch (e) { setErr(errStr(e)); }
    finally { setBusy(false); }
  }

  if (!status) return null;

  return (
    <div style={{ marginTop: 20, paddingTop: 16, borderTop: '1px solid var(--frost-deep)', width: '100%', textAlign: 'left' }}>
      <h3 className="h3" style={{ margin: '0 0 8px' }}>License</h3>
      {status.state === 'licensed' && (
        <>
          <p style={{ fontSize: 14, margin: '0 0 4px' }}>
            Licensed to <strong>{status.name || '—'}</strong>
            {status.email && <> &lt;{status.email}&gt;</>}
          </p>
          <p className="meta" style={{ fontSize: 11, fontFamily: 'var(--font-mono)' }}>
            License ID: {fingerprint || '…'}
          </p>
          {status.expires && (
            <p className="meta" style={{ fontSize: 12 }}>Expires {new Date(status.expires * 1000).toLocaleDateString()}</p>
          )}
          <p className="meta" style={{ fontSize: 11, marginTop: 8 }}>
            This license allows 3 devices.{' '}
            <button
              onClick={() => void deactivate()}
              disabled={busy}
              className="btn-link"
            >Deactivate this device</button>{' '}to free a seat.
          </p>
        </>
      )}
      {status.state !== 'licensed' && (
        <>
          <p className="meta" style={{ fontSize: 13, marginBottom: 8 }}>
            {status.state === 'trial' ? `Trial — ${status.days_left} day${status.days_left === 1 ? '' : 's'} left.` : 'Trial ended.'}
          </p>
          <textarea
            value={key} disabled={busy}
            onChange={(e) => setKey(e.target.value)}
            placeholder="paste your license key"
            rows={2}
            className="textarea--mono" style={{ width: '100%' }}
          />
          <div style={{ display: 'flex', gap: 8, marginTop: 8 }}>
            <button className="btn btn--primary" disabled={busy || !key.trim()} onClick={() => void activate()}>Activate</button>
            <button className="btn" onClick={() => { void tauri.openExternal(PRICING_URL); }}>Get a license ↗</button>
          </div>
        </>
      )}
      {err && <p className="meta" style={{ color: 'var(--danger)', marginTop: 8 }}>{err}</p>}
    </div>
  );
}

// Read-only viewer for the in-app Terms of Service + Privacy Policy the user
// accepted at first run (the embedded copy, available offline).
function LegalViewer({ onClose }: { onClose: () => void }) {
  const [tab, setTab] = useState<'tos' | 'privacy'>('tos');
  return (
    <div className="modal-backdrop" onClick={onClose}>
      <div
        className="modal"
        style={{ width: 'min(720px, 94vw)', maxHeight: '86vh', display: 'flex', flexDirection: 'column' }}
        onClick={(e) => e.stopPropagation()}
      >
        <div className="modal__title">Terms &amp; Privacy</div>
        <p className="meta" style={{ fontSize: 12, margin: '2px 0 10px' }}>
          The version you accepted. Last updated {LEGAL_LAST_UPDATED}.
        </p>
        <div style={{ display: 'flex', gap: 8, marginBottom: 8 }}>
          {(['tos', 'privacy'] as const).map((t) => (
            <button
              key={t}
              className="btn"
              onClick={() => setTab(t)}
              style={{ fontWeight: tab === t ? 600 : 400, background: tab === t ? 'var(--tint)' : undefined }}
            >
              {t === 'tos' ? 'Terms of Service' : 'Privacy Policy'}
            </button>
          ))}
        </div>
        <div style={{ flex: 1, minHeight: 0, overflowY: 'auto', border: '1px solid var(--frost-deep)', borderRadius: 8, padding: '12px 16px', textAlign: 'left' }}>
          <MarkdownView markdown={tab === 'tos' ? TOS_MARKDOWN : PRIVACY_MARKDOWN} />
        </div>
        <div className="modal__actions">
          <button className="btn" onClick={onClose}>Close</button>
        </div>
      </div>
    </div>
  );
}

function AboutSection({ onLicenseChanged }: { onLicenseChanged?: () => void }) {
  const year = new Date().getFullYear();
  const [build, setBuild] = useState<{ version: string; sha: string; tagged: boolean } | null>(null);
  const [showLegal, setShowLegal] = useState(false);
  const [benching, setBenching] = useState(false);
  useEffect(() => { tauri.buildInfo().then(setBuild).catch(() => {}); }, []);

  const runBench = () => {
    if (benching) return;
    setBenching(true);
    pushToast({
      id: 'whisper-bench',
      severity: 'working',
      title: 'Benchmark in progress…',
      body: 'Measuring local transcription speed on this machine',
    });
    tauri.runLiveCaptionsBench()
      .then((st) => updateToast('whisper-bench', {
        severity: 'done',
        title: 'Benchmark complete',
        body: `${st.bench_xrt?.toFixed(1) ?? '?'}× realtime on ${st.machine} — live captions ${st.enabled ? 'on' : 'off'}`,
        dismissible: true,
        autoDismissMs: 20000,
      }))
      .catch((e) => updateToast('whisper-bench', {
        severity: 'error',
        title: 'Benchmark failed',
        body: String(e),
        dismissible: true,
        autoDismissMs: 10000,
      }))
      .finally(() => setBenching(false));
  };
  return (
    <section>
      <h2 className="h2">About</h2>

      <div style={{ display: 'flex', flexDirection: 'column', alignItems: 'center', textAlign: 'center', marginTop: 20 }}>
        <img src="/daisy-logo.png" alt="Daisy" width={128} height={128} style={{ borderRadius: 24, boxShadow: '0 8px 28px rgba(0,0,0,0.18)' }} />
        <p style={{ fontSize: 16, marginTop: 16 }}>
          <strong>Daisy</strong> — Record · Recognize · Recall
        </p>
        <p className="meta" style={{ fontSize: 13, marginTop: 4 }}>
          Version {build?.version ?? '…'}{build?.sha ? ` · build ${build.sha}` : ''}
        </p>
        <button
          className="btn"
          onClick={() => setShowLegal(true)}
          style={{ marginTop: 8 }}
        >
          View Terms &amp; Privacy
        </button>
        <button
          className="btn"
          onClick={runBench}
          disabled={benching}
          style={{ marginTop: 8 }}
          title="Measure how fast local transcription runs on this machine. Result is logged to daisy.log."
        >
          {benching ? 'Running speed check…' : 'Run speed check (diagnostic)'}
        </button>
        {build && !build.tagged && (
          <p
            style={{
              marginTop: 20,
              color: '#b45309',
              fontWeight: 800,
              fontSize: 13,
              letterSpacing: '0.04em',
              textTransform: 'uppercase',
              textAlign: 'center',
            }}
          >
            For testing purposes only — do not distribute
          </p>
        )}
        <LicenseInfo onChanged={onLicenseChanged} />
      </div>
      {showLegal && <LegalViewer onClose={() => setShowLegal(false)} />}

      <div style={{ marginTop: 28, paddingTop: 20, borderTop: '1px solid var(--frost-deep)', fontSize: 13, lineHeight: 1.6, textAlign: 'center' }}>
        <div
          aria-label="Smallbricktory"
          style={{
            // Company brand lockup — keeps its serif, like the Daisy
            // wordmark in the rail. Fraunces isn't bundled; Georgia carries it.
            fontFamily: "'Fraunces', Georgia, 'Times New Roman', serif",
            fontSize: 28,
            fontWeight: 600,
            letterSpacing: '-0.04em',
            textTransform: 'uppercase',
            color: 'var(--indigo)',
            lineHeight: 1,
            marginBottom: 10,
          }}
        >
          Smallbricktory
        </div>
        <p>A Small Bricktory Production · © {year}</p>
        <p className="meta">
          <button
            onClick={() => { void tauri.openExternal('https://www.daisylocal.app'); }}
            className="btn-link"
          >www.daisylocal.app ↗</button>
        </p>
        <p className="meta">
          <button
            onClick={() => { void tauri.openExternal('https://github.com/smallbricktory/daisy-local'); }}
            className="btn-link"
          >github.com/smallbricktory/daisy-local ↗</button>
        </p>
        <p className="meta" style={{ marginTop: 12 }}>
          Daisy is source-visible software under the Elastic License 2.0 with an
          additional permission: the source is published so you can verify what the app
          does — read it, modify it, build it yourself, and for personal, non-commercial
          use run your own build without a license. Commercial use requires a paid
          license; providing Daisy to others as a product or service is not permitted.
          Official signed builds and license keys are the product. It bundles
          open-source components under their respective licenses — Opus audio compression by
          Xiph.Org (BSD-3-Clause), local transcription via whisper.cpp (MIT), and a
          permissive (MIT / Apache-2.0 / BSD-style) Rust + React stack. Full attribution and
          license text below.
        </p>
      </div>

      <div style={{ marginTop: 24, paddingTop: 20, borderTop: '1px solid var(--frost-deep)', textAlign: 'left', fontSize: 13, lineHeight: 1.6 }}>
        <h3 className="h3" style={{ margin: '0 0 8px' }}>Open-source models</h3>
        <p style={{ margin: '0 0 12px', color: 'var(--iron)' }}>
          Daisy uses these on-device models for transcription and speaker labeling. All
          audio stays on your machine when you use them.
        </p>
        <ul style={{ paddingLeft: 20, margin: 0 }}>
          <li style={{ marginBottom: 8 }}>
            <strong>Whisper (GGML)</strong> — OpenAI Whisper · MIT.<br />
            Downloaded from{' '}
            <a
              href="https://huggingface.co/ggerganov/whisper.cpp"
              target="_blank"
              rel="noreferrer"
              style={{ color: 'var(--indigo-deep)' }}
            >
              huggingface.co/ggerganov/whisper.cpp
            </a>
          </li>
          <li style={{ marginBottom: 8 }}>
            <strong>WeSpeaker ResNet34 (VoxCeleb)</strong> — Apache-2.0.<br />
            Source:{' '}
            <a
              href="https://huggingface.co/hbredin/wespeaker-voxceleb-resnet34-LM"
              target="_blank"
              rel="noreferrer"
              style={{ color: 'var(--indigo-deep)' }}
            >
              huggingface.co/hbredin/wespeaker-voxceleb-resnet34-LM
            </a>
          </li>
        </ul>
      </div>

      <OpenSourceLicenses />
    </section>
  );
}

function OpenSourceLicenses() {
  const [open, setOpen] = useState(false);
  return (
    <div style={{ marginTop: 24, paddingTop: 16, borderTop: '1px solid var(--frost-deep)' }}>
      <button
        onClick={() => setOpen((v) => !v)}
        className="btn"
        style={{ width: '100%', justifyContent: 'space-between' }}
        aria-expanded={open}
      >
        <span>
          Open-source licenses · {RUST_LICENSES.reduce((n, l) => n + l.usedBy.length, 0)} Rust
          {' '}crates · {FRONTEND_PACKAGES.length} npm packages
        </span>
        <span>{open ? '▴' : '▾'}</span>
      </button>
      {open && (
        <div style={{ marginTop: 14, textAlign: 'left', fontSize: 12, lineHeight: 1.5 }}>
          <p className="meta" style={{ fontSize: 12 }}>
            Daisy is built on these open-source dependencies. Full license texts and the
            complete per-package list ship with Daisy in <code>THIRD-PARTY-LICENSES.txt</code>{' '}
            and are mirrored at{' '}
            <a href="#" onClick={(e) => { e.preventDefault(); void tauri.openExternal('https://www.daisylocal.app/licenses'); }}>
              daisylocal.app/licenses
            </a>.
          </p>
        </div>
      )}
    </div>
  );
}

// ============================================================================
// Shared bits
// ============================================================================
function Row({ label, value }: { label: string; value: string }) {
  return (
    <tr>
      <td style={{ padding: '6px 12px 6px 0', color: 'var(--muted)', verticalAlign: 'top', whiteSpace: 'nowrap' }}>{label}</td>
      <td style={{ padding: '6px 0', wordBreak: 'break-all' }}><code>{value}</code></td>
    </tr>
  );
}

function Field({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div style={{ marginTop: 16 }}>
      <label style={{ display: 'block', fontSize: 13, color: 'var(--muted)', marginBottom: 4 }}>{label}</label>
      {children}
    </div>
  );
}



type TagSelection = { kind: 'new' } | { kind: 'tag'; id: string };

type DeleteConfirmStage = null | { stage: 'initial' } | { stage: 'detach'; id: string };

const DAISY_PROMPT_ID = 'builtin:daisy';
// Mirror of summarize::prompts::DAISY_DIRECTIVE_MIN_CHARS (backend enforces).
const DAISY_DIRECTIVE_MIN_CHARS = 40;

/** Sample output per built-in, shown under the editor. Fictional data only. */
const BUILTIN_EXAMPLES: Record<string, string> = {
  'builtin:daisy': `**TL;DR.** You agreed with Mira to ship the analytics pilot on May 12 with the trimmed feature list, retire the legacy exporter once the pilot lands, and revisit pricing after three pilot customers are live.

## Action items
- Send Mira the pilot checklist — _you_ (due Fri)
- Book the security review slot with Priya — _Mira_ (due May 2)
- Draft the exporter deprecation notice for customers — _you_

## Decisions
- Pilot ships May 12 with the trimmed feature list
- Legacy exporter is retired after the pilot

## Open questions
- Does the Fabrikam contract require the exporter through Q3?

## Key topics
- Pilot scope and schedule
- Exporter retirement
- Pricing follow-up`,
  'builtin:zoom': `**Summary.** You and Mira aligned on the analytics pilot scope, set a May 12 ship date, and agreed the legacy exporter retires once the pilot lands. Pricing questions were parked until three pilot customers are live.

## Pilot scope
Mira walked through the trimmed feature list and flagged that dashboards and CSV import are the only must-haves for the pilot cohort. You both agreed the exporter stays out of scope, and that anything cut now gets reconsidered only after pilot feedback.

## Exporter retirement
You raised the Fabrikam contract as the one blocker to killing the exporter. Mira will confirm whether their renewal language still references it; if not, the deprecation notice goes out with the pilot announcement.

## Action items
- Send Mira the pilot checklist — _you_ (due Fri)
- Confirm Fabrikam contract language — _Mira_ (due May 2)`,
  'builtin:otter': `**Chapter summary.** The meeting opened with Mira presenting the trimmed pilot scope, which you approved after confirming dashboards and CSV import stay in. Discussion then moved to the legacy exporter, where the Fabrikam contract surfaced as the only blocker to retiring it. The final stretch covered timing: May 12 was set as the ship date, and pricing was deliberately parked until three pilot customers are live.

## Pilot scope
- Trimmed feature list approved
- Dashboards + CSV import are the must-haves
- Legacy exporter out of scope

## Exporter retirement
- Fabrikam contract may still reference the exporter — needs checking
- Deprecation notice ships with the pilot announcement if clear

## Timing
- Pilot ships May 12
- Pricing revisited after three pilot customers are live

## Action items
- Send Mira the pilot checklist — _you_ (due Fri)
- Confirm Fabrikam contract language — _Mira_ (due May 2)`,
  'builtin:pm': `**Summary.** You drove the meeting to a clear ship date and named the schedule risk early, but two commitments left the room without owners and one blocker was talked around rather than named.

## Strengths
- Named the schedule risk directly and proposed a mitigation ("we slip a week unless QA starts Monday")
- Closed the loop on last week's exporter question before opening new business
- Drove the ship-date decision with two concrete options instead of an open-ended discussion

## Improvements
- "Someone should draft the checklist" left the task unowned — assign a name and a date in the moment
- The staging-environment outage came up twice without being named as a blocker — call it what it is and ask for help explicitly`,
  'builtin:consultant': `**Summary.** Clear recommendation-first framing on the pilot scope; the middle third of the meeting drifted because the cost discussion mixed overlapping categories and one opinion was stated as fact.

## Strengths
- Led with the recommendation ("ship May 12 with the trimmed list"), then gave three supporting reasons
- Pulled a wandering discussion back into two named buckets ("this is either a scope question or a timing question")
- Named the question before answering it when Mira asked about pricing

## Improvements
- "Cost" and "budget risk" overlapped — collapse into one MECE category before presenting
- "Customers won't miss the exporter" was stated as fact without an anchor — cite the usage data that supports it`,
  'builtin:teamlead': `**Summary.** Strong coaching posture through the first half — open questions before prescriptions, credit given by name — but one delegation went out without success criteria and a disagreement about QA timing was left hanging.

## Strengths
- Asked "what have you tried?" before offering a fix on the import bug
- Credited Mira by name for the migration script in front of the group
- Invited dissent on the ship date ("who thinks May 12 is wrong?") before locking it

## Improvements
- "You own the pilot" had no definition of done — say what success looks like and by when
- Devon's pushback on QA timing got a nod but no resolution — close the disagreement explicitly, even if the answer is "we'll decide Friday"`,
};

export function PromptsSection({
  settings,
  update,
}: {
  settings: SettingsT;
  update: <K extends keyof SettingsT>(key: K, value: SettingsT[K]) => void;
}) {
  const [prompts, setPrompts] = useState<Prompt[]>([]);
  const [selId, setSelId] = useState<string>('');
  const [name, setName] = useState('');
  const [directive, setDirective] = useState('');
  const [savingAs, setSavingAs] = useState(false);
  const [newName, setNewName] = useState('');
  const [saveErr, setSaveErr] = useState<string | null>(null);

  const defaultId = settings.default_summary_prompt_id ?? DAISY_PROMPT_ID;
  const sel = prompts.find((p) => p.id === selId) ?? null;
  const dirty = sel != null && (directive !== sel.directive_md || (!sel.builtin && name !== sel.name));

  const reload = async (keep?: string) => {
    const list = await tauri.listPrompts();
    setPrompts(list);
    const id = keep ?? selId;
    const cur = list.find((p) => p.id === id) ?? list[0];
    if (cur) {
      setSelId(cur.id);
      setName(cur.name);
      setDirective(cur.directive_md);
    }
  };
  useEffect(() => {
    void reload();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const switchTo = async (id: string) => {
    if (dirty) {
      const ok = await confirm({
        title: 'Discard unsaved changes?',
        body: 'This prompt has unsaved edits.',
        confirmLabel: 'Discard',
        danger: true,
      });
      if (!ok) return;
    }
    const p = prompts.find((x) => x.id === id);
    if (!p) return;
    setSelId(p.id);
    setName(p.name);
    setDirective(p.directive_md);
    setSavingAs(false);
    setSaveErr(null);
  };

  // The Daisy Summarizer's shipped directive is hardcoded in the app; edits
  // here are a file override, gated on a minimum length (short/garbled
  // overrides are ignored and the built-in text wins).
  const editable = sel != null && (!sel.builtin || sel.id === DAISY_PROMPT_ID);

  const save = async () => {
    if (!sel || !editable) return;
    if (sel.id === DAISY_PROMPT_ID && directive.trim().length < DAISY_DIRECTIVE_MIN_CHARS) {
      setSaveErr(`The Summarizer prompt must be at least ${DAISY_DIRECTIVE_MIN_CHARS} characters — it drives every summary.`);
      return;
    }
    setSaveErr(null);
    try {
      await tauri.savePrompt({ id: sel.id, name: name.trim(), directive_md: directive.trim(), output: sel.output });
    } catch (e) {
      setSaveErr(errStr(e));
      return;
    }
    await reload(sel.id);
  };

  const resetToDefault = async () => {
    if (!sel || sel.id !== DAISY_PROMPT_ID) return;
    const ok = await confirm({
      title: 'Reset to default?',
      body: 'Your edits will be replaced with the built-in Summarizer prompt.',
      confirmLabel: 'Reset',
      danger: true,
    });
    if (!ok) return;
    setSaveErr(null);
    await tauri.resetPrompt(sel.id);
    await reload(sel.id);
  };

  const saveAsNew = async () => {
    if (!sel || !newName.trim()) return;
    const created = await tauri.savePrompt({
      id: null,
      name: newName.trim(),
      directive_md: directive.trim(),
      output: sel.output,
    });
    setSavingAs(false);
    setNewName('');
    await reload(created.id);
  };

  const remove = async () => {
    if (!sel || sel.builtin) return;
    const ok = await confirm({
      title: 'Delete this prompt?',
      body: `"${sel.name}" will be removed.`,
      confirmLabel: 'Delete',
      danger: true,
    });
    if (!ok) return;
    const wasDefault = defaultId === sel.id;
    await tauri.deletePrompt(sel.id);
    if (wasDefault) {
      await tauri.setDefaultSummaryPrompt(DAISY_PROMPT_ID);
      update('default_summary_prompt_id', DAISY_PROMPT_ID);
    }
    await reload(DAISY_PROMPT_ID);
  };

  const makeDefault = async () => {
    if (!sel) return;
    await tauri.setDefaultSummaryPrompt(sel.id);
    update('default_summary_prompt_id', sel.id);
  };

  return (
    <section>
      <h2 className="h2">Prompts</h2>
      <p className="meta">
        Prompts control what Daisy writes about a meeting. The default summary prompt runs
        automatically after each recording; any prompt can be run on demand from the Analyzer.
      </p>
      <label className="meta" style={{ display: 'block', fontSize: 12, marginBottom: 6 }}>Prompt</label>
      <select value={selId} onChange={(e) => void switchTo(e.target.value)} style={{ maxWidth: 420 }}>
        {prompts.map((p) => (
          <option key={p.id} value={p.id}>
            {p.name}{p.id === defaultId ? ' · default' : ''}
          </option>
        ))}
      </select>
      {sel && (
        <div style={{ marginTop: 12, maxWidth: 640 }}>
          <label style={{ display: 'block', marginBottom: 8 }}>
            Name
            <input
              style={{ display: 'block', width: '100%' }}
              value={name}
              disabled={sel.builtin}
              onChange={(e) => setName(e.target.value)}
            />
          </label>
          <label style={{ display: 'block', marginBottom: 8 }}>
            Describe what this analysis should produce
            <textarea
              style={{ display: 'block', width: '100%' }}
              value={directive}
              rows={10}
              onChange={(e) => setDirective(e.target.value)}
            />
          </label>
          <FieldError style={{ margin: '0 0 8px' }}>{saveErr}</FieldError>
          <div style={{ display: 'flex', gap: 8, flexWrap: 'wrap' }}>
            {editable && (
              <button className="btn btn--primary" onClick={() => void save()} disabled={!dirty}>Save</button>
            )}
            {sel.id === DAISY_PROMPT_ID && (
              <button className="btn" onClick={() => void resetToDefault()}>Reset to default</button>
            )}
            <button className="btn" onClick={() => { setSavingAs(true); setNewName(''); }}>Save as…</button>
            {!sel.builtin && (
              <button className="btn btn--danger" onClick={() => void remove()}>Delete</button>
            )}
            {sel.id !== defaultId && (
              <button className="btn" onClick={() => void makeDefault()}>Default for summaries</button>
            )}
          </div>
          {savingAs && (
            <div style={{ marginTop: 12 }}>
              <input
                autoFocus
                placeholder="New prompt name"
                value={newName}
                onChange={(e) => setNewName(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === 'Enter') void saveAsNew();
                }}
              />
              <div style={{ display: 'flex', gap: 8, marginTop: 8 }}>
                <button className="btn btn--primary" onClick={() => void saveAsNew()} disabled={!newName.trim()}>Save</button>
                <button className="btn" onClick={() => setSavingAs(false)}>Cancel</button>
              </div>
            </div>
          )}
          {sel.builtin && BUILTIN_EXAMPLES[sel.id] && (
            <div style={{ marginTop: 16 }}>
              <label className="meta" style={{ display: 'block', fontSize: 12, marginBottom: 6 }}>Example output</label>
              <div style={{ border: '1px solid var(--frost-deep)', borderRadius: 6, padding: 12 }}>
                <MarkdownView markdown={BUILTIN_EXAMPLES[sel.id]} />
              </div>
            </div>
          )}
        </div>
      )}
    </section>
  );
}

function TagsSection() {
  const [tags, setTags] = useState<Tag[]>([]);
  const [sel, setSel] = useState<TagSelection>({ kind: 'new' });
  const [name, setName] = useState('');
  const [color, setColor] = useState('#F2A900');
  const [promptText, setPromptText] = useState('');
  const [vocabText, setVocabText] = useState('');
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const [confirm, setConfirm] = useState<DeleteConfirmStage>(null);
  // Saved-confirmation banner; stays up for ~3s.
  const [savedFlash, setSavedFlash] = useState<string | null>(null);
  useEffect(() => {
    if (!savedFlash) return;
    const id = window.setTimeout(() => setSavedFlash(null), 3000);
    return () => window.clearTimeout(id);
  }, [savedFlash]);

  async function reload(selectId?: string) {
    try {
      const list = await tauri.listTags();
      setTags(list);
      if (selectId) {
        const found = list.find((t) => t.id === selectId);
        if (found) seedFrom(found);
      }
    } catch (e: unknown) {
      setErr(String((e as { message?: unknown })?.message ?? e));
    }
  }

  useEffect(() => { reload(); }, []);

  function seedFrom(t: Tag) {
    setSel({ kind: 'tag', id: t.id });
    setName(t.name);
    setColor(t.color_hex);
    setPromptText(t.prompt_md ?? '');
    setVocabText(t.vocab_md ?? '');
    setErr(null);
  }

  function seedNew() {
    setSel({ kind: 'new' });
    setName('');
    setColor('#F2A900');
    setPromptText('');
    setVocabText('');
    setErr(null);
  }

  const currentTag = sel.kind === 'tag' ? tags.find((t) => t.id === sel.id) ?? null : null;

  async function save() {
    setBusy(true);
    setErr(null);
    const wasNew = sel.kind === 'new';
    try {
      if (sel.kind === 'tag') {
        await tauri.updateTag({ id: sel.id, name: name.trim(), color_hex: color, prompt_md: promptText.trim(), vocab_md: vocabText.trim() });
        await reload(sel.id);
      } else {
        const created = await tauri.createTag({ name: name.trim(), color_hex: color, prompt_md: promptText.trim(), vocab_md: vocabText.trim() });
        const list = await tauri.listTags();
        setTags(list);
        seedFrom(created);
      }
      setSavedFlash(wasNew ? `Tag "${name.trim()}" created.` : `Tag "${name.trim()}" saved.`);
    } catch (e: unknown) {
      setErr(String((e as { message?: unknown })?.message ?? e));
    } finally {
      setBusy(false);
    }
  }

  function cancel() {
    if (currentTag) seedFrom(currentTag);
    else seedNew();
  }

  function openDeleteConfirm() {
    if (sel.kind !== 'tag') return;
    setErr(null);
    setConfirm({ stage: 'initial' });
  }

  async function performDelete(force: boolean): Promise<void> {
    if (sel.kind !== 'tag') return;
    const id = sel.id;
    try {
      await tauri.deleteTag(id, force);
      setConfirm(null);
      seedNew();
      await reload();
    } catch (e: unknown) {
      const msg = String((e as { message?: unknown })?.message ?? e);
      if (!force && /session/i.test(msg)) {
        setConfirm({ stage: 'detach', id });
        return;
      }
      throw e;
    }
  }

  const canSave = name.trim() !== '' && !busy;

  return (
    <section>
      <h2 className="h2">Tags</h2>
      <div style={{ display: 'flex', gap: 24, alignItems: 'flex-start' }}>
        <div style={{ width: 220, borderRight: '1px solid var(--frost-deep)', paddingRight: 16 }}>
          {tags.map((t) => {
            const active = sel.kind === 'tag' && sel.id === t.id;
            return (
              <button
                key={t.id}
                className="option-row"
                onClick={() => seedFrom(t)}
                style={{
                  display: 'block', width: '100%', textAlign: 'left', padding: '6px 8px', marginBottom: 4,
                  border: 'none',
                  borderRadius: 6, background: active ? 'var(--tint)' : 'transparent', cursor: 'pointer',
                  color: active ? 'var(--graphite)' : 'inherit', fontWeight: active ? 600 : 400,
                }}
              >
                <span style={{ display: 'flex', alignItems: 'center', gap: 8, fontSize: 14 }}>
                  <span style={{ display: 'inline-block', width: 14, height: 14, borderRadius: 3, background: t.color_hex, flexShrink: 0 }} />
                  <span>{t.name}</span>
                </span>
                <span className="meta" style={{ fontSize: 11, marginLeft: 22 }}>
                  {t.use_count} session{t.use_count === 1 ? '' : 's'} · {t.prompt_md ? 'prompt' : 'no prompt'}
                </span>
              </button>
            );
          })}
          <button
            className="option-row"
            onClick={seedNew}
            style={{
              display: 'block', width: '100%', textAlign: 'left', padding: '6px 8px', marginTop: 8,
              border: '1px dashed var(--frost-deep)', borderRadius: 6,
              background: sel.kind === 'new' ? 'var(--tint)' : 'transparent',
              color: sel.kind === 'new' ? 'var(--graphite)' : 'inherit',
              fontWeight: sel.kind === 'new' ? 600 : 400, fontSize: 13, cursor: 'pointer',
            }}
          >
            + New tag
          </button>
        </div>

        <div style={{ flex: 1, minWidth: 0 }}>
          <Field label="Display name">
            <input type="text" value={name} onChange={(e) => setName(e.target.value)} style={{ display: 'block', width: 320 }} placeholder="e.g. Weekly stand-up" />
          </Field>

          <Field label="Color">
            <TagColorPicker value={color} onChange={setColor} />
          </Field>

          <Field label="Preview">
            <TagChip tag={{ name: name || 'tag', color_hex: color }} />
          </Field>

          <Field label="Prompt (summary + in-call chat, optional)">
            <textarea
              className="summary-edit"
              style={{ minHeight: 140, width: '100%', maxWidth: 480 }}
              value={promptText}
              onChange={(e) => setPromptText(e.target.value)}
              placeholder={'e.g.\nLead with decisions; list action items with owners.\nUse plain English; avoid jargon and acronyms.'}
            />
          </Field>

          <Field label="Vocabulary (names & terms, optional)">
            <textarea
              className="summary-edit"
              style={{ minHeight: 100, width: '100%', maxWidth: 480 }}
              value={vocabText}
              onChange={(e) => setVocabText(e.target.value)}
              placeholder={'Northwind Logistics, NWL, Priya Okonkwo, Project Aurora'}
            />
            <p className="meta" style={{ fontSize: 11, color: 'var(--iron)', marginTop: 4 }}>
              Sent to the transcriber so it spells these correctly. Comma or line-separated.
            </p>
          </Field>

          {err && <p className="meta" style={{ color: 'var(--danger)', fontSize: 12 }}>{err}</p>}
          {savedFlash && (
            <p
              role="status"
              style={{
                fontSize: 13, color: 'var(--indigo-deep)', fontWeight: 600,
                margin: '8px 0 0', padding: '6px 10px',
                background: 'var(--cream-pure)',
                border: '1px solid var(--frost-deep)', borderLeft: '3px solid var(--indigo-deep)',
                borderRadius: 4,
              }}
            >
              ✓ {savedFlash}
            </p>
          )}

          <div style={{ display: 'flex', gap: 10, marginTop: 12, flexWrap: 'wrap' }}>
            <button className="btn btn--primary" disabled={!canSave} onClick={save}>{busy ? 'Saving…' : 'Save'}</button>
            <button className="btn" disabled={busy} onClick={cancel}>Cancel</button>
            {sel.kind === 'tag' && (
              <button className="btn btn--danger" disabled={busy} onClick={openDeleteConfirm}>Delete tag</button>
            )}
          </div>
        </div>
      </div>

      {confirm?.stage === 'initial' && currentTag && (
        <ConfirmDialog
          title={`Delete tag "${currentTag.name}"?`}
          body={
            <p style={{ margin: 0 }}>
              This removes the tag from your library. It can&apos;t be undone.
              {currentTag.use_count > 0 && (
                <>
                  {' '}
                  <strong>
                    {currentTag.use_count} session{currentTag.use_count === 1 ? '' : 's'}
                  </strong>{' '}
                  reference{currentTag.use_count === 1 ? 's' : ''} this tag and will need to be detached first.
                </>
              )}
            </p>
          }
          confirmLabel="Delete tag"
          danger
          onCancel={() => setConfirm(null)}
          onConfirm={() => performDelete(false)}
        />
      )}

      {confirm?.stage === 'detach' && currentTag && (
        <ConfirmDialog
          title="Detach and delete?"
          body={
            <p style={{ margin: 0 }}>
              <strong>{currentTag.use_count}</strong> session
              {currentTag.use_count === 1 ? '' : 's'} still reference this tag.
              Detach the tag from those sessions and delete it anyway?
            </p>
          }
          confirmLabel="Detach & delete"
          danger
          onCancel={() => setConfirm(null)}
          onConfirm={() => performDelete(true)}
        />
      )}
    </section>
  );
}
