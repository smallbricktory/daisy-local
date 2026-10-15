// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { releaseNotesUrl } from '../lib/releaseNotes';
import { useEffect, useRef, useState, useSyncExternalStore } from 'react';
import { PRICING_URL } from '../lib/trial-banner';
import { snoozeHelp } from '../lib/meeting-prompt';
import { ConfirmDialog } from '../components/ConfirmDialog';
import { RowMenu } from '../components/RowMenu';
import { PageHeader } from '../components/PageHeader';
import { alert, confirm } from '../lib/confirm';
import { useBuiltinModel } from '../builtinModel';
import { pushToast } from '../lib/toastStore';
import {
  machineStore, setMachine, setSettings, settingsStore, useMachineName, useMachineSettings, useSettings,
} from '../lib/settingsStore';
import { showGatewayNotice, isGatewayNotEntitled } from '../lib/gatewayNotice';
import { Field } from '../components/Field';
import { Group, SettingRow } from '../components/SettingsGroup';
import { Select } from '../components/Select';
import { Toggle } from '../components/Toggle';
import { MobileSyncSettings } from '../components/MobileSyncSettings';
import { FieldError, FieldOk } from '../components/FieldError';
import { MarkdownView } from '../components/MarkdownView';
import { LEGAL_LAST_UPDATED, TOS_MARKDOWN, PRIVACY_MARKDOWN } from '../legalContent';
import { useAudioSourcesChanged } from '../lib/useAudioSourcesChanged';
import { invalidateTags, useTags } from '../lib/useTags';
import {
  subscribeRecJob,
  getRecJobState,
  startDeleteAll,
} from '../recordingsJob';
import {
  tauri,
  aiProvider,
  errStr,
  formatBytes,
  providerLabel,
  type AecModeOverride,
  type AudioSourceInfo,
  type BootstrapStatus,
  type CalendarSubscription,
  type IntegrationPublic,
  type LicenseStatus,
  type ProviderId,
  type ProviderListEntry,
  type RecordingsStats,
  type Settings as SettingsT,
  type SpeechLevelInfo,
  type Prompt,
  type Tag,
  type UpsertIntegration,
  type WebhookAuthInput,
  type MachinePatch,
  type MachineSettings,
  type SettingsPatch,
  type AsrSettings,
} from '../tauri';
import { MicLevel } from '../components/MicLevel';
import { HotkeyField } from '../components/HotkeyField';
import { applyTheme } from '../lib/themeLoader';
import { setAppearance } from '../lib/appearance';
import { McpSettings } from '../components/McpSettings';
import { AsrModels } from '../components/AsrModels';
import { LocalModel } from '../components/LocalModel';
import { LicenseLink } from '../components/LicenseLink';
import { ProviderEditor } from '../components/ProviderEditor';
import { DEFAULT_TAG_COLOR, TagColorPicker } from '../components/tags/TagColorPicker';
import { PROVIDER_DEFAULTS, buildProviderRows, isProviderConfigured } from '../lib/providerRows';
import { sectionFallback, setNerdMode, useNerdMode, visibleSections, SETTINGS_SECTIONS } from '../lib/nerdMode';
import { getZoom, setZoom, useZoom } from '../lib/uiZoomController';
import { isMacPlatform, stepZoom, ZOOM_MIN, ZOOM_MAX, ZOOM_DEFAULT } from '../lib/uiZoom';
import { RUST_LICENSES, FRONTEND_PACKAGES, MODEL_LICENSES } from '../licenses.generated';
import type { VoiceprintView, MeetingDetectionSupport, MeetingPrompt } from '../tauri';

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

const SECTIONS = SETTINGS_SECTIONS;

// Selects an AI provider; a failure reaches `onError` and leaves the
// selection unchanged.
async function selectAiProvider(
  value: ProviderId | null,
  apply: (p: ProviderId | null) => void,
  onError: (message: string | null) => void,
): Promise<void> {
  onError(null);
  if (value === 'daisy_gateway') {
    try {
      await tauri.registerGateway();
    } catch (e: unknown) {
      if (isGatewayNotEntitled(e)) showGatewayNotice();
      else onError(errStr(e));
      return;
    }
  }
  apply(value);
}

/** A setting that did not save shows its stored value again; this says why. */
function saveFailed(e: unknown): void {
  pushToast({
    id: 'settings-save',
    severity: 'error',
    title: "That setting didn't save",
    body: errStr(e),
    autoDismissMs: 8000,
  });
}

/** Stores fields of this machine's block from a control that has no error
 *  line of its own. */
function saveMachine(patch: MachinePatch): void {
  void setMachine(patch).catch(saveFailed);
}

/** Stores fields of the profile's settings from a control that has no error
 *  line of its own. */
function saveSettings(patch: SettingsPatch): void {
  void setSettings(patch).catch(saveFailed);
}

// Settings → Display: text size + theme, both properties of this machine's
// screen. Theme files live in <profile>/css (synced); the SELECTION is
// per-machine.
function DisplaySection({ machine, machineName }: {
  machine: MachineSettings;
  machineName: string;
}) {
  const nerd = useNerdMode();
  const trayLabel = isMacPlatform() ? 'Show Daisy in the menu bar' : 'Show Daisy in the system tray';
  const [themes, setThemes] = useState<string[] | null>(null);
  const [themeErr, setThemeErr] = useState<string | null>(null);
  useEffect(() => {
    tauri.listCssThemes().then(setThemes)
      .catch((e) => setThemeErr(errStr(e)));
  }, []);

  async function pick(name: string | null) {
    setThemeErr(await applyTheme(name));
    saveMachine({ css_theme: name });
  }

  return (
    <section>
      <Group title="Appearance" scope={machineName}>
        <SettingRow label="Text size">
          <TextSizeControls />
        </SettingRow>

        <SettingRow stack label="Theme">
          <div className="theme-cards">
            {(['light', 'dark', 'system'] as const).map((mode) => (
              <button
                key={mode}
                data-testid={`display-theme-${mode}`}
                type="button"
                className="theme-card"
                aria-pressed={(machine.appearance ?? 'system') === mode}
                onClick={() => { void setAppearance(mode).catch(saveFailed); }}
              >
                <span className={`theme-card__prev theme-card__prev--${mode}`}>
                  <i /><i /><i />
                </span>
                <span className="theme-card__name">
                  {mode === 'light' ? 'Light' : mode === 'dark' ? 'Dark' : 'Match system'}
                </span>
              </button>
            ))}
          </div>
        </SettingRow>

        {nerd && (
          <SettingRow
            label="Theme file"
            help={<>Optional CSS from the profile&apos;s <code>css/</code> folder, loaded on
              top of the theme above. Edit <code>default.css</code> for a commented reference.</>}
          >
            <div>
              <Select
                testId="display-css-theme"
                label="Theme file"
                value={machine.css_theme ?? ''}
                disabled={themes == null}
                onChange={(v) => void pick(v || null)}
                options={[
                  { value: '', label: 'None' },
                  ...(themes ?? []).map((t) => ({ value: t, label: t })),
                ]}
              />
              {themeErr && (
                <p className="set-row__help meta--danger">{themeErr} — using the built-in look.</p>
              )}
            </div>
          </SettingRow>
        )}
      </Group>

      <Group title="Advanced" scope={machineName}>
        <SettingRow
          label="Nerd Mode"
          help="Shows every screen and setting. Off hides the ones most people never need."
        >
          <Toggle
            checked={nerd}
            label="Nerd Mode"
            testId="toggle-nerd-mode"
            onChange={(v) => { void setNerdMode(v).catch(saveFailed); }}
          />
        </SettingRow>
        {nerd && (
          <SettingRow label={trayLabel}>
            <Toggle
              checked={machine.show_tray_icon ?? true}
              label={trayLabel}
              testId="toggle-show-tray-icon"
              onChange={(v) => saveMachine({ show_tray_icon: v })}
            />
          </SettingRow>
        )}
        {nerd && (
          <SettingRow label="Minimize on close?">
            <Toggle
              checked={machine.minimize_on_close === true}
              label="Minimize on close?"
              testId="toggle-minimize-on-close"
              onChange={(v) => saveMachine({ minimize_on_close: v })}
            />
          </SettingRow>
        )}
      </Group>
    </section>
  );
}

// Whole-UI zoom control. Reads/writes this machine's zoom (also driven by
// the Cmd/Ctrl +/-/0 shortcuts); the percentage stays live regardless of
// source.
function TextSizeControls() {
  const zoom = useZoom();
  const pct = Math.round(zoom * 100);
  return (
    <div className="zoom-control">
      <div className="stepper">
        <button
          data-testid="display-zoom-out"
          className="stepper__btn" aria-label="Decrease text size"
          disabled={zoom <= ZOOM_MIN} onClick={() => setZoom(stepZoom(getZoom(), 'out'))}
        >A−</button>
        <span className="stepper__value" aria-live="polite">{pct}%</span>
        <button
          data-testid="display-zoom-in"
          className="stepper__btn" aria-label="Increase text size"
          disabled={zoom >= ZOOM_MAX} onClick={() => setZoom(stepZoom(getZoom(), 'in'))}
        >A+</button>
      </div>
      <button
        data-testid="display-zoom-reset"
        className="btn-link" disabled={zoom === ZOOM_DEFAULT}
        onClick={() => setZoom(ZOOM_DEFAULT)}
      >Reset</button>
    </div>
  );
}

type SectionName = (typeof SECTIONS)[number];

export function Settings({ onLocked, onLaunchWizard, onCalendarsChanged, initialSection, onLicenseChanged }: Props) {
  const nerd = useNerdMode();
  const [section, setSection] = useState<SectionName>(
    visibleSections(nerd).includes(initialSection as SectionName) ? (initialSection as SectionName) : 'Recordings',
  );
  // Applies a deep-linked section while Settings is already open. Each
  // initialSection value is applied once; a tab the user clicked stays put.
  const appliedRef = useRef<string | undefined>(initialSection);
  useEffect(() => {
    if (!initialSection || initialSection === appliedRef.current) return;
    if (!visibleSections(nerd).includes(initialSection as SectionName)) return;
    appliedRef.current = initialSection;
    setSection(initialSection as SectionName);
  }, [initialSection, nerd]);
  // A section the switch just hid cannot stay open.
  useEffect(() => {
    const to = sectionFallback(nerd, section);
    if (to) setSection(to);
  }, [nerd, section]);
  const settings = useSettings();
  // This machine's settings block.
  const machine = useMachineSettings();
  const machineName = useMachineName();
  const [bs, setBs] = useState<BootstrapStatus | null>(null);
  const [sources, setSources] = useState<AudioSourceInfo[]>([]);
  const [providers, setProviders] = useState<ProviderListEntry[]>([]);
  const builtin = useBuiltinModel();
  const [error, setError] = useState<string | null>(null);
  const [moving, setMoving] = useState(false);
  // Hotplug changes the source list; the renumbered mic view arrives with
  // the machine block.
  useAudioSourcesChanged(() => {
    tauri.listAudioSources().then(setSources).catch(() => {});
  });

  // Settings, machine block and bootstrap status carry the page; audio
  // sources and providers fill in as each one answers, and the first failure
  // is shown.
  async function refresh() {
    setError(null);
    const shown = (e: unknown) => setError((prev) => prev ?? errStr(e));
    await Promise.allSettled([
      settingsStore.refresh().catch(shown),
      machineStore.refresh().catch(shown),
      tauri.bootstrapStatus().then(setBs, shown),
      tauri.listAudioSources().then(setSources, shown),
      tauri.listProviders().then(setProviders, shown),
    ]);
  }

  useEffect(() => { refresh(); }, []);

  // Scrolls the page to the top when a section is opened or re-picked.
  const pageRef = useRef<HTMLDivElement>(null);
  const toTop = () => {
    pageRef.current?.scrollTo({ top: 0, left: 0, behavior: 'instant' as ScrollBehavior });
  };
  useEffect(toTop, [section]);

  // The choice arrives with the machine block the backend announces.
  async function setAiProvider(provider: ProviderId | null) {
    try {
      await tauri.setAiProvider(provider);
    } catch (e: unknown) {
      setError(errStr(e));
    }
  }

  async function lock() {
    await tauri.lockVault();
    onLocked();
  }

  async function handleMoveProfile() {
    const picked = await tauri.pickDirectory('profile-move');
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
      setError(errStr(e));
      setMoving(false);
    }
  }

  async function handleSwitchProfile() {
    const picked = await tauri.pickDirectory('profile-switch');
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
      setError(errStr(e));
    }
  }

  // Merges known providers with the vault's providers; providers without a
  // key still get a row (see lib/providerRows).
  const providerRows: ProviderListEntry[] = buildProviderRows(providers);

  if (!settings || !machine || !bs) {
    return error
      ? <p className="meta meta--danger" data-testid="settings-error" style={{ padding: 32 }}>{error}</p>
      : <p className="meta">Loading…</p>;
  }

  return (
    <div className="page" ref={pageRef}>
      <PageHeader
        title={nerd
          ? <>Settings <span data-testid="settings-nerd-badge" className="page-header__badge">Nerd Mode</span></>
          : 'Settings'}
      />
      <div className="page__body" style={{ display: 'flex', gap: 28, alignItems: 'flex-start' }}>
      {/* Pins under the page header, which is sticky in the same scroller. */}
      <nav style={{ flexShrink: 0, width: 150, position: 'sticky', top: 'var(--page-header-h)' }}>
        {visibleSections(nerd).map((s) => (
          <button
            key={s}
            data-testid={`settings-section-${s.toLowerCase()}`}
            className={section === s ? 'settings-tab is-on' : 'settings-tab'}
            onClick={() => (s === section ? toTop() : setSection(s))}
          >
            {s}
          </button>
        ))}
      </nav>

      <div style={{ flex: 1, minWidth: 0, maxWidth: 720 }}>
        {section === 'Recordings' && (
          <RecordingsSection
            settings={settings}
            machine={machine}
            machineName={machineName}
            sources={sources}
            providers={providers}
            localReady={builtin != null && builtin.model.installed && builtin.model.capability !== 'unsupported'}
            setAiProvider={setAiProvider}
          />
        )}
        {section === 'Mobile Sync' && <MobileSyncSettings />}
        {section === 'Tags' && <TagsSection />}
        {section === 'Prompts' && <PromptsSection settings={settings} />}
        {section === 'Providers' && (
          <ProvidersSection
            providerRows={providerRows}
            current={aiProvider(settings, machine)}
            enable={(p, onError) => selectAiProvider(p, (v) => void setAiProvider(v), onError)}
            onSaved={(name) => {
              if (aiProvider(settings, machine) == null) void setAiProvider(name);
              tauri.listProviders().then(setProviders).catch((e) => setError(errStr(e)));
            }}
            machine={machine}
            nerd={nerd}
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
            onUserDisplayNameChange={(v) => saveSettings({ user_display_name: v.trim() ? v.trim() : null })}
          />
        )}
        {section === 'Voiceprints' && (
          <VoiceprintsSection settings={settings} />
        )}
        {section === 'Calendars' && <CalendarsSection onChanged={onCalendarsChanged} />}
        {section === 'Display' && <DisplaySection machine={machine} machineName={machineName} />}
        {section === 'MCP' && <McpSettings settings={machine} />}
        {section === 'About' && <AboutSection onLicenseChanged={onLicenseChanged} settings={settings} />}

        {error && <p className="meta meta--danger" data-testid="settings-error" style={{ marginTop: 16 }}>{error}</p>}
      </div>
      </div>
    </div>
  );
}

// ============================================================================
// Recordings
// ============================================================================
function RecordingsSection({
  settings, machine, machineName, sources, providers, localReady, setAiProvider,
}: {
  settings: SettingsT;
  machine: MachineSettings;
  machineName: string;
  sources: AudioSourceInfo[];
  providers: ProviderListEntry[];
  localReady: boolean;
  setAiProvider: (provider: ProviderId | null) => Promise<void>;
}) {
  const nerd = useNerdMode();
  // Usable providers (key saved, or keyless with a model), plus the current selection.
  const summaryOptions: ProviderId[] = [
    ...(localReady ? (['local'] as ProviderId[]) : []),
    ...providers.filter(isProviderConfigured).map((p) => p.name),
  ];
  const current = aiProvider(settings, machine);
  if (current && !summaryOptions.includes(current)) summaryOptions.push(current);
  // Why a provider pick did not save.
  const [pickError, setPickError] = useState<string | null>(null);
  // The backend refuses a combination the OS already holds; the field says so.
  const [hotkeyError, setHotkeyError] = useState<string | null>(null);
  const [support, setSupport] = useState<MeetingDetectionSupport>({ supported: true, reason: null });
  useEffect(() => {
    tauri.meetingDetectionSupport().then(setSupport).catch(() => {});
  }, []);
  const prompt: MeetingPrompt = machine.meeting_prompt;
  const paused = snoozeHelp(prompt.snooze_until, Date.now());
  const setPrompt = (next: MeetingPrompt) => saveMachine({ meeting_prompt: next });

  return (
    <section>

      <Group title="Live speaker names" scope={machineName}>
        <SettingRow
          label="Live speaker names"
          help="Shows who is speaking above the live captions. Uses more memory and improves as you use Daisy more. Turn it off if your system feels slow."
        >
          <Toggle
            checked={machine.live_speaker_names}
            label="Live speaker names"
            testId="toggle-live-speaker-names"
            onChange={(v) => saveMachine({ live_speaker_names: v })}
          />
        </SettingRow>
      </Group>

      <Group
        title="Microphone"
        scope={machineName}
        hint="The mic Daisy listens to, and how loud you come through on it."
      >
        <SettingRow label="Microphone">
          <div style={{ display: 'flex', flexDirection: 'column', alignItems: 'flex-start', gap: 6 }}>
            <Select
              testId="rec-mic"
              label="Microphone"
              value={machine.default_mic_source_id?.toString() ?? ''}
              onChange={(v) => saveMachine({ default_mic_source_id: v ? Number(v) : null })}
              options={[
                { value: '', label: '— none selected —' },
                ...sources.filter((s) => s.kind === 'mic').map((s) => ({ value: String(s.id), label: s.description })),
              ]}
            />
            {machine.default_mic_source_id !== null && (
              // Level from the Tauri mic level events.
              <MicLevel tauriSourceId={machine.default_mic_source_id} width={260} height={8} />
            )}
          </div>
        </SettingRow>
        {nerd && (
          <SettingRow
            stack
            label="Your speaking level"
            help="Daisy learns this while you talk. Recalibrate if your voice is being missed or clipped."
          >
            <SpeechLevelBlock sources={sources} defaultMicId={machine.default_mic_source_id ?? null} />
          </SettingRow>
        )}
      </Group>

      <Group
        title="Start recording"
        scope={machineName}
        hint="How a recording begins from any app."
      >
        <SettingRow
          label="Keyboard shortcut"
          help="Starts a recording with your default microphone from anywhere."
        >
          <HotkeyField
            value={machine.record_hotkey ?? null}
            error={hotkeyError ?? undefined}
            onChange={async (v) => {
              setHotkeyError(null);
              try {
                await setMachine({ record_hotkey: v });
              } catch (e: unknown) {
                setHotkeyError(errStr(e));
              }
            }}
          />
        </SettingRow>
      </Group>

      {nerd && (
        <Group
          title="Meeting detection"
          scope={machineName}
          hint="A nudge when a call starts or ends."
        >
          <SettingRow
            label="Ask to record when a meeting starts"
            help={
              !support.supported ? support.reason
                : paused ? (
                  <span data-testid="meeting-prompt-help">
                    {paused}
                    {' · '}
                    <button
                      type="button"
                      className="btn-link"
                      data-testid="meeting-prompt-ask-again"
                      onClick={() => void setPrompt({ ...prompt, snooze_until: null })}
                    >
                      Ask again
                    </button>
                  </span>
                )
                : 'Daisy notices when another app opens your microphone.'
            }
          >
            <Toggle
              checked={prompt.enabled}
              label="Ask to record when a meeting starts"
              testId="toggle-meeting-prompt"
              disabled={!support.supported}
              onChange={(v) => void setPrompt({ ...prompt, enabled: v })}
            />
          </SettingRow>
          <SettingRow
            label="Offer to stop when the meeting ends"
            help={
              !support.supported ? support.reason
                : 'Daisy asks whether to stop when the call app releases your microphone and the call goes quiet.'
            }
          >
            <Toggle
              checked={prompt.stop_enabled}
              label="Offer to stop when the meeting ends"
              testId="toggle-meeting-stop-prompt"
              disabled={!support.supported}
              onChange={(v) => void setPrompt({ ...prompt, stop_enabled: v })}
            />
          </SettingRow>
        </Group>
      )}

      <Group title="Live captions" scope={machineName}>
        <SettingRow
          label="Show captions while recording"
          help="Display only; the transcript is always produced."
        >
          <Toggle
            checked={machine.show_live_captions}
            label="Show captions while recording"
            testId="toggle-show-captions-while-recording"
            onChange={(v) => saveMachine({ show_live_captions: v })}
          />
        </SettingRow>
      </Group>

      {nerd && (
        <Group
          title="Cleanup"
          scope={machineName}
          hint="Applied when a recording finishes. Your original audio is never changed."
        >
          <SettingRow
            label="Remove the other side's voice from your mic"
            help="Stops the far end bleeding into your track when you are not on headphones."
          >
            <Select
              testId="rec-aec"
              label="Remove the other side's voice from your mic"
              value={machine.aec_mode_override}
              onChange={(v) => saveMachine({ aec_mode_override: v as AecModeOverride })}
              options={[
                { value: 'auto', label: 'Automatic' },
                { value: 'always', label: 'Always' },
                { value: 'never', label: 'Never' },
              ]}
            />
          </SettingRow>
          <SettingRow
            label="Reduce background noise"
            help="Helps in noisy rooms. Slower to process, and only affects the saved audio."
          >
            <Toggle
              checked={machine.denoise_enabled}
              onChange={(v) => saveMachine({ denoise_enabled: v })}
              label="Reduce background noise"
              testId="toggle-reduce-background-noise"
            />
          </SettingRow>
        </Group>
      )}

      {nerd && (
        <Group
          title="AI features"
          scope={machineName}
          hint="Used for summaries, chapters, Q&A and transcript polish."
        >
          <SettingRow
            label="Provider"
            help={summaryOptions.length === 0
              ? `Download Daisy's built-in model or add a provider key under Settings → Providers to list it here. With "None", the Copy buttons on Transcript and Summary include a prompt you can paste into any chatbot yourself.`
              : 'With "None", the Copy buttons on Transcript and Summary include a prompt you can paste into any chatbot yourself.'}
          >
            <Select
              testId="rec-ai-provider"
              label="Provider"
              value={current ?? ''}
              onChange={(v) => void selectAiProvider(
                (v || null) as ProviderId | null,
                (p) => void setAiProvider(p),
                setPickError,
              )}
              options={[
                { value: '', label: 'None — copy and paste' },
                ...summaryOptions.map((p) => ({ value: p, label: providerLabel(p) })),
              ]}
            />
          </SettingRow>
          {pickError && (
            <p className="meta meta--danger" data-testid="rec-ai-provider-error">{pickError}</p>
          )}
        </Group>
      )}

      {nerd && (
        <Group title="Troubleshooting" scope={machineName}>
          <SettingRow
            label="Detailed logs"
            help="Only needed when chasing a problem. Takes effect after a restart."
          >
            <Select
              testId="rec-debug-level"
              label="Detailed logs"
              value={machine.debug_level ?? 'off'}
              onChange={(v) => saveMachine({ debug_level: v as 'off' | 'basic' | 'full' })}
              options={[
                { value: 'off', label: 'Off' },
                { value: 'basic', label: 'Basic' },
                { value: 'full', label: 'Full (adds decode + audio tracing)' },
              ]}
            />
          </SettingRow>
          <SettingRow label="Log files" help="Where diagnostic files are written on this machine.">
            <button data-testid="rec-open-logs" type="button" className="btn" onClick={() => { void tauri.openLogsDir(); }}>
              Open folder
            </button>
          </SettingRow>
        </Group>
      )}
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
  // Slider position while dragging; persisted on release.
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
        <Select
          testId="rec-speech-level-device"
          label="Microphone to measure"
          value={selId?.toString() ?? ''}
          onChange={(v) => setSelId(v ? Number(v) : null)}
          options={mics.map((s) => ({ value: String(s.id), label: s.description }))}
        />
        {sel && calibrating && <MicLevel tauriSourceId={sel.id} width={120} height={8} />}
      </div>
      <p className="meta" style={{ fontSize: 'var(--t-small)', margin: 0 }}>
        {info?.effective_dbfs != null
          ? (
            <>
              Speech level:{' '}
              <span className="mono">
                {info.effective_dbfs.toFixed(1)} dBFS
              </span>
              {sourceLabel ? ` (${sourceLabel})` : ''}
            </>
          )
          : 'No data yet for this microphone — it learns automatically from meetings, or calibrate below.'}
      </p>
      <div style={{ display: 'flex', alignItems: 'center', gap: 10 }}>
        <button data-testid="rec-calibrate" type="button" className="btn" onClick={() => void calibrate()} disabled={!sel || calibrating}>
          {calibrating ? 'Listening… read a sentence aloud' : 'Calibrate by speaking'}
        </button>
      </div>
      {calError && <p className="meta meta--danger" style={{ margin: 0 }}>{calError}</p>}
      <div style={{ display: 'flex', alignItems: 'center', gap: 8, fontSize: 'var(--t-ui)' }}>
        <Toggle
          testId="rec-speech-level-manual"
          label="Manual override"
          checked={overrideOn}
          disabled={!sel}
          onChange={(on) => {
            if (!sel) return;
            const v = on ? Math.round((info?.effective_dbfs ?? -20) * 10) / 10 : null;
            void tauri.speechLevelSetOverride(sel.description, v).then(refresh);
          }}
        />
        Manual override
      </div>
      {overrideOn && sel && (
        <div style={{ display: 'flex', alignItems: 'center', gap: 10 }}>
          <input
            data-testid="rec-speech-level-slider"
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
          <span className="mono" style={{ fontSize: 'var(--t-small)' }}>
            {(sliderDbfs ?? info?.override_dbfs ?? -20).toFixed(1)} dBFS
          </span>
        </div>
      )}
      <p className="meta" style={{ fontSize: 'var(--t-small)', margin: 0, lineHeight: 'var(--lh-text)' }}>
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
export function ProvidersSection({
  providerRows, current, enable, onSaved, machine, nerd,
}: {
  providerRows: ProviderListEntry[];
  current: ProviderId | null;
  enable: (p: ProviderId, onError: (message: string | null) => void) => Promise<void>;
  onSaved: (name: ProviderId) => void;
  machine: MachineSettings;
  nerd: boolean;
}) {
  const [enableError, setEnableError] = useState<string | null>(null);
  return (
    <section>
      <Group
        title="Local transcription"
        hint="Transcription always runs on-device. One model is enabled at a time; switching applies to the next recording."
      >
        <AsrModels nerd={nerd} />
      </Group>

      <Group
        title="AI provider"
        hint={nerd
          ? `Summary, Q&A, chapters, analysis and transcript polish run on Daisy's built-in model`
            + ' on this computer, or through a provider key. Configure the ones you have, then Enable the'
            + ' one to use. Keys are stored encrypted in <profile>/keys.vault.json.'
          : `Summary, Q&A, chapters, analysis and transcript polish run on Daisy's built-in model on this`
            + ' computer. Turn on Nerd Mode in Settings → Display to add a cloud provider.'}
      >
        {enableError && <p className="meta meta--danger" data-testid="provider-enable-error">{enableError}</p>}
        <LocalModel
          enabled={current === 'local'}
          onEnable={() => void enable('local', setEnableError)}
        />
        {/* Cloud and self-hosted providers show with Nerd Mode on. */}
        {providerRows.filter((p) => nerd || current === p.name).map((p) => PROVIDER_DEFAULTS[p.name]
          ? (
            <ProviderEditor
              key={p.name}
              provider={p}
              onSaved={() => onSaved(p.name)}
              enabled={current === p.name}
              onEnable={() => void enable(p.name, setEnableError)}
            />
          )
          : (
            <div key={p.name} className="list-row" data-testid={`provider-row-${p.name}`}>
              <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center', gap: 12 }}>
                <div className="list-row__name"><span>{providerLabel(p.name)}</span></div>
                {current === p.name
                  ? <span className="meta--success asr-model__active" data-testid={`provider-enabled-${p.name}`}>● Enabled</span>
                  : (
                    <button data-testid={`provider-enable-${p.name}`} className="btn" style={{ whiteSpace: 'nowrap' }}
                            onClick={() => void enable(p.name, setEnableError)}>
                      Enable
                    </button>
                  )}
              </div>
            </div>
          ))}
      </Group>

      {nerd && (
        <Group title="Advanced">
          <details className="set-group__details">
            <summary className="set-group__summary">Transcription tuning</summary>
            <AsrTunables machine={machine} />
          </details>
        </Group>
      )}
    </section>
  );
}

/** Clamped number input: commits on change, pulls to [min, max] at the edge. */
function TuneNumber({ testId, value, min, max, onCommit }: {
  testId: string; value: number; min: number; max: number; onCommit: (v: number) => void;
}) {
  // Free typing while focused; clamps and commits on blur or Enter.
  const [draft, setDraft] = useState<string | null>(null);
  const commit = () => {
    if (draft === null) return;
    const raw = Number(draft);
    if (Number.isFinite(raw) && draft.trim() !== '') {
      onCommit(Math.min(max, Math.max(min, Math.round(raw))));
    }
    setDraft(null);
  };
  return (
    <input
      data-testid={testId}
      type="number"
      min={min}
      max={max}
      value={draft ?? value}
      onChange={(e) => setDraft(e.target.value)}
      onBlur={commit}
      onKeyDown={(e) => { if (e.key === 'Enter') (e.target as HTMLInputElement).blur(); }}
    />
  );
}

/** Per-engine tunables (Providers → Advanced). The whisper group resets to
 *  the backend's platform default by omitting its block from the update. */
function AsrTunables({ machine }: { machine: MachineSettings }) {
  const asr = machine.asr;
  const wh = asr.whisper;
  const cores = navigator.hardwareConcurrency || 8;
  const setAsr = (next: AsrSettings) => saveMachine({ asr: next });
  // The stored block comes back with the platform default in its place.
  const resetWhisper = () => {
    const next = { ...asr };
    delete next.whisper;
    saveMachine({ asr: next });
  };
  return (
    <div className="tune-panel">
      <p className="hint">Changes apply to the next recording.</p>

      <h4 className="tune-panel__engine">Whisper</h4>
      {wh && (
        <>
          <Field label="Language">
            <input
              data-testid="rec-whisper-language"
              type="text"
              placeholder="auto"
              value={wh.language ?? ''}
              onChange={(e) =>
                setAsr({ ...asr, whisper: { ...wh, language: e.target.value.trim() || null } })}
            />
          </Field>
          <Field label="Live window cap (s)">
            <TuneNumber testId="rec-asr-number-live-window-cap-s" value={wh.window_cap_s} min={15} max={300}
              onCommit={(v) => setAsr({ ...asr, whisper: { ...wh, window_cap_s: v } })} />
          </Field>
          <Field label="Decode threads (0 = auto)">
            <TuneNumber testId="rec-whisper-threads" value={wh.threads} min={0} max={cores}
              onCommit={(v) => setAsr({ ...asr, whisper: { ...wh, threads: v } })} />
          </Field>
        </>
      )}
      <div>
        <button data-testid="rec-whisper-reset" className="btn" onClick={() => resetWhisper()}>Reset to defaults</button>
      </div>
    </div>
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
      <p className="set-group__hint">
        Outbound destinations for a meeting’s summary, notes and/or transcript. A webhook receives a single JSON POST — the platform on the other end (Zapier, n8n, your own endpoint…) decides what to do with it. Add as many as you like. Secrets are write-only — once saved, they’re never shown again.
      </p>

      {error && <p className="meta meta--danger" style={{ marginTop: 8 }}>{error}</p>}

      <div style={{ marginTop: 16 }}>
        {(items ?? []).map((it) =>
          editing && editing !== 'new' && editing.id === it.id ? (
            <IntegrationEditor key={it.id} initial={it} onSaved={() => { setEditing(null); reload(); }} onCancel={() => setEditing(null)} />
          ) : (
            <div key={it.id} className="list-row">
              <div className="list-row__main">
                <div className="list-row__name">
                  {!it.enabled && (
                    <span className="meta list-row__off">disabled</span>
                  )}
                  {it.name}
                </div>
                <div className="meta" style={{ fontSize: 'var(--t-small)', wordBreak: 'break-all' }}>
                  {it.kind} · {it.webhook_url} · auth: {it.auth_kind === 'header' ? `header ${it.auth_header_name}` : it.auth_kind} · sends: {payloadSummaryStr(it.payloads)}
                </div>
              </div>
              <button data-testid={`integration-edit-${it.id}`} className="btn" onClick={() => setEditing(it)}>Edit</button>
              <RowMenu
                testId={`integration-menu-${it.id}`}
                label={`Actions for ${it.name}`}
                items={[{
                  label: 'Delete',
                  testId: `integration-delete-${it.id}`,
                  danger: true,
                  onSelect: () => del(it.id, it.name),
                }]}
              />
            </div>
          ),
        )}
        {items && items.length === 0 && editing !== 'new' && <p className="meta" style={{ marginTop: 8 }}>No destinations yet.</p>}
      </div>

      {editing === 'new' ? (
        <IntegrationEditor onSaved={() => { setEditing(null); reload(); }} onCancel={() => setEditing(null)} />
      ) : (
        <button data-testid="integration-add" className="btn btn--primary" style={{ marginTop: 16 }} onClick={() => setEditing('new')}>+ Add destination</button>
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
    <div className="editor-card">
      <div className="editor-card__title">{isEdit ? `Edit “${initial!.name}”` : 'New destination'}</div>

      <div style={{ marginTop: 8 }}>
        <label className="meta">Name</label>
        <input data-testid="integration-name" style={{ display: 'block', width: '100%' }} value={name} disabled={busy} onChange={(e) => setName(e.target.value)} placeholder="e.g. Zapier — meeting notes" />
      </div>
      <div style={{ marginTop: 12 }}>
        <label className="meta">Webhook URL <span>(https://, or http://localhost for local n8n)</span></label>
        <input data-testid="integration-url" style={{ display: 'block', width: '100%' }} value={url} disabled={busy} onChange={(e) => setUrl(e.target.value)} placeholder="https://hooks.zapier.com/hooks/catch/…" />
      </div>

      <div style={{ marginTop: 12 }}>
        <label className="meta">Auth</label>
        <div style={{ display: 'flex', flexWrap: 'wrap', gap: 14, marginTop: 4, fontSize: 'var(--t-ui)' }}>
          {isEdit && <label><input data-testid="integration-auth-keep" type="radio" checked={authMode === 'keep'} onChange={() => setAuthMode('keep')} /> keep current</label>}
          <label><input data-testid="integration-auth-none" type="radio" checked={authMode === 'none'} onChange={() => setAuthMode('none')} /> none</label>
          <label><input data-testid="integration-auth-header" type="radio" checked={authMode === 'header'} onChange={() => setAuthMode('header')} /> custom header</label>
          <label><input data-testid="integration-auth-bearer" type="radio" checked={authMode === 'bearer'} onChange={() => setAuthMode('bearer')} /> bearer token</label>
        </div>
        {authMode === 'header' && (
          <div style={{ display: 'flex', gap: 8, marginTop: 8 }}>
            <input data-testid="integration-header-name" style={{ display: 'block', flex: 1 }} value={headerName} disabled={busy} onChange={(e) => setHeaderName(e.target.value)} placeholder="Header name (e.g. X-API-Key)" />
            <input data-testid="integration-header-value" style={{ display: 'block', flex: 2 }} type="password" value={headerValue} disabled={busy} onChange={(e) => setHeaderValue(e.target.value)} placeholder="Header value" />
          </div>
        )}
        {authMode === 'bearer' && (
          <input data-testid="integration-bearer" style={{ display: 'block', width: '100%', marginTop: 8 }} type="password" value={bearer} disabled={busy} onChange={(e) => setBearer(e.target.value)} placeholder="Bearer token" />
        )}
        {isEdit && authMode === 'keep' && initial!.auth_kind !== 'none' && (
          <p className="meta" style={{ fontSize: 'var(--t-small)', marginTop: 4 }}>
            Currently using {initial!.auth_kind === 'header' ? `header “${initial!.auth_header_name}”` : 'a bearer token'} — leave this to keep it, or pick another option to replace it.
          </p>
        )}
      </div>

      <div style={{ marginTop: 12 }}>
        <label className="meta">Include in the payload</label>
        <div style={{ display: 'flex', gap: 16, marginTop: 4, fontSize: 'var(--t-ui)' }}>
          <label><input data-testid="integration-include-summary" type="checkbox" checked={pSummary} onChange={(e) => setPSummary(e.target.checked)} /> summary</label>
          <label><input data-testid="integration-include-notes" type="checkbox" checked={pNotes} onChange={(e) => setPNotes(e.target.checked)} /> notes</label>
          <label><input data-testid="integration-include-transcript" type="checkbox" checked={pTranscript} onChange={(e) => setPTranscript(e.target.checked)} /> transcript</label>
        </div>
      </div>

      <div style={{ marginTop: 12, fontSize: 'var(--t-ui)' }}>
        <label><input data-testid="integration-enabled" type="checkbox" checked={enabled} onChange={(e) => setEnabled(e.target.checked)} /> enabled (appears in “Send to…” on a meeting)</label>
      </div>

      <div style={{ display: 'flex', gap: 8, marginTop: 16 }}>
        <button data-testid="integration-save" className="btn btn--primary" disabled={busy || !name.trim() || !url.trim()} onClick={save}>{busy ? 'Saving…' : 'Save'}</button>
        <button data-testid="integration-cancel" className="btn" disabled={busy} onClick={onCancel}>Cancel</button>
      </div>
      {error && <p className="meta meta--danger" style={{ marginTop: 8 }}>{error}</p>}
    </div>
  );
}

// ============================================================================
// Recordings (clear transcribed-or-orphaned audio)
// ============================================================================

function StorageSection() {
  const [stats, setStats] = useState<RecordingsStats | null>(null);
  const [confirmDelete, setConfirmDelete] = useState(false);

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
    void startDeleteAll();
  }

  const pct = progress && progress.total > 0 ? Math.round((progress.done / progress.total) * 100) : 0;
  const deletable = stats?.deletable_session_count ?? 0;

  return (
    <section>
      {stats && (
        <p style={{ fontSize: 'var(--t-ui)', marginTop: 12 }}>
          <strong>{stats.session_count}</strong> recording{stats.session_count === 1 ? '' : 's'} ·{' '}
          <strong>{formatBytes(stats.wav_bytes)}</strong> of WAVs
          {stats.opus_bytes > 0 && <> · {formatBytes(stats.opus_bytes)} of Opus</>} ·{' '}
          <strong>{deletable}</strong> eligible to clear ({formatBytes(stats.deletable_bytes)})
        </p>
      )}

      {progress !== null && (
        <div style={{ marginTop: 16 }}>
          <div className="progress">
            <div className="progress__fill" style={{ width: `${pct}%` }} />
          </div>
          <p className="meta" style={{ fontSize: 'var(--t-small)', marginTop: 4 }}>
            Clearing {progress.done}/{progress.total}
            {progress.current ? ` — ${progress.current}` : ''}
          </p>
        </div>
      )}

      {msg && <p className="meta" style={{ fontSize: 'var(--t-ui)', marginTop: 12 }}>{msg}</p>}

      <Group
        title="Clear transcribed / orphaned recordings"
        hint="Deletes only the bulky raw WAV chunks for sessions that have already been transcribed, or that have no Library entry. The compact meeting.opus archive, transcripts, summaries, notes and the session record are always kept — so you can still play back and re-transcribe. This can’t be undone."
      >
        <button
          data-testid="storage-clear"
          onClick={() => setConfirmDelete(true)}
          disabled={busy !== null || deletable === 0}
          className="btn btn--danger"
          style={{ marginTop: 10 }}
        >
          Clear {deletable} recording{deletable === 1 ? '' : 's'}…
        </button>
        {confirmDelete && (
          <ConfirmDialog
            title={`Clear ${deletable} recording${deletable === 1 ? '' : 's'}?`}
            body={<>
              This permanently deletes the raw WAV audio for <strong>{deletable}</strong> session{deletable === 1 ? '' : 's'} and frees about <strong>{formatBytes(stats?.deletable_bytes)}</strong>. Playback (meeting.opus), transcripts and summaries are kept.
            </>}
            confirmLabel="Delete the audio"
            typedConfirm="DELETE"
            danger
            onCancel={() => setConfirmDelete(false)}
            onConfirm={runDelete}
          />
        )}
      </Group>
    </section>
  );
}

// ============================================================================
// Profile (+ vault lock)
// ============================================================================
function VoiceprintsSection({ settings }: { settings: SettingsT }) {
  // Committed on release, not on every step of a drag.
  const [tolerance, setTolerance] = useState(settings.voice_match_tolerance ?? 65);
  const [rows, setRows] = useState<VoiceprintView[] | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const [editing, setEditing] = useState<VoiceprintView | null>(null);
  const [rematching, setRematching] = useState(false);
  const [rematchMsg, setRematchMsg] = useState<string | null>(null);
  const reload = () => {
    setErr(null);
    tauri.listVoiceprints()
      .then((v) => setRows([...v].sort((a, b) => a.display_name.localeCompare(b.display_name, undefined, { sensitivity: 'base' }))))
      .catch((e) => setErr(errStr(e)));
  };
  useEffect(() => {
    reload();
  }, []);

  async function remove(v: VoiceprintView) {
    const ok = await confirm({
      title: `Delete ${v.display_name}?`,
      body: `Their voice and contact are removed here and on your phone. Past meetings keep the name.`,
      confirmLabel: 'Delete', danger: true,
    });
    if (!ok) return;
    try { await tauri.deleteVoiceprint(v.id); reload(); }
    catch (e) { setErr(errStr(e)); }
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
      setErr(errStr(e));
    } finally {
      setRematching(false);
    }
  }

  return (
    <section>
      <p className="set-group__hint">
        Voice embeddings Daisy uses to recognize speakers across meetings. Enrolled from the
        participants tab when you label a speaker — the audio sample never leaves your machine.
        Voiceprints are biometric data; only enroll people you have permission to identify.
      </p>

      {err && <p className="meta meta--danger">{err}</p>}
      {rematchMsg && <p className="meta meta--success">{rematchMsg}</p>}
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
            data-testid="vp-rematch"
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
        <table className="data-table">
          <thead>
            <tr>
              <th>Name</th>
              <th>Email</th>
              <th>Sessions</th>
              <th>Samples</th>
              <th></th>
            </tr>
          </thead>
          <tbody>
            {rows.map((r) => (
              <tr key={r.id}>
                <td>{r.display_name}</td>
                <td className="data-table__dim">{r.email ?? '—'}</td>
                <td className="data-table__dim">{r.session_count}</td>
                <td className="data-table__dim" title="Voice samples in this person's gallery — more samples = more robust matching.">{r.sample_count}</td>
                <td className="data-table__actions">
                  <button data-testid={`vp-rename-${r.id}`} className="btn" onClick={() => setEditing(r)}>Rename</button>{' '}
                  <RowMenu
                    testId={`vp-menu-${r.id}`}
                    label={`Actions for ${r.display_name}`}
                    items={[{
                      label: 'Delete',
                      testId: `vp-delete-${r.id}`,
                      danger: true,
                      onSelect: () => void remove(r),
                    }]}
                  />
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

      <Group title="Advanced">
        <details className="set-group__details">
          <summary data-testid="vp-advanced" className="set-group__summary">Speaker recognition tuning</summary>

          <div className="vp-tuning">
            <label className="meta vp-tuning__label" htmlFor="vp-tolerance">
              Voice matching tolerance
            </label>
            <div className="vp-tuning__row">
              <span className="meta vp-tuning__end">Stricter</span>
              <input
                data-testid="vp-tolerance"
                id="vp-tolerance"
                type="range" min={0} max={100} step={5}
                value={tolerance}
                onChange={(e) => setTolerance(Number(e.target.value))}
                onPointerUp={() => saveSettings({ voice_match_tolerance: tolerance })}
                onKeyUp={() => saveSettings({ voice_match_tolerance: tolerance })}
              />
              <span className="meta vp-tuning__end">Looser</span>
              <span className="vp-tuning__value">{tolerance}</span>
            </div>
            <p className="meta vp-tuning__help">
              If Daisy keeps reporting &ldquo;not one clear voice&rdquo;, move this up. If it splits
              one person into several speakers, move this down. Applies the next time you diarize.
            </p>
          </div>
        </details>
      </Group>
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
      setErr(errStr(e));
      setBusy(false);
    }
  }
  return (
    <div data-testid="vp-rename-backdrop" className="modal-backdrop" onClick={busy ? undefined : onClose}>
      <div data-testid="vp-rename-modal" className="modal" onClick={(e) => e.stopPropagation()} style={{ maxWidth: 420 }}>
        <h2 className="modal__title">Rename voiceprint</h2>
        <label className="field-label">Name</label>
        <input data-testid="vp-name" type="text" autoFocus value={name} disabled={busy}
               onChange={(e) => setName(e.target.value)}
               style={{ width: '100%', marginBottom: 12 }} />
        <label className="field-label">Email (optional)</label>
        <input data-testid="vp-email" type="email" value={email} disabled={busy}
               onChange={(e) => setEmail(e.target.value)}
               style={{ width: '100%', marginBottom: 12 }} />
        {err && <p className="meta meta--danger">{err}</p>}
        <div style={{ display: 'flex', gap: 8, justifyContent: 'flex-end' }}>
          <button data-testid="vp-cancel" className="btn" onClick={onClose} disabled={busy}>Cancel</button>
          <button data-testid="vp-save" className="btn btn--primary" onClick={() => void save()} disabled={busy || !name.trim()}>
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
  const nerd = useNerdMode();
  return (
    <section>
      <Group title="Your name">
        {licensed ? (
          <>
            <input data-testid="profile-name" type="text" value={userDisplayName} readOnly disabled style={{ width: 260 }} />
            <p className="meta" style={{ fontSize: 'var(--t-small)', marginTop: 6 }}>
              Set from your license and locked to the licensee. Used so the summarizer
              refers to you as &ldquo;you&rdquo; in TL;DRs and action items.
            </p>
          </>
        ) : (
          <>
            <div style={{ display: 'flex', gap: 8, alignItems: 'center' }}>
              <input
                data-testid="profile-display-name"
                type="text"
                value={draft}
                onChange={(e) => setDraft(e.target.value)}
                placeholder="e.g. Daisy"
                style={{ width: 260 }}
              />
              <button
                data-testid="profile-save"
                className="btn btn--primary"
                disabled={!dirty}
                onClick={() => onUserDisplayNameChange(draft.trim())}
              >
                {dirty ? 'Save' : 'Saved'}
              </button>
            </div>
            <p className="meta" style={{ fontSize: 'var(--t-small)', marginTop: 6 }}>
              Used to identify you in recordings — the summarizer refers to you as &ldquo;you&rdquo;
              in TL;DRs and action items instead of by name.
            </p>
          </>
        )}
      </Group>

      <Group title="Profile directory">
        <table style={{ width: '100%', fontSize: 'var(--t-ui)' }}>
          <tbody><Row label="Profile directory" value={envOverride ?? profileDir ?? '—'} /></tbody>
        </table>
        {envOverride && (
          <p className="meta" style={{ fontSize: 'var(--t-meta)', marginTop: 2 }}>
            Set by the <span className="mono">DAISY_PROFILE_DIR</span> environment
            variable for this session — overrides the saved location{profileDir ? ` (${profileDir})` : ''}.
          </p>
        )}
        {nerd && (
          <div style={{ marginTop: 8, display: 'flex', alignItems: 'center', gap: 12 }}>
            <button data-testid="profile-open-dir" onClick={() => void tauri.openProfileDir().catch(() => {})} className="btn">Open Profile Directory</button>
            <button data-testid="profile-switch" onClick={onSwitch} className="btn">Switch profile…</button>
            <button data-testid="profile-move" onClick={onMove} disabled={moving} className="btn">{moving ? 'Moving…' : 'Move profile…'}</button>
          </div>
        )}
      </Group>

      {nerd && (
        <Group title="Vault">
          <VaultSection onLock={onLock} />
        </Group>
      )}

      <Group title="Re-run setup wizard">
        <button data-testid="profile-open-wizard" onClick={onLaunchWizard} className="btn">
          ↻ Open wizard
        </button>
      </Group>
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
        'vault is unrecoverable. Other computers that open this profile folder need a passphrase: ' +
        'they stay locked until you add one here again.',
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
        <p className="meta" style={{ fontSize: 'var(--t-ui)', margin: '0 0 10px' }}>
          <strong>Trust this machine.</strong> Your API keys + voiceprints are encrypted under a
          machine-derived key, so Daisy unlocks automatically — no passphrase to enter.
          Only this computer can open the profile. To open it on another computer, add a passphrase
          here and enter it there.
        </p>
        {!adding ? (
          <button data-testid="vault-add-passphrase" className="btn" disabled={busy} onClick={() => { setErr(null); setAdding(true); }}>
            Add a passphrase…
          </button>
        ) : (
          <div style={{ display: 'flex', flexDirection: 'column', gap: 8, maxWidth: 340 }}>
            <input data-testid="vault-new" type="password" autoComplete="new-password" placeholder="New passphrase"
              value={addPass} onChange={(e) => setAddPass(e.target.value)} disabled={busy} style={{ width: '100%' }} />
            <input data-testid="vault-new-confirm" type="password" autoComplete="new-password" placeholder="Confirm passphrase"
              value={addConfirm} onChange={(e) => setAddConfirm(e.target.value)} disabled={busy}
              onKeyDown={(e) => { if (e.key === 'Enter') void addPassphrase(); }} style={{ width: '100%' }} />
            <div style={{ display: 'flex', gap: 8 }}>
              <button data-testid="vault-add-submit" className="btn btn--primary" disabled={busy || !addPass} onClick={() => void addPassphrase()}>
                {busy ? 'Migrating…' : 'Add passphrase'}
              </button>
              <button data-testid="vault-add-cancel" className="btn" disabled={busy} onClick={() => { setAdding(false); setAddPass(''); setAddConfirm(''); setErr(null); }}>Cancel</button>
            </div>
            <p className="meta" style={{ fontSize: 'var(--t-meta)', margin: 0 }}>
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
    <div className="inset-box">
      <p className="meta" style={{ margin: '0 0 10px' }}>
        <strong>Passphrase-protected.</strong> Daisy asks for your passphrase each launch.
      </p>
      {!changing ? (
        <div style={{ display: 'flex', gap: 8, flexWrap: 'wrap' }}>
          <button data-testid="vault-change" className="btn" disabled={busy} onClick={() => { setErr(null); setChanging(true); }}>Change passphrase…</button>
          <button data-testid="vault-lock" className="btn" disabled={busy} onClick={onLock}>Lock vault now</button>
          <button data-testid="vault-drop" className="btn" disabled={busy} onClick={() => void dropPassphrase()}>Drop passphrase (trust this machine)…</button>
        </div>
      ) : (
        <div style={{ display: 'flex', flexDirection: 'column', gap: 8, maxWidth: 340 }}>
          <input data-testid="vault-current" type="password" autoComplete="current-password" placeholder="Current passphrase"
            value={oldPass} onChange={(e) => setOldPass(e.target.value)} disabled={busy} style={{ width: '100%' }} />
          <input data-testid="vault-change-new" type="password" autoComplete="new-password" placeholder="New passphrase"
            value={newPass} onChange={(e) => setNewPass(e.target.value)} disabled={busy} style={{ width: '100%' }} />
          <input data-testid="vault-change-confirm" type="password" autoComplete="new-password" placeholder="Confirm new passphrase"
            value={confirmPass} onChange={(e) => setConfirmPass(e.target.value)} disabled={busy}
            onKeyDown={(e) => { if (e.key === 'Enter') void changePassphrase(); }} style={{ width: '100%' }} />
          <div style={{ display: 'flex', gap: 8 }}>
            <button data-testid="vault-change-submit" className="btn btn--primary" disabled={busy || !oldPass || !newPass} onClick={() => void changePassphrase()}>
              {busy ? 'Changing…' : 'Save new passphrase'}
            </button>
            <button data-testid="vault-change-cancel" className="btn" disabled={busy} onClick={() => { setChanging(false); setOldPass(''); setNewPass(''); setConfirmPass(''); setErr(null); }}>Cancel</button>
          </div>
          <p className="meta" style={{ fontSize: 'var(--t-meta)', margin: 0 }}>
            Min 22 characters. No recovery if you forget it.
          </p>
        </div>
      )}
      {/* Error from the vault actions. */}
      <FieldError testId="vault-change-error">{err}</FieldError>
      <FieldOk>{msg}</FieldOk>
    </div>
  );
}

// ============================================================================
// About
// ============================================================================

function CalendarsSection({ onChanged }: { onChanged?: () => void }) {
  const [subs, setSubs] = useState<CalendarSubscription[] | null>(null);
  const tags = useTags();
  const [err, setErr] = useState<string | null>(null);
  const [name, setName] = useState('');
  const [url, setUrl] = useState('');
  const [busy, setBusy] = useState(false);
  const [refreshing, setRefreshing] = useState(false);
  const [refreshMsg, setRefreshMsg] = useState<string | null>(null);
  const [editingColorFor, setEditingColorFor] = useState<string | null>(null);

  const reload = () => {
    setErr(null);
    tauri.listCalendarSubscriptions()
      .then((s) => setSubs(s))
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
      <p className="set-group__hint">
        Paste a read-only ICS URL. Daisy fetches it on demand and suggests the next
        event&apos;s attendees when you start recording.
      </p>
      <details className="set-group__details">
        <summary className="set-group__summary">Where do I find this URL?</summary>
        <p className="set-group__hint">
          Google Calendar: Settings → &ldquo;Secret address in iCal format&rdquo;.
          Microsoft 365 Outlook: Share → Publish → ICS link. Apple Calendar: the
          calendar.icloud.com share link. URLs are stored locally; the only network
          call is the fetch to your calendar provider.
        </p>
      </details>
      {err && <p className="meta meta--danger" style={{ whiteSpace: 'pre-wrap' }}>{err}</p>}
      {refreshMsg && <p className="meta meta--success">{refreshMsg}</p>}

      <div className="inset-box inset-box--dashed" style={{ marginBottom: 16 }}>
        <label className="field-label">Name</label>
        <input
          data-testid="cal-name"
          type="text" value={name} disabled={busy}
          onChange={(e) => setName(e.target.value)}
          placeholder="e.g. Work, Personal"
          style={{ display: 'block', width: 320, marginBottom: 8 }}
        />
        <label className="field-label">ICS URL</label>
        <input
          data-testid="cal-url"
          type="url" value={url} disabled={busy}
          onChange={(e) => setUrl(e.target.value)}
          placeholder="https://calendar.google.com/calendar/ical/.../basic.ics"
          style={{ display: 'block', width: '100%', maxWidth: 720, marginBottom: 10 }}
        />
        <button data-testid="cal-add" className="btn btn--primary" disabled={busy || !name.trim() || !url.trim()} onClick={() => void add()}>
          {busy ? 'Adding…' : 'Add subscription'}
        </button>
      </div>

      {subs && subs.length > 0 && (
        <div style={{ marginBottom: 12, display: 'flex', alignItems: 'center', gap: 12 }}>
          <button data-testid="cal-refresh" className="btn" disabled={refreshing} onClick={() => void refresh()}>
            {refreshing ? 'Refreshing…' : '↻ Refresh now'}
          </button>
        </div>
      )}

      {subs == null && <p className="meta">Loading…</p>}
      {subs && subs.length === 0 && (
        <p className="meta" style={{ marginTop: 12 }}>No calendars yet. Add one above — Daisy will suggest attendees when you record and remind you just before meetings start.</p>
      )}
      {subs && subs.length > 0 && (
        <table className="data-table data-table--top">
          <thead>
            <tr>
              <th style={{ width: 60 }}>Enabled</th>
              <th style={{ width: 36 }}>Color</th>
              <th>Name</th>
              <th style={{ width: 160 }}>Auto-tag</th>
              <th>URL</th>
              <th style={{ width: 70 }}></th>
            </tr>
          </thead>
          <tbody>
            {subs.map((s) => (
              <tr key={s.id}>
                <td>
                  <Toggle
                    testId={`cal-enabled-${s.id}`}
                    label={`Sync ${s.name}`}
                    checked={s.enabled}
                    onChange={(on) => void toggle(s, on)}
                  />
                </td>
                <td style={{ position: 'relative' }}>
                  <button
                    data-testid={`cal-color-${s.id}`}
                    className="color-swatch"
                    onClick={() => setEditingColorFor(editingColorFor === s.id ? null : s.id)}
                    aria-label="Pick color"
                    title="Pick color"
                    style={{ '--swatch': s.color_hex } as React.CSSProperties}
                  />
                  {editingColorFor === s.id && (
                    <div className="menu swatch-pop">
                      <TagColorPicker value={s.color_hex} onChange={(hex) => { void changeColor(s, hex); setEditingColorFor(null); }} />
                    </div>
                  )}
                </td>
                <td>{s.name}</td>
                <td>
                  <Select
                    testId={`cal-tag-${s.id}`}
                    label={`Auto-tag for ${s.name}`}
                    className="cal-tag-select"
                    value={s.tag_id ?? ''}
                    onChange={(v) => void changeTag(s, v)}
                    options={[
                      { value: '', label: '(no auto-tag)' },
                      ...tags.map((t) => ({ value: t.id, label: t.name })),
                    ]}
                  />
                </td>
                <td className="data-table__url" title={s.url}>
                  {hideQuery(s.url)}
                </td>
                <td className="data-table__actions">
                  <RowMenu
                    testId={`cal-menu-${s.id}`}
                    label={`Actions for ${s.name}`}
                    items={[{
                      label: 'Delete',
                      testId: `cal-delete-${s.id}`,
                      danger: true,
                      onSelect: () => void remove(s),
                    }]}
                  />
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
    <Group title="License">
      {status.state === 'licensed' && (
        <>
          <p style={{ fontSize: 'var(--t-ui)', margin: '0 0 4px' }}>
            Licensed to <strong>{status.name || '—'}</strong>
            {status.email && <> &lt;{status.email}&gt;</>}
          </p>
          <p className="meta mono" style={{ fontSize: 'var(--t-meta)' }}>
            License ID: {fingerprint || '…'}
          </p>
          {status.expires && (
            <p className="meta" style={{ fontSize: 'var(--t-small)' }}>Expires {new Date(status.expires * 1000).toLocaleDateString()}</p>
          )}
          <p className="meta" style={{ fontSize: 'var(--t-meta)', marginTop: 8 }}>
            This license allows 3 devices.{' '}
            <button
              data-testid="about-deactivate"
              onClick={() => void deactivate()}
              disabled={busy}
              className="btn-link"
            >Deactivate this device</button>{' '}to free a seat.
          </p>
        </>
      )}
      {status.state !== 'licensed' && (
        <>
          <p className="meta" style={{ fontSize: 'var(--t-ui)', marginBottom: 8 }}>
            {status.state === 'trial' ? `Trial — ${status.days_left} day${status.days_left === 1 ? '' : 's'} left.` : 'Trial ended.'}
          </p>
          <textarea
            data-testid="about-license-key"
            value={key} disabled={busy}
            onChange={(e) => setKey(e.target.value)}
            placeholder="paste your license key"
            rows={2}
            className="textarea--mono" style={{ width: '100%' }}
          />
          <div style={{ display: 'flex', gap: 8, marginTop: 8 }}>
            <button data-testid="about-activate" className="btn btn--primary" disabled={busy || !key.trim()} onClick={() => void activate()}>Activate</button>
            <button data-testid="about-get-license" className="btn" onClick={() => { void tauri.openExternal(PRICING_URL); }}>Get a license ↗</button>
          </div>
        </>
      )}
      {err && <p className="meta meta--danger" style={{ marginTop: 8 }}>{err}</p>}
    </Group>
  );
}

// Read-only viewer for the in-app Terms of Service + Privacy Policy the user
// accepted at first run (the embedded copy, available offline).
function LegalViewer({ onClose }: { onClose: () => void }) {
  const [tab, setTab] = useState<'tos' | 'privacy'>('tos');
  return (
    <div data-testid="about-legal-backdrop" className="modal-backdrop" onClick={onClose}>
      <div
        data-testid="about-legal-modal"
        className="modal"
        style={{ width: 'min(720px, 94vw)', maxHeight: '86vh', display: 'flex', flexDirection: 'column' }}
        onClick={(e) => e.stopPropagation()}
      >
        <div className="modal__title">Terms &amp; Privacy</div>
        <p className="meta" style={{ fontSize: 'var(--t-small)', margin: '2px 0 10px' }}>
          The version you accepted. Last updated {LEGAL_LAST_UPDATED}.
        </p>
        <div style={{ display: 'flex', gap: 8, marginBottom: 8 }}>
          {(['tos', 'privacy'] as const).map((t) => (
            <button
              key={t}
              data-testid={`about-legal-${t}`}
              className={tab === t ? 'doc-tab is-on' : 'doc-tab'}
              onClick={() => setTab(t)}
            >
              {t === 'tos' ? 'Terms of Service' : 'Privacy Policy'}
            </button>
          ))}
        </div>
        <div className="doc-pane" style={{ textAlign: 'left' }}>
          <MarkdownView markdown={tab === 'tos' ? TOS_MARKDOWN : PRIVACY_MARKDOWN} />
        </div>
        <div className="modal__actions">
          <button data-testid="about-legal-close" className="btn" onClick={onClose}>Close</button>
        </div>
      </div>
    </div>
  );
}

function AboutSection({ onLicenseChanged, settings }: {
  onLicenseChanged?: () => void;
  settings: SettingsT;
}) {
  const [checking, setChecking] = useState(false);
  async function checkNow(): Promise<void> {
    setChecking(true);
    try {
      const u = await tauri.checkForUpdate();
      await alert({
        title: u.update_available ? `Version ${u.latest} is available` : 'Daisy is up to date',
        body: u.update_available
          ? `You are on ${u.current}. Download the new version from ${u.url ?? 'daisylocal.app'}.${u.notes ? `\n\n${u.notes}` : ''}`
          : `You are on ${u.current}, the newest release.`,
      });
    } catch (e) {
      await alert({ title: 'Could not check for updates', body: errStr(e) });
    } finally {
      setChecking(false);
    }
  }

  const year = new Date().getFullYear();
  const [build, setBuild] = useState<{ version: string; sha: string; tagged: boolean } | null>(null);
  const [showLegal, setShowLegal] = useState(false);
  useEffect(() => { tauri.buildInfo().then(setBuild).catch(() => {}); }, []);
  return (
    <section>
      <div style={{ display: 'flex', flexDirection: 'column', alignItems: 'center', textAlign: 'center', marginTop: 20 }}>
        <span className="about-mark" role="img" aria-label="Daisy" />
        <p style={{ fontSize: 'var(--t-read)', marginTop: 16 }}>
          <strong>Daisy</strong> — Record · Recognize · Recall
        </p>
        <p data-testid="about-version" className="meta" style={{ fontSize: 'var(--t-ui)', marginTop: 4 }}>
          Version {build?.version ?? '…'}{build?.sha ? ` · build ${build.sha}` : ''}
          {build && (
            <>
              {' · '}
              <button
                data-testid="about-release-notes"
                onClick={() => { void tauri.openExternal(releaseNotesUrl(build.version)); }}
                className="btn-link"
              >Release notes ↗</button>
            </>
          )}
        </p>
        <button
          data-testid="about-check-update"
          className="btn"
          onClick={() => void checkNow()}
          disabled={checking}
          style={{ marginTop: 16 }}
          title="Ask daisy.smbr.app whether a newer release exists. Nothing is downloaded or installed."
        >
          {checking ? 'Checking…' : 'Check for new version now'}
        </button>
        <span className="about-toggle">
          <Toggle
            checked={settings.auto_update_check}
            onChange={(v) => saveSettings({ auto_update_check: v })}
            label="Check for new versions automatically"
            testId="toggle-check-for-new-versions-automatically"
          />
          <span>Check automatically</span>
        </span>
        <p className="meta" style={{ fontSize: 'var(--t-small)', marginTop: 2, maxWidth: 330 }}>
          Tells you when an update exists. Never downloads or installs on its own.
        </p>
        <button
          data-testid="about-view-terms"
          className="btn"
          onClick={() => setShowLegal(true)}
          style={{ marginTop: 16 }}
        >
          View Terms &amp; Privacy
        </button>
        {build && !build.tagged && (
          <p className="build-warning">
            For testing purposes only — do not distribute
          </p>
        )}
      </div>

      <LicenseInfo onChanged={onLicenseChanged} />
      {showLegal && <LegalViewer onClose={() => setShowLegal(false)} />}

      <div className="about-footer">
        <div aria-label="Smallbricktory" className="brand-lockup">
          Smallbricktory
        </div>
        <p>A Small Bricktory Production · © {year}</p>
        <details className="set-group__details">
          <summary data-testid="about-legal-credits" className="set-group__summary">Legal &amp; credits</summary>
          <p className="meta">
            <button
              data-testid="about-link-site"
              onClick={() => { void tauri.openExternal('https://www.daisylocal.app'); }}
              className="btn-link"
            >www.daisylocal.app ↗</button>
          </p>
          <p className="meta">
            <button
              data-testid="about-link-github"
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
        </details>
      </div>

      <Group
        title="Open-source models"
        hint="Daisy uses these on-device models for transcription and speaker labeling. All
          audio stays on your machine when you use them."
      >
        <ul style={{ paddingLeft: 20, margin: 0, fontSize: 'var(--t-ui)', lineHeight: 'var(--lh-text)' }}>
          <li style={{ marginBottom: 8 }}>
            <strong>Nemotron Speech Streaming 0.6B</strong> — © NVIDIA ·{' '}
            <a
              href="https://www.nvidia.com/en-us/agreements/enterprise-software/nvidia-open-model-license/"
              target="_blank"
              rel="noreferrer"
              className="gold-link"
            >
              NVIDIA Open Model License
            </a>
            .<br />
            Licensed by NVIDIA Corporation under the NVIDIA Open Model License. ONNX export
            adapted from the sherpa-onnx export script (Apache-2.0).
          </li>
          <li style={{ marginBottom: 8 }}>
            <strong>Whisper (GGML)</strong> — OpenAI Whisper · MIT.<br />
            Downloaded from{' '}
            <a
              href="https://huggingface.co/ggerganov/whisper.cpp"
              target="_blank"
              rel="noreferrer"
              className="gold-link"
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
              className="gold-link"
            >
              huggingface.co/hbredin/wespeaker-voxceleb-resnet34-LM
            </a>
          </li>
        </ul>
      </Group>

      <OpenSourceLicenses />
    </section>
  );
}

function OpenSourceLicenses() {
  const [open, setOpen] = useState(false);
  return (
    <Group>
      <button
        data-testid="about-oss-licenses"
        onClick={() => setOpen((v) => !v)}
        className="btn"
        style={{ width: '100%', justifyContent: 'space-between' }}
        aria-expanded={open}
      >
        <span>
          Open-source licenses · {RUST_LICENSES.reduce((n, l) => n + l.usedBy.length, 0)} Rust
          {' '}crates · {FRONTEND_PACKAGES.length} npm packages · {MODEL_LICENSES.length} models
        </span>
        <span>{open ? '▴' : '▾'}</span>
      </button>
      {open && (
        <div style={{ marginTop: 14, textAlign: 'left', fontSize: 'var(--t-small)', lineHeight: 'var(--lh-text)' }}>
          <p className="meta" style={{ fontSize: 'var(--t-small)' }}>
            Daisy is built on these open-source dependencies. Full license texts and the
            complete per-package list ship with Daisy in <code>THIRD-PARTY-LICENSES.txt</code>{' '}
            and are mirrored at{' '}
            <a data-testid="about-oss-mirror" href="#" onClick={(e) => { e.preventDefault(); void tauri.openExternal('https://www.daisylocal.app/licenses'); }}>
              daisylocal.app/licenses
            </a>.
          </p>
          <ul data-testid="about-model-licenses" style={{ margin: '8px 0 0', paddingLeft: 18 }}>
            {MODEL_LICENSES.map((m, i) => (
              <li key={m.name}>
                {m.name} ·{' '}
                <LicenseLink url={m.url} license={m.license} testId={`about-model-license-${i}`} />
              </li>
            ))}
          </ul>
        </div>
      )}
    </Group>
  );
}

// ============================================================================
// Shared bits
// ============================================================================
function Row({ label, value }: { label: string; value: string }) {
  return (
    <tr>
      <td className="fact-row__label" style={{ padding: '6px 12px 6px 0' }}>{label}</td>
      <td className="fact-row__value" style={{ padding: '6px 0' }}><code>{value}</code></td>
    </tr>
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

export function PromptsSection({ settings }: { settings: SettingsT }) {
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
    // Deleting the default prompt makes Daisy's the default; the settings
    // store hears it from the backend.
    await tauri.deletePrompt(sel.id);
    await reload(DAISY_PROMPT_ID);
  };

  const makeDefault = async () => {
    if (!sel) return;
    await tauri.setDefaultSummaryPrompt(sel.id);
  };

  return (
    <section>
      <p className="set-group__hint">
        Prompts control what Daisy writes about a meeting. The default summary prompt runs
        automatically after each recording; any prompt can be run on demand from the Analyzer.
      </p>
      <label className="field-label field-label--small" htmlFor="prompts-pick">Prompt</label>
      <Select
        id="prompts-pick"
        testId="prompts-pick"
        className="set-select"
        value={selId}
        onChange={(v) => void switchTo(v)}
        options={prompts.map((p) => ({
          value: p.id,
          label: `${p.name}${p.id === defaultId ? ' · default' : ''}`,
        }))}
      />
      {sel && (
        <div style={{ marginTop: 12, maxWidth: 640 }}>
          <label style={{ display: 'block', marginBottom: 8 }}>
            Name
            <input
              data-testid="prompt-name"
              style={{ display: 'block', width: '100%' }}
              value={name}
              disabled={sel.builtin}
              onChange={(e) => setName(e.target.value)}
            />
          </label>
          <label style={{ display: 'block', marginBottom: 8 }}>
            Describe what this analysis should produce
            <textarea
              data-testid="prompt-directive"
              style={{ display: 'block', width: '100%' }}
              value={directive}
              rows={10}
              onChange={(e) => setDirective(e.target.value)}
            />
          </label>
          <FieldError style={{ margin: '0 0 8px' }}>{saveErr}</FieldError>
          <div style={{ display: 'flex', gap: 8, flexWrap: 'wrap' }}>
            {editable && (
              <button data-testid="prompt-save" className="btn btn--primary" onClick={() => void save()} disabled={!dirty}>Save</button>
            )}
            {sel.id === DAISY_PROMPT_ID && (
              <button data-testid="prompt-reset" className="btn" onClick={() => void resetToDefault()}>Reset to default</button>
            )}
            <button data-testid="prompt-save-as" className="btn" onClick={() => { setSavingAs(true); setNewName(''); }}>Save as…</button>
            {!sel.builtin && (
              <button data-testid="prompt-delete" className="btn btn--danger" onClick={() => void remove()}>Delete</button>
            )}
            {sel.id !== defaultId && (
              <button data-testid="prompt-make-default" className="btn" onClick={() => void makeDefault()}>Default for summaries</button>
            )}
          </div>
          {savingAs && (
            <div style={{ marginTop: 12 }}>
              <input
                data-testid="prompt-new-name"
                autoFocus
                placeholder="New prompt name"
                value={newName}
                onChange={(e) => setNewName(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === 'Enter') void saveAsNew();
                }}
              />
              <div style={{ display: 'flex', gap: 8, marginTop: 8 }}>
                <button data-testid="prompt-new-save" className="btn btn--primary" onClick={() => void saveAsNew()} disabled={!newName.trim()}>Save</button>
                <button data-testid="prompt-new-cancel" className="btn" onClick={() => setSavingAs(false)}>Cancel</button>
              </div>
            </div>
          )}
          {sel.builtin && BUILTIN_EXAMPLES[sel.id] && (
            <details className="set-group__details">
              <summary className="set-group__summary">See an example</summary>
              <div className="inset-box" style={{ marginTop: 8 }}>
                <MarkdownView markdown={BUILTIN_EXAMPLES[sel.id]} />
              </div>
            </details>
          )}
        </div>
      )}
    </section>
  );
}

function TagsSection() {
  const tags = useTags();
  const [sel, setSel] = useState<TagSelection>({ kind: 'new' });
  const [name, setName] = useState('');
  const [color, setColor] = useState(DEFAULT_TAG_COLOR);
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
    setColor(DEFAULT_TAG_COLOR);
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
        const updated = await tauri.updateTag({ id: sel.id, name: name.trim(), color_hex: color, prompt_md: promptText.trim(), vocab_md: vocabText.trim() });
        await invalidateTags();
        seedFrom(updated);
      } else {
        const created = await tauri.createTag({ name: name.trim(), color_hex: color, prompt_md: promptText.trim(), vocab_md: vocabText.trim() });
        await invalidateTags();
        seedFrom(created);
      }
      setSavedFlash(wasNew ? `Tag "${name.trim()}" created.` : `Tag "${name.trim()}" saved.`);
    } catch (e: unknown) {
      setErr(errStr(e));
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
      await invalidateTags();
    } catch (e: unknown) {
      const msg = errStr(e);
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
      <div style={{ display: 'flex', gap: 24, alignItems: 'flex-start' }}>
        <div className="tags-column">
          {tags.map((t) => {
            const active = sel.kind === 'tag' && sel.id === t.id;
            return (
              <button
                key={t.id}
                data-testid={`tags-row-${t.id}`}
                className={active ? 'option-row is-on' : 'option-row'}
                onClick={() => seedFrom(t)}
              >
                <span style={{ display: 'flex', alignItems: 'center', gap: 8, fontSize: 'var(--t-ui)' }}>
                  <span className="tag-swatch" style={{ '--tag-color': t.color_hex } as React.CSSProperties} />
                  <span>{t.name}</span>
                </span>
                <span className="meta" style={{ fontSize: 'var(--t-meta)', marginLeft: 22 }}>
                  {t.use_count} session{t.use_count === 1 ? '' : 's'} · {t.prompt_md ? 'prompt' : 'no prompt'}
                </span>
              </button>
            );
          })}
          <button
            data-testid="tags-new"
            className={sel.kind === 'new' ? 'option-row option-row--new is-on' : 'option-row option-row--new'}
            onClick={seedNew}
            style={{ marginTop: 8 }}
          >
            + New tag
          </button>
        </div>

        <div style={{ flex: 1, minWidth: 0 }}>
          <Field label="Display name">
            <input data-testid="tag-name" type="text" value={name} onChange={(e) => setName(e.target.value)} style={{ display: 'block', width: 320 }} placeholder="e.g. Weekly stand-up" />
          </Field>

          <Field label="Color">
            <TagColorPicker value={color} onChange={setColor} />
          </Field>

          <Field label="Prompt (summary + in-call chat, optional)">
            <textarea
              data-testid="tag-prompt"
              className="summary-edit"
              style={{ minHeight: 140, width: '100%', maxWidth: 480 }}
              value={promptText}
              onChange={(e) => setPromptText(e.target.value)}
              placeholder={'e.g.\nLead with decisions; list action items with owners.\nUse plain English; avoid jargon and acronyms.'}
            />
          </Field>

          <Field label="Vocabulary (names & terms, optional)">
            <textarea
              data-testid="tag-vocab"
              className="summary-edit"
              style={{ minHeight: 100, width: '100%', maxWidth: 480 }}
              value={vocabText}
              onChange={(e) => setVocabText(e.target.value)}
              placeholder={'Northwind Logistics, NWL, Priya Okonkwo, Project Aurora'}
            />
            <p className="meta" style={{ fontSize: 'var(--t-meta)', marginTop: 4 }}>
              Sent to the transcriber so it spells these correctly. Comma or line-separated.
            </p>
          </Field>

          {err && <p className="meta meta--danger" style={{ fontSize: 'var(--t-small)' }}>{err}</p>}
          {savedFlash && (
            <p role="status" className="saved-flash">
              ✓ {savedFlash}
            </p>
          )}

          <div style={{ display: 'flex', gap: 10, marginTop: 12, flexWrap: 'wrap' }}>
            <button data-testid="tag-save" className="btn btn--primary" disabled={!canSave} onClick={save}>{busy ? 'Saving…' : 'Save'}</button>
            <button data-testid="tag-cancel" className="btn" disabled={busy} onClick={cancel}>Cancel</button>
            {sel.kind === 'tag' && (
              <button data-testid="tag-delete" className="btn btn--danger" disabled={busy} onClick={openDeleteConfirm}>Delete tag</button>
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
