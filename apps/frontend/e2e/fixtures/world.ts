// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
// One fictional backend for browser mode. `install.ts` bundles this module with
// esbuild and runs it in the page, so everything here is browser-only code — no
// Node imports.
//
// A handler serves the state it keeps; `install.ts` clones every reply at the
// dispatch boundary, so a handler never has to copy what it returns.
import type {
  AiJob, AiJobKind,
  AnalysisResult, AsrModelInfo, AudioSourceInfo, BootstrapStatus, CalendarEvent,
  CalendarRefreshResult, CalendarSubscription, CallChat, Contact,
  DownloadFinished, DownloadJob, FinalizeStatus, IntegrationHistoryEntry, IntegrationPublic, LicenseStatus,
  BuiltinRow, BuiltinStatus, MachinePatch, MachineSettingsStatus, McpStatus, MeetingDetectionSupport, OnboardingPlan,
  Prompt, ProviderId, ProviderListEntry, QaAnswer, QueuedWorkflowRun, RecordingSnapshot,
  RecordingsStats, ReminderPayload, SessionChapters, SessionHit, SessionListEntry,
  SessionMeta, SessionSpeaker, SessionSuggestions, SessionSummary, SessionView,
  Settings, SettingsPatch, SpeechLevelInfo, SyncStatus, Tag, UndiarizedNames, UpdateInfo, VaultTrust, VoiceprintView, Workflow,
  WorkflowRunRecord,
} from '../../src/tauri.ts';

export type WorldData = ReturnType<typeof baseData>;
export type World = { data: WorldData; handlers: Record<string, (args: any) => unknown> };

/** The AI provider status with no provider selected — the string
 *  `commands::summary::provider_status` returns. */
const NO_PROVIDER_HINT =
  'No AI provider selected. Set one up in Settings → Providers — or use the Copy buttons to paste into an LLM yourself.';

/** Delivers a Tauri event to the page's listeners, a copy of the payload as
 *  the native IPC's serialization gives. The browser harness installs the
 *  dispatcher; the readability sweep runs without one. */
function emitEvent(event: string, payload: unknown): void {
  const w = window as unknown as { __wdio_emit_tauri_event__?: (e: string, p: unknown) => void };
  w.__wdio_emit_tauri_event__?.(event, structuredClone(payload));
}

const sleep = (ms: number) => new Promise<void>((resolve) => { setTimeout(resolve, ms); });

export function baseData() {
  const now = Math.floor(Date.now() / 1000);
  const day = 86400;
  const profileDir = '/Users/alex/Library/Application Support/daisy';

  const phase = { eula: true, consent: true, vaultExists: true, unlocked: true };

  // 'foreign' routes App.tsx to the profile-blocked gate.
  const binding: { state: 'ok' | 'foreign' } = { state: 'ok' };

  const license: LicenseStatus = {
    state: 'licensed', name: 'Alex Example', email: 'alex@example.com',
    key: 'DAISY-0000-0000-0000', expires: null, subscription_type: 'annual', entitlements: [],
  };

  const sessions: SessionListEntry[] = [
    { session_id: 's-001', created_at_unix_seconds: now - 2 * 3600, finalized_at_unix_seconds: now - 3600, duration_seconds: 2712, title: 'Product sync — Q3 roadmap', tag_ids: ['t-1'], has_transcript: true, has_dedup: true, has_summary: true },
    { session_id: 's-002', created_at_unix_seconds: now - day, finalized_at_unix_seconds: now - day + 1800, duration_seconds: 1805, title: 'Design review: onboarding flow', tag_ids: [], has_transcript: true, has_dedup: true, has_summary: true },
    { session_id: 's-003', created_at_unix_seconds: now - 2 * day, finalized_at_unix_seconds: now - 2 * day + 3600, duration_seconds: 3598, title: 'Vendor call — Northwind Analytics', tag_ids: ['t-2'], has_transcript: true, has_dedup: false, has_summary: false },
    { session_id: 's-004', created_at_unix_seconds: now - 4 * day, finalized_at_unix_seconds: now - 4 * day + 900, duration_seconds: 912, title: 'Standup', tag_ids: [], has_transcript: true, has_dedup: true, has_summary: true, recording_kind: 'phone_room' },
    { session_id: 's-005', created_at_unix_seconds: now - 6 * day, finalized_at_unix_seconds: null, duration_seconds: 1500, title: null, tag_ids: [], has_transcript: false, has_dedup: false, has_summary: false, interrupted: true },
  ];

  const tags: Tag[] = [
    { id: 't-1', name: 'roadmap', color_hex: '#c47a1f', prompt_md: null, vocab_md: null, created_at_unix_seconds: now - 30 * day, use_count: 1 },
    { id: 't-2', name: 'vendor', color_hex: '#2f6f9f', prompt_md: null, vocab_md: 'Northwind\nMSA', created_at_unix_seconds: now - 30 * day, use_count: 1 },
    { id: 't-3', name: 'standup', color_hex: '#4c8a3c', prompt_md: null, vocab_md: null, created_at_unix_seconds: now - 30 * day, use_count: 0 },
  ];

  const prompts: Prompt[] = [
    { id: 'builtin:daisy', name: 'Daisy Summarizer', output: 'classic', directive_md: 'Standard meeting summary.', builtin: true },
    { id: 'builtin:zoom', name: 'Zoom-style Meeting Notes', output: 'sectioned', directive_md: 'Executive summary.', builtin: true },
    { id: 'builtin:otter', name: 'Otter-style Meeting Notes', output: 'sectioned', directive_md: 'Chapter summary then outline.', builtin: true },
    { id: 'u1', name: 'My weekly digest', output: 'sectioned', directive_md: 'List the three headlines.', builtin: false },
  ];

  const providers: ProviderListEntry[] = [
    { name: 'anthropic', has_key: true, model: 'model-a', base_url: null, context_tokens: null },
  ];

  const settings: Settings = {
    schema_version: 5,
    default_summary_provider: 'anthropic',
    default_summary_prompt_id: null,
    user_display_name: 'Alex',
    nav_order: [],
    auto_update_check: false,
    reminder_lead_seconds: 60,
    voice_match_tolerance: 65,
  };

  const machine: MachineSettingsStatus = {
    machine: 'e2e-machine',
    settings: {
      default_mic_key: 'usb-mic',
      default_mic_source_id: 1,
      aec_mode_override: 'auto',
      denoise_enabled: false,
      debug_level: 'off',
      ui_zoom: 1,
      css_theme: null,
      appearance: 'system',
      show_tray_icon: true,
      minimize_on_close: null,
      mcp_enabled: false,
      mcp_port: 8765,
      mcp_allow_write: false,
      nerd_mode: true,
      show_live_captions: true,
      live_speaker_names: true,
      asr: { active: 'nemotron-speech-streaming-en-0.6b-560ms-int8' },
      meeting_prompt: { enabled: true, snooze_until: null, stop_enabled: true },
      record_hotkey: 'Control+Alt+R',
    },
  };

  const audioSources: AudioSourceInfo[] = [
    { id: 1, kind: 'mic', node_name: 'usb-mic', description: 'USB Microphone' },
    { id: 2, kind: 'monitor', node_name: 'monitor', description: 'System audio (monitor)' },
  ];

  const workflows: Workflow[] = [
    {
      id: 'w-1', name: 'Client A design specs', enabled: true, trigger: 'finalized',
      condition: { type: 'all', children: [{ type: 'has_tag', tag_id: 't-1' }] },
      actions: [{ type: 'run_prompt', prompt_id: 'u1', send_to_integration: 'i-1', write_to_dir: null }],
      created_at_unix_seconds: now - day,
    },
    {
      id: 'w-2', name: 'Notify ops on failures', enabled: false, trigger: 'finalize_failed',
      condition: { type: 'all', children: [] },
      actions: [{ type: 'push_integration', integration_id: 'i-1' }],
      created_at_unix_seconds: now - 2 * day,
    },
  ];

  const runs: WorkflowRunRecord[] = [
    {
      run_id: 'r-1', at_unix_seconds: now - 3600, workflow_id: 'w-1', workflow_name: 'Client A design specs',
      session_id: 's-001', session_title: 'Product sync — Q3 roadmap', trigger: 'finalized', status: 'ok',
      steps: [{ label: 'My weekly digest', status: 'ok', duration_ms: 1840 }],
    },
  ];

  const queue: QueuedWorkflowRun[] = [];

  const integrations: IntegrationPublic[] = [
    {
      id: 'i-1', name: 'Ops webhook', enabled: true, kind: 'webhook',
      webhook_url: 'https://hooks.example.test/ops', auth_kind: 'none', auth_header_name: null,
      payloads: { summary: true, notes: false, transcript: false },
    },
  ];

  const integrationRuns: IntegrationHistoryEntry[] = [];

  const contacts: Contact[] = [
    { id: 'c-1', display_name: 'Jordan Rivera', emails: ['jordan@example.com'], created_at_unix_seconds: now - 40 * day },
    { id: 'c-2', display_name: 'Sam Okafor', emails: ['sam@example.com'], created_at_unix_seconds: now - 40 * day },
  ];

  const voiceprints: VoiceprintView[] = [
    { id: 'v-1', display_name: 'Jordan Rivera', email: 'jordan@example.com', created_at_unix_seconds: now - 40 * day, session_count: 3, vector_dim: 192, sample_count: 4 },
  ];

  const calendars: CalendarSubscription[] = [
    { id: 'cal-1', name: 'Work', url: 'https://calendar.example.test/work.ics', enabled: true, color_hex: '#2f6f9f', tag_id: null, dismissed_event_uids: [] },
  ];

  const events: CalendarEvent[] = [
    {
      id: 'ev-1', uid: 'uid-1', subscription_id: 'cal-1', subscription_name: 'Work',
      subscription_color: '#2f6f9f', subscription_tag_id: null, title: 'Weekly planning',
      start_unix_seconds: now + 3600, end_unix_seconds: now + 5400,
      attendees: [{ display_name: 'Jordan Rivera', email: 'jordan@example.com' }],
      location: null, description: null,
    },
  ];

  const speakers: SessionSpeaker[] = [
    { cluster_id: 0, display_name: 'Jordan Rivera', email: 'jordan@example.com', voiceprint_id: 'v-1', match_confidence: 0.82, is_user_labeled: false, sample_text: 'Let us start with the roadmap.', speech_ms: 412_000, side: 'remote', mixed_voices: false },
    { cluster_id: 1, display_name: 'Speaker 2', email: null, voiceprint_id: null, match_confidence: null, is_user_labeled: false, sample_text: 'Agreed, Friday works.', speech_ms: 180_000, side: 'remote', mixed_voices: false },
  ];

  const asrModels: AsrModelInfo[] = [
    { id: 'nemotron-speech-streaming-en-0.6b-560ms-int8', engine: 'nemotron', display: 'Nemotron Speech Streaming 0.6B (int8)', installed: true, active: true, bundled_floor: true, profile_copy: false, multilingual: false, bytes: 704_550_790, license: 'NVIDIA Open Model License', license_url: 'https://www.nvidia.com/en-us/agreements/enterprise-software/nvidia-open-model-license/' },
    { id: 'ggml-base.en.bin', engine: 'whisper', display: 'Whisper base (English)', installed: false, active: false, bundled_floor: false, profile_copy: false, multilingual: false, bytes: 148_000_000, license: 'MIT', license_url: 'https://github.com/openai/whisper/blob/main/LICENSE' },
  ];

  const builtinModel: BuiltinRow = {
    id: 'gemma-4-e4b-it-q4', display: 'Gemma 4 E4B', installed: false, bytes: 5_130_000_000, license: 'Gemma Terms of Use', license_url: 'https://ai.google.dev/gemma/terms', capability: 'fast', capability_line: 'Fast on this machine.', partial_bytes: 0, warming: false, warm_failed: null,
  };

  const sync: SyncStatus = {
    device_id: '3f9c2b1a4d5e6f708192a3b4c5d6e7f8', machine_name: 'studio-mac',
    paired: [{ device_id: 'phone-a1', name: "Rowan's phone", phone_name: "Rowan's phone", platform: 'ios', paired_at_unix: now - 3 * day, last_hello_unix: now - 600, out_of_date: null }],
    window: null,
    host: 'studio-mac.local', port: 7845, listening: true, bluetooth: 'on',
    refused: [{ session: 'daisy-1787000001', device_id: 'phone-a1', reason: 'sealed' }],
    platform: 'macos',
  };

  const mcp: McpStatus = {
    enabled: false, running: false, port: 8765, token: 'e2e-token',
    endpoint: 'http://127.0.0.1:8765/mcp', claude_command: 'claude mcp add daisy http://127.0.0.1:8765/mcp',
  };

  const build = { version: '9.9.9', sha: 'e2etest', tagged: false };

  const update: UpdateInfo = {
    current: '9.9.9', latest: '9.9.9', update_available: false, notes: '', url: null,
  };

  const recordingsStats: RecordingsStats = {
    session_count: 5, wav_bytes: 1_200_000_000, opus_bytes: 42_000_000,
    deletable_session_count: 3, deletable_bytes: 900_000_000,
  };

  // Keyed by the source's description — the key Settings looks a level up by.
  const speechLevels: SpeechLevelInfo[] = [
    { device: 'USB Microphone', effective_dbfs: -31.5, source: 'learned', samples: 12, override_dbfs: null },
  ];

  const summaryMarkdown = [
    '## TL;DR',
    'The team agreed the Q3 plan and picked a launch window.',
    '',
    '## Action items',
    '- Alex: send the roadmap by Friday',
    '- Jordan Rivera: book the vendor follow-up',
  ].join('\n');

  // Shaped like render_markdown: every paragraph carries its start time.
  const transcriptMd = [
    '[00:00:04] **Jordan Rivera**: Let us start with the roadmap.',
    '',
    '[00:00:19] **Alex**: The two big items are onboarding and the vendor migration.',
  ].join('\n');

  const summaries: Record<string, SessionSummary> = {};
  const notes: Record<string, string> = { 's-001': 'Bring the pricing sheet.' };
  const chapters: Record<string, SessionChapters> = {};
  const analyses: Record<string, AnalysisResult> = {};
  const chats: Record<string, CallChat> = {};
  const finalizeStatus: Record<string, FinalizeStatus> = {};

  // What the matcher would have found: `s-002` draws a vocabulary hit, `s-004`
  // the tags its predecessor carried. Every other session draws nothing.
  const suggestions: Record<string, SessionSuggestions> = {
    's-002': {
      last_time: null,
      vocab: [{ tag_id: 't-2', total_hits: 3, evidence: [{ term: 'Northwind', count: 2 }, { term: 'MSA', count: 1 }] }],
    },
    's-004': {
      last_time: { source_session_id: 's-001', tag_ids: ['t-1', 't-3'] },
      vocab: [],
    },
  };
  const dismissedSuggestions: Record<string, string[]> = {};

  return {
    /** Seeded from `E2E_PASSPHRASE` by `installWorld`. */
    passphrase: '',
    phase, binding, profileDir,
    /** How `vault_trust` answers. Defaults to a passphrase-mode vault opened
     *  on this machine; a spec sets `kind: 'machine'` with a `trusted_on`
     *  other than `this_machine` to land on the unlock screen's
     *  machine-mode-elsewhere branch. */
    vaultTrust: { kind: 'passphrase', trusted_on: null, this_machine: sync.machine_name } as VaultTrust,
    // LicenseStatus union so overrides can set any state.
    license: license as LicenseStatus,
    /** Set to make `register_gateway` reject with this message. */
    gatewayError: null as string | null,
    sessions, tags, prompts, providers, settings, machine, audioSources,
    workflows, runs, queue, integrations, integrationRuns,
    contacts, voiceprints, calendars, events, speakers, asrModels, builtinModel, mcp, sync,
    build, update, recordingsStats, speechLevels,
    summaryMarkdown, transcriptMd, summaries, notes, chapters, analyses, chats,
    finalizeStatus, suggestions, dismissedSuggestions,
    /** What `read_live_names` answers: the overlay of a stopped session whose
     *  transcript has no speakers yet, or null once speakers exist. */
    liveNames: null as UndiarizedNames | null,
    recording: null as RecordingSnapshot | null,
    /** What `onboarding_plan` answers; `onboarding_apply` writes it into the
     *  machine settings and returns it. */
    onboardingPlan: {
      ai: { kind: 'builtin', capability: 'fast', installed: true },
      transcription: { engine: 'nemotron', installed: true },
      profile_dir: profileDir,
    } as OnboardingPlan,
    reminderTitle: null as string | null,
    /** Which face the reminder popup shows. */
    reminderKind: 'calendar' as 'calendar' | 'meeting' | 'meeting_ended',
    /** The app the meeting-ended face names. */
    reminderApp: 'Fictional Call',
    /** The choices `reminder_action` has been called with, in order. */
    reminderChoices: [] as string[],
    meetingSupport: { supported: true, reason: null } as MeetingDetectionSupport,
    /** Hold the built-in model's download in this phase (or its warm-up,
     *  once installed) until it is cancelled or deleted (or ten seconds
     *  pass), so a spec can drive the progress, retrying, warming and Cancel
     *  states. `fails` ends the download failed. */
    llmDownloadHold: null as 'downloading' | 'retrying' | 'warming' | 'fails' | 'wont-start' | null,
    /** The Rust download queue, as `download_queue_status` answers it. */
    downloadQueue: [] as DownloadJob[],
    /** Hold the AI task (summary or chapters) in the registry until a spec
     *  clears the flag (or ten seconds pass), so a spec can drive the gated
     *  buttons and the survives-navigation state. */
    aiJobHold: false,
    /** The Rust AI-job registry, as `ai_jobs_list` answers it. */
    aiJobs: [] as AiJob[],
  };
}

export function makeWorld(overrides: Partial<WorldData> = {}): World {
  const d: WorldData = { ...baseData(), ...overrides };
  const stamp = () => Math.floor(Date.now() / 1000);
  /** Ids `download_queue_cancel` has been called with. */
  const queueCancelled = new Set<string>();
  const pushQueue = () => { emitEvent('download-queue:changed', d.downloadQueue); };
  const pushAiJobs = () => { emitEvent('ai-jobs:changed', d.aiJobs); };
  /** Every settings write, from any command, is announced with the result. */
  const pushMachine = () => { emitEvent('machine-settings:changed', d.machine); };
  const pushSettings = () => { emitEvent('settings:changed', d.settings); };
  const pushModels = () => { emitEvent('models:changed', d.asrModels); };
  const finished = (job: DownloadFinished) => { emitEvent('download:finished', job); };
  /** Runs `work` with its entry in the AI-job registry, as the backend's
   *  one-task-at-a-time guard does. */
  const withAiJob = async <T>(sessionId: string, kind: AiJobKind, work: () => T): Promise<T> => {
    d.aiJobs = [...d.aiJobs, { session_id: sessionId, kind, started_at_unix_seconds: stamp() }];
    pushAiJobs();
    try {
      const deadline = Date.now() + 10_000;
      while (d.aiJobHold && Date.now() < deadline) await sleep(50);
      return work();
    } finally {
      d.aiJobs = d.aiJobs.filter((j) => j.session_id !== sessionId || j.kind !== kind);
      pushAiJobs();
    }
  };
  const setJob = (job: DownloadJob) => {
    const i = d.downloadQueue.findIndex((j) => j.id === job.id);
    if (i >= 0) d.downloadQueue[i] = job; else d.downloadQueue.push(job);
    pushQueue();
  };
  /** Takes a job off the queue; `outcome` announces how it ended. */
  const dropJob = (id: string, outcome?: DownloadFinished['outcome']) => {
    const job = d.downloadQueue.find((j) => j.id === id);
    d.downloadQueue = d.downloadQueue.filter((j) => j.id !== id);
    pushQueue();
    if (job && outcome) finished({ id, kind: job.kind, outcome });
  };
  /** Waits in `phase` while the world holds it; true when a cancel arrived. */
  const heldIn = async (id: string, phase: 'downloading' | 'retrying' | 'warming') => {
    if (d.llmDownloadHold !== phase) return false;
    const deadline = Date.now() + 10_000;
    while (!queueCancelled.has(id) && Date.now() < deadline) await sleep(50);
    return queueCancelled.delete(id);
  };
  const runLlmJob = async (id: string) => {
    queueCancelled.delete(id);
    const job = (phase: DownloadJob['phase']): DownloadJob =>
      ({ kind: 'llm', id, indeterminate: false, phase });
    const m = d.builtinModel;
    // Steps through a distinct pct before the held value.
    setJob(job({ state: 'downloading', pct: 0.1 }));
    pushBuiltin();
    await sleep(0);
    setJob(job({ state: 'downloading', pct: 0.42 }));
    if (await heldIn(id, 'downloading')) return;
    // An attempt failed; the entry waits out its delay before the next one.
    if (d.llmDownloadHold === 'retrying') {
      setJob(job({ state: 'retrying', attempt: 2, of: 5, error: 'timed out' }));
      if (await heldIn(id, 'retrying')) return;
    }
    if (d.llmDownloadHold === 'fails') {
      setJob(job({ state: 'failed', error: 'no route to host' }));
      finished({ id, kind: 'llm', outcome: 'failed' });
      pushBuiltin();
      return;
    }
    // The job ends at installed; the first install becomes the provider when
    // none is chosen, then the model warms up outside the queue.
    m.installed = true;
    m.partial_bytes = 0;
    m.warm_failed = null;
    const autoSelected = aiProvider() == null && m.capability === 'fast';
    if (autoSelected) { d.machine.settings.ai_provider = 'local'; pushMachine(); }
    if (d.onboardingPlan.ai.kind === 'builtin') d.onboardingPlan.ai.installed = true;
    dropJob(id, 'installed');
    pushBuiltin();
    // A worker that will not start: the install's own selection is taken
    // back and the row carries the error.
    if (d.llmDownloadHold === 'wont-start') {
      m.warm_failed = 'the worker exited before it replied';
      if (autoSelected && aiProvider() === 'local') { d.machine.settings.ai_provider = null; pushMachine(); }
      pushBuiltin();
      return;
    }
    if (d.llmDownloadHold !== 'warming') return;
    m.warming = true;
    pushBuiltin();
    await heldIn(id, 'warming');
    m.warming = false;
    pushBuiltin();
  };
  /** The built-in model's delete: the download, the warm-up, the weights and
   *  a `local` choice all go. */
  const deleteBuiltin = () => {
    const id = d.builtinModel.id;
    queueCancelled.add(id);
    dropJob(id, 'cancelled');
    d.builtinModel.installed = false;
    d.builtinModel.partial_bytes = 0;
    d.builtinModel.warming = false;
    d.builtinModel.warm_failed = null;
    if (d.onboardingPlan.ai.kind === 'builtin') d.onboardingPlan.ai.installed = false;
    if (aiProvider() === 'local') { d.machine.settings.ai_provider = null; pushMachine(); }
    pushBuiltin();
  };
  const runAsrJob = async (kind: 'asr' | 'sa-assets', id: string) => {
    queueCancelled.delete(id);
    setJob({ kind, id, indeterminate: kind === 'sa-assets', phase: { state: 'downloading', pct: 0.42 } });
    await sleep(0);
    // Check the cancel before the model is marked installed.
    if (queueCancelled.delete(id)) return;
    const m = d.asrModels.find((x) => x.id === id);
    if (m) { m.installed = true; m.profile_copy = true; }
    if (kind === 'asr' && d.onboardingPlan.transcription.engine === 'nemotron') {
      d.onboardingPlan.transcription.installed = true;
    } else if (kind === 'sa-assets' && d.onboardingPlan.transcription.engine === 'speech_analyzer') {
      d.onboardingPlan.transcription.ready = true;
    }
    dropJob(id, 'installed');
    pushModels();
  };
  /** This machine's AI provider: its own choice when it has one, else the profile's. */
  const aiProvider = () => (d.machine.settings.ai_provider !== undefined ? d.machine.settings.ai_provider : d.settings.default_summary_provider);
  const session = (id: string) => d.sessions.find((s) => s.session_id === id) ?? null;
  /** What `builtin_model_status` answers and `builtin-model:changed` carries. */
  const builtinStatus = (): BuiltinStatus => {
    const m = d.builtinModel;
    const p = aiProvider();
    const installing = d.downloadQueue.some((j) => j.kind === 'llm' && j.phase.state !== 'failed');
    const provider = ((): BuiltinStatus['provider'] => {
      if (p == null) {
        return installing && d.phase.unlocked
          ? { state: 'Installing', provider: 'local', hint: "Daisy's model is still downloading." }
          : { state: 'None', provider: null, hint: NO_PROVIDER_HINT };
      }
      if (!d.phase.unlocked) return { state: 'VaultLocked', provider: p, hint: 'Unlock the vault to use AI.' };
      if (p === 'local') {
        if (m.capability === 'unsupported') return { state: 'Unsupported', provider: p, hint: m.capability_line };
        if (m.installed) return { state: 'Configured', provider: p, hint: null };
        return installing
          ? { state: 'Installing', provider: p, hint: "Daisy's model is still downloading." }
          : { state: 'Missing', provider: p, hint: 'Download the built-in model in Settings → Providers.' };
      }
      return { state: 'Configured', provider: p, hint: null };
    })();
    return { model: { ...m }, provider };
  };
  function pushBuiltin() { emitEvent('builtin-model:changed', builtinStatus()); }

  const summaryFor = (id: string): SessionSummary => d.summaries[id] ?? {
    v: 2, session_id: id, produced_by: { device: 'e2e', provider: 'anthropic', model: 'model-a' },
    generated_at_unix_seconds: stamp(), source_inputs_hash: 'e2ehash',
    tldr: 'The team agreed the Q3 plan and picked a launch window.',
    action_items: [{ text: 'Send the roadmap', owner: 'Alex', due: 'Friday' }],
    decisions: ['Launch in week 3 of Q3'],
    open_questions: ['Who owns the vendor migration?'],
    key_topics: ['roadmap', 'onboarding'],
    chapters: [],
    markdown: d.summaryMarkdown,
    user_edited: false,
    skipped: [],
    turn_count: 3,
  };

  const dismiss = (sessionId: string, tagIds: string[]) => {
    const held = d.dismissedSuggestions[sessionId] ?? [];
    d.dismissedSuggestions[sessionId] = [...new Set([...held, ...tagIds])];
  };

  /** Seeded suggestions minus what this session has dismissed. */
  const visibleSuggestions = (id: string): SessionSuggestions => {
    const held = d.suggestions[id] ?? { last_time: null, vocab: [] };
    const dropped = new Set(d.dismissedSuggestions[id] ?? []);
    const lastTimeIds = (held.last_time?.tag_ids ?? []).filter((t) => !dropped.has(t));
    return {
      last_time: held.last_time && lastTimeIds.length > 0
        ? { ...held.last_time, tag_ids: lastTimeIds }
        : null,
      vocab: held.vocab.filter((v) => !dropped.has(v.tag_id)),
    };
  };

  const metaFor = (id: string): SessionMeta => {
    const s = session(id);
    return {
      session_id: id,
      title: s?.title ?? null,
      tag_ids: s?.tag_ids ?? [],
      attendees: [{ display_name: 'Alex', role: 'self' }, { display_name: 'Jordan Rivera', role: 'other' }],
      has_notes: typeof d.notes[id] === 'string',
      has_summary: s?.has_summary ?? false,
      recording_segments: 3,
      sent_integration_ids: [],
      recording_kind: s?.recording_kind ?? null,
    };
  };

  const view = (id: string): SessionView => {
    const s = session(id);
    return {
      session_id: id,
      // The detail header reads the recording date and the chunk durations off
      // the manifest, and the date is editable in place.
      manifest_json: {
        session_id: id,
        title: s?.title ?? null,
        created_at_unix_seconds: s?.created_at_unix_seconds ?? stamp(),
        chunks: [{ duration_seconds: s?.duration_seconds ?? 0 }],
        recording_kind: s?.recording_kind ?? null,
      },
      transcript_md: s?.has_transcript ? d.transcriptMd : null,
      has_transcript: s?.has_transcript ?? false,
      has_dedup: s?.has_dedup ?? false,
      has_summary: s?.has_summary ?? false,
    };
  };

  const newSession = (title: string | null, recording_kind: SessionListEntry['recording_kind'] = null): string => {
    const id = `s-${d.sessions.length + 100}`;
    d.sessions.unshift({
      session_id: id, created_at_unix_seconds: stamp(), finalized_at_unix_seconds: stamp(),
      duration_seconds: 0, title, tag_ids: [], has_transcript: false, has_dedup: false, has_summary: false, recording_kind,
    });
    return id;
  };

  const learnedDbfs = new Map<string, number>();

  const handlers: World['handlers'] = {
    // --- gates, vault, license ---
    eula_status: () => d.phase.eula,
    accept_eula: () => { d.phase.eula = true; return null; },
    consent_status: () => d.phase.consent,
    accept_consent: () => { d.phase.consent = true; return null; },
    bootstrap_status: (): BootstrapStatus => ({
      has_bootstrap: d.phase.vaultExists, profile_dir: d.phase.vaultExists ? d.profileDir : null,
      platform_default: d.profileDir, env_override: null,
    }),
    bootstrap_set: () => null,
    onboarding_plan: (): OnboardingPlan => d.onboardingPlan,
    onboarding_apply: (): OnboardingPlan => {
      const { ai, transcription } = d.onboardingPlan;
      if (ai.kind === 'builtin') {
        // A model still to download is selected when its install lands.
        if (ai.installed) d.machine.settings.ai_provider = 'local';
        else if (!d.downloadQueue.some((j) => j.id === 'gemma-4-e4b-it-q4')) {
          void runLlmJob('gemma-4-e4b-it-q4');
        }
      }
      const active = transcription.engine === 'nemotron' ? 'nemotron-speech-streaming-en-0.6b-560ms-int8' : 'speechanalyzer';
      d.machine.settings.asr.active = active;
      for (const m of d.asrModels) m.active = m.id === active;
      pushMachine();
      pushModels();
      if (transcription.engine === 'nemotron' && !transcription.installed
          && !d.downloadQueue.some((j) => j.id === 'nemotron-speech-streaming-en-0.6b-560ms-int8')) {
        void runAsrJob('asr', 'nemotron-speech-streaming-en-0.6b-560ms-int8');
      } else if (transcription.engine === 'speech_analyzer' && !transcription.ready
          && !d.downloadQueue.some((j) => j.id === 'speechanalyzer')) {
        void runAsrJob('sa-assets', 'speechanalyzer');
      }
      return d.onboardingPlan;
    },
    onboarding_finish: () => null,
    env_profile_dir: () => null,
    probe_profile_dir: () => ({ is_current: false, has_profile: false, empty: true }),
    profile_binding_check: () => d.binding,
    vault_status: () => ({ vault_exists: d.phase.vaultExists, unlocked: d.phase.unlocked }),
    vault_kind: () => 'passphrase',
    vault_trust: () => d.vaultTrust,
    init_vault_machine_mode: () => { d.phase.vaultExists = true; d.phase.unlocked = true; pushBuiltin(); return null; },
    unlock_vault: ({ passphrase }: { passphrase: string }) => {
      if (passphrase !== d.passphrase) {
        throw { kind: 'vault', code: 'E_VAULT_PASSPHRASE', message: 'wrong passphrase', friendly: 'Wrong passphrase.' };
      }
      d.phase.unlocked = true;
      pushBuiltin();
      return null;
    },
    lock_vault: () => { d.phase.unlocked = false; pushBuiltin(); return null; },
    reset_vault: () => { d.phase.vaultExists = false; d.phase.unlocked = false; pushBuiltin(); return null; },
    change_vault_passphrase: ({ newPassphrase }: { newPassphrase: string }) => { d.passphrase = newPassphrase; return null; },
    switch_vault_mode: ({ newPassphrase }: { newPassphrase: string | null }) => (newPassphrase === null ? 'machine' : 'passphrase'),
    license_status: () => d.license,
    license_checkin: () => d.license,
    activate_license: (): LicenseStatus => {
      d.license = {
        state: 'licensed', name: 'Alex Example', email: 'alex@example.com',
        key: 'DAISY-0000-0000-0000', expires: null, subscription_type: 'annual', entitlements: [],
      };
      return d.license;
    },
    deactivate_license: () => ({ state: 'trial', days_left: 14 }),
    register_gateway: () => {
      if (d.gatewayError) {
        throw {
          kind: 'config', code: 'E_CONFIG',
          message: d.gatewayError, friendly: d.gatewayError,
        };
      }
      return null;
    },

    // --- settings, app shell ---
    read_settings: () => d.settings,
    update_settings: ({ patch }: { patch: SettingsPatch }): Settings => {
      d.settings = { ...d.settings, ...patch };
      pushSettings();
      return d.settings;
    },
    machine_settings: () => d.machine,
    meeting_detection_support: () => d.meetingSupport,
    default_recording_title: () => '3:42 PM',
    set_clock_hour12: () => null,
    // The stub browser is not macOS; the media query answers there.
    reduce_transparency: () => false,
    update_machine_settings: ({ patch }: { patch: MachinePatch }): MachineSettingsStatus => {
      const asr = patch.asr ? { ...patch.asr, active: d.machine.settings.asr.active } : d.machine.settings.asr;
      d.machine = { machine: d.machine.machine, settings: { ...d.machine.settings, ...patch, asr } };
      pushMachine();
      return d.machine;
    },
    set_ai_provider: ({ provider }: { provider: ProviderId | null }) => {
      if (provider === 'local' && d.builtinModel.capability === 'unsupported') {
        throw { kind: 'config', code: 'E_CONFIG', message: "This computer can't run the built-in model.", friendly: "This computer can't run the built-in model." };
      }
      if (provider === 'local' && !d.builtinModel.installed) {
        throw { kind: 'config', code: 'E_CONFIG', message: "Built-in model isn't downloaded.", friendly: "Built-in model isn't downloaded." };
      }
      d.machine.settings.ai_provider = provider;
      pushMachine();
      // Picking the model by hand warms it again; here that start works.
      if (provider === 'local') d.builtinModel.warm_failed = null;
      pushBuiltin();
      return null;
    },
    build_info: () => d.build,
    check_for_update: () => d.update,
    list_css_themes: () => ['default'],
    read_css_theme: () => ':root { --e2e: 1; }',
    open_external: () => null,
    open_logs_dir: () => `${d.profileDir}/logs`,
    open_profile_dir: () => d.profileDir,
    move_profile: () => null,
    switch_profile: () => null,
    builtin_model_status: (): BuiltinStatus => builtinStatus(),
    sync_status: () => d.sync,
    linux_firewall_hint: () => null,
    sync_pairing_begin: () => {
      // The desktop's pairing code: daisy-pair:2:<16 chars>@<host>:<port>.
      const keyText = '7K2M-QX9D-4HNB-R8TC';
      const host = 'studio-mac.local';
      const port = 7845;
      const expires_unix = Math.floor(Date.now() / 1000) + 600;
      d.sync.window = {
        qr_text: `daisy-pair:2:${keyText.replace(/-/g, '')}@${host}:${port}`,
        key_text: keyText,
        expires_unix,
      };
      return d.sync.window;
    },
    sync_pairing_cancel: () => { d.sync.window = null; return null; },
    sync_rename_device: ({ deviceId, name }: { deviceId: string; name: string }) => {
      const phone = d.sync.paired.find((p) => p.device_id === deviceId);
      if (!phone) throw new Error('That phone is no longer paired with this computer.');
      const trimmed = name.trim().slice(0, 80);
      phone.name = trimmed || phone.phone_name;
      return d.sync;
    },
    sync_unpair: ({ deviceId }: { deviceId: string }) => {
      d.sync.paired = d.sync.paired.filter((p) => p.device_id !== deviceId);
      d.sync.refused = d.sync.refused.filter((r) => r.device_id !== deviceId);
      return d.sync;
    },
    mcp_status: () => d.mcp,
    mcp_apply: () => d.mcp,
    mcp_regenerate_token: () => d.mcp,
    mcp_port_available: () => true,

    // --- library ---
    list_sessions: () => d.sessions,
    read_session: ({ sessionId }: { sessionId: string }) => view(sessionId),
    session_meta_get: ({ sessionId }: { sessionId: string }) => metaFor(sessionId),
    session_meta_update: ({ req }: { req: { session_id: string; title?: string | null; tag_ids?: string[]; created_at_unix_seconds?: number } }) => {
      const s = session(req.session_id);
      if (s && req.title !== undefined) s.title = req.title;
      if (s && req.tag_ids !== undefined) s.tag_ids = req.tag_ids;
      if (s && req.created_at_unix_seconds !== undefined) s.created_at_unix_seconds = req.created_at_unix_seconds;
      return null;
    },
    delete_session: ({ sessionId }: { sessionId: string }) => {
      d.sessions = d.sessions.filter((s) => s.session_id !== sessionId);
      return null;
    },
    session_assign_tags: ({ sessionId, tagIds }: { sessionId: string; tagIds: string[] }) => {
      const s = session(sessionId);
      if (s) s.tag_ids = tagIds;
      return null;
    },

    // --- tag suggestions ---
    tag_suggest_for_session: ({ sessionId }: { sessionId: string }) => visibleSuggestions(sessionId),
    tag_suggest_prerecord: ({ title }: { title: string | null }) =>
      ((title ?? '').toLowerCase().includes('standup')
        ? { source_session_id: 's-004', tag_ids: ['t-3'] }
        : null),
    session_dismiss_tag_suggestions: ({ sessionId, tagIds }: { sessionId: string; tagIds: string[] }) => {
      dismiss(sessionId, tagIds);
      return null;
    },
    session_undismiss_tag_suggestions: ({ sessionId, tagIds }: { sessionId: string; tagIds: string[] }) => {
      const held = d.dismissedSuggestions[sessionId] ?? [];
      d.dismissedSuggestions[sessionId] = held.filter((id) => !tagIds.includes(id));
      return null;
    },
    session_apply_tag_suggestions: (
      { sessionId, tagIds, dismissTagIds }: { sessionId: string; tagIds: string[]; dismissTagIds: string[] },
    ) => {
      const s = session(sessionId);
      if (s) s.tag_ids = tagIds;
      dismiss(sessionId, dismissTagIds);
      return null;
    },
    session_notes_load: ({ sessionId }: { sessionId: string }) => d.notes[sessionId] ?? '',
    session_notes_save: ({ sessionId, markdown }: { sessionId: string; markdown: string }) => {
      d.notes[sessionId] = markdown;
      return null;
    },
    // No fixture audio in the world; the player stays hidden.
    session_has_playback_audio: () => false,
    session_playback_audio_bytes: () => new ArrayBuffer(0),
    session_playback_audio_url: () => null,
    search_sessions: ({ req }: { req: { query?: string } }): SessionHit[] => {
      const q = (req?.query ?? '').toLowerCase();
      // Like the backend: a title match snippets the title, and a transcript
      // match snippets the line the word appears on.
      const line = (s: SessionListEntry) => (q && s.has_transcript
        ? d.transcriptMd.split('\n').find((l) => l.toLowerCase().includes(q))
        : undefined);
      return d.sessions
        .filter((s) => (s.title ?? '').toLowerCase().includes(q) || line(s))
        .map((s) => {
          const matches: SessionHit['matches'] = [];
          if ((s.title ?? '').toLowerCase().includes(q)) {
            matches.push({ source: 'title', snippet: s.title ?? '' });
          }
          const hit = line(s);
          if (hit) matches.push({ source: 'transcript', snippet: hit.replace(/\*\*/g, '') });
          return {
            session_id: s.session_id, title: s.title, created_at_unix_seconds: s.created_at_unix_seconds,
            tag_ids: s.tag_ids, snippet: matches[0]?.snippet ?? s.title,
            match_source: matches[0]?.source ?? 'title',
            matches,
          };
        });
    },
    create_note_session: ({ req }: { req: { title?: string | null; notes_md: string } }) => {
      const id = newSession(req?.title ?? null);
      d.notes[id] = req?.notes_md ?? '';
      return id;
    },
    import_audio_meeting: ({ req }: { req: { title?: string | null } }) => ({
      session_id: newSession(req?.title ?? 'Imported meeting', 'imported_file'),
      quality_ok: true, quality_note: '', duration_secs: 30,
    }),
    split_session_at: () => ({ new_session_id: newSession('Split part 2'), chunks_moved: 2, chunks_kept: 4 }),
    truncate_session_after: () => ({ chunks_removed: 2, chunks_kept: 4, duration_seconds: 1200 }),
    repair_session: () => 0,
    mark_session_complete: ({ sessionId }: { sessionId: string }) => {
      const s = session(sessionId);
      if (s) { s.finalized_at_unix_seconds = stamp(); s.interrupted = false; }
      return null;
    },
    save_text_file: () => null,
    pick_import_audio: () => null,
    pick_directory: () => null,

    // --- summary, transcript, analysis, chat ---
    summary_load: ({ sessionId }: { sessionId: string }) => (session(sessionId)?.has_summary ? summaryFor(sessionId) : null),
    summary_save_edit: ({ sessionId, markdown }: { sessionId: string; markdown: string }) => {
      const s = session(sessionId);
      const prior = s?.has_summary
        ? summaryFor(sessionId)
        : {
            ...summaryFor(sessionId), produced_by: { device: 'e2e', provider: 'user', model: '' }, source_inputs_hash: '',
            tldr: '', action_items: [], decisions: [], open_questions: [], key_topics: [],
          };
      d.summaries[sessionId] = { ...prior, markdown, user_edited: true };
      if (s) s.has_summary = true;
      return null;
    },
    ai_jobs_list: () => d.aiJobs.map((j) => ({ ...j })),
    summary_regenerate: ({ sessionId }: { sessionId: string }) =>
      withAiJob(sessionId, 'summary', () => {
        const s = session(sessionId);
        if (s) s.has_summary = true;
        return summaryFor(sessionId);
      }),
    load_session_chapters: ({ sessionId }: { sessionId: string }) => d.chapters[sessionId] ?? null,
    extract_session_chapters: ({ req }: { req: { session_id: string } }) =>
      withAiJob(req.session_id, 'chapters', () => {
        const chapters = [
          { title: 'Opening', start_hms: '00:00:00', summary: 'Agenda and attendance.' },
          { title: 'Roadmap', start_hms: '00:04:10', summary: 'Q3 priorities.' },
        ];
        d.chapters[req.session_id] = {
          schema_version: 1, session_id: req.session_id, model: 'model-a',
          generated_at_unix_seconds: stamp(), chapters,
        };
        return { chapters, skipped: false, reason: null };
      }),
    read_live_transcript: () => [
      { track: 'system', start_ms: 0, end_ms: 4200, text: 'Let us start with the roadmap.' },
      { track: 'mic', start_ms: 4200, end_ms: 9000, text: 'The two big items are onboarding and the vendor migration.' },
    ],
    read_finalize_status: ({ sessionId }: { sessionId: string }) => d.finalizeStatus[sessionId] ?? null,
    run_analysis: ({ req }: { req: { session_id: string; prompt_id?: string } }): AnalysisResult => {
      const result = {
        prompt_id: req?.prompt_id ?? 'u1', prompt_name: 'My weekly digest',
        markdown: '## Headlines\n- Roadmap agreed\n- Vendor call booked',
        generated_at_unix_seconds: stamp(),
      };
      d.analyses[`${req.session_id}:${result.prompt_id}`] = result;
      return result;
    },
    analysis_load: ({ sessionId, promptId }: { sessionId: string; promptId: string }) => d.analyses[`${sessionId}:${promptId}`] ?? null,
    qa_ask_stream: ({ req }: { req: { query: string } }): QaAnswer => ({
      query: req?.query ?? '', answer: 'The roadmap is due Friday.', citations: [],
      indexed_sessions: d.sessions.length, total_chunks: 42,
    }),
    live_chat_send_stream: ({ req }: { req: { session_id: string; user_text: string } }) => {
      const chat = d.chats[req.session_id] ?? { messages: [], transcript_cursor_ms: 0 };
      chat.messages.push({ role: 'user', content: req.user_text, ts: stamp() });
      chat.messages.push({ role: 'assistant', content: 'Noted.', ts: stamp() });
      d.chats[req.session_id] = chat;
      return { reply: 'Noted.', chat };
    },
    live_chat_load: ({ sessionId }: { sessionId: string }) => d.chats[sessionId] ?? { messages: [], transcript_cursor_ms: 0 },
    live_chat_delete: ({ sessionId }: { sessionId: string }) => { delete d.chats[sessionId]; return null; },

    // --- speakers, voiceprints ---
    list_session_speakers: () => d.speakers,
    read_live_names: () => d.liveNames,
    set_session_speaker_label: ({ clusterId, displayName, email }: { clusterId: number; displayName: string; email: string | null }) => {
      const sp = d.speakers.find((x) => x.cluster_id === clusterId);
      if (sp) { sp.display_name = displayName; sp.email = email; sp.is_user_labeled = true; }
      return null;
    },
    add_session_speaker: ({ displayName, email, voiceprintId }: { displayName: string; email: string | null; voiceprintId: string | null }) => {
      const clusterId = d.speakers.length;
      d.speakers.push({
        cluster_id: clusterId, display_name: displayName, email, voiceprint_id: voiceprintId,
        match_confidence: null, is_user_labeled: true, sample_text: null, speech_ms: 0,
        side: 'room', mixed_voices: false,
      });
      return clusterId;
    },
    remove_speaker_cluster: ({ clusterId }: { clusterId: number }) => {
      d.speakers = d.speakers.filter((s) => s.cluster_id !== clusterId);
      return null;
    },
    detach_speaker_voiceprint: ({ clusterId }: { clusterId: number }) => {
      const sp = d.speakers.find((x) => x.cluster_id === clusterId);
      if (sp) sp.voiceprint_id = null;
      return null;
    },
    enroll_voiceprint_from_speaker: ({ req }: { req: { cluster_id: number; display_name: string; email: string | null } }) => {
      const id = `v-${d.voiceprints.length + 1}`;
      d.voiceprints.push({
        id, display_name: req.display_name, email: req.email, created_at_unix_seconds: stamp(),
        session_count: 1, vector_dim: 192, sample_count: 1,
      });
      const sp = d.speakers.find((x) => x.cluster_id === req.cluster_id);
      if (sp) { sp.voiceprint_id = id; sp.display_name = req.display_name; sp.is_user_labeled = true; }
      return { voiceprint_id: id, vector_dim: 192, samples_ms: 8000 };
    },
    session_speaker_sample_text: () => 'Let us start with the roadmap.',
    session_speaker_sample_audio_bytes: () => new ArrayBuffer(0),
    list_voiceprints: () => d.voiceprints,
    rename_voiceprint: ({ req }: { req: { id: string; display_name: string; email: string | null } }) => {
      const v = d.voiceprints.find((x) => x.id === req.id);
      if (v) { v.display_name = req.display_name; v.email = req.email; }
      return null;
    },
    delete_voiceprint: ({ id }: { id: string }) => {
      d.voiceprints = d.voiceprints.filter((v) => v.id !== id);
      return null;
    },
    rematch_all_sessions: () => ({ sessions_scanned: d.sessions.length, clusters_matched: 1 }),
    diarize_session: () => ({
      speakers: d.speakers.length, clustered: d.speakers.length, count: d.speakers.length,
      segments_labeled: 24, segments_seen: 24, count_source: 'auto',
    }),
    // Every fixture session is a call with remote speech: the count lands on
    // the system track (the one other invitee) unless the mic is chosen.
    diarize_count: () => null,
    diarize_attendee_count: ({ track }: { track: string | null }) => (track === 'mic' ? 2 : 1),
    voice_model_status: () => ({ state: 'ready', path: 'C:\\Program Files\\Daisy\\models\\voiceprints\\model.onnx', error: null }),
    list_contacts: () => d.contacts,

    // --- tags ---
    list_tags: () => d.tags,
    search_tags: ({ query }: { query: string }) => d.tags.filter((t) => t.name.includes(String(query ?? ''))),
    create_tag: ({ req }: { req: { name: string; color_hex: string; prompt_md?: string | null; vocab_md?: string | null } }) => {
      const tag: Tag = {
        id: `t-${d.tags.length + 1}`, name: req.name, color_hex: req.color_hex,
        prompt_md: req.prompt_md ?? null, vocab_md: req.vocab_md ?? null,
        created_at_unix_seconds: stamp(), use_count: 0,
      };
      d.tags.push(tag);
      return tag;
    },
    update_tag: ({ req }: { req: { id: string; name?: string; color_hex?: string; prompt_md?: string | null; vocab_md?: string | null } }) => {
      const tag = d.tags.find((t) => t.id === req.id);
      if (!tag) throw { kind: 'tags', code: 'E_TAG_MISSING', message: 'no such tag', friendly: 'That tag no longer exists.' };
      if (req.name !== undefined) tag.name = req.name;
      if (req.color_hex !== undefined) tag.color_hex = req.color_hex;
      if (req.prompt_md !== undefined) tag.prompt_md = req.prompt_md;
      if (req.vocab_md !== undefined) tag.vocab_md = req.vocab_md;
      return tag;
    },
    delete_tag: ({ id, force }: { id: string; force: boolean }) => {
      const used = d.sessions.filter((s) => s.tag_ids.includes(id)).length;
      if (!force && used > 0) return { dangling_session_count: used };
      d.tags = d.tags.filter((t) => t.id !== id);
      for (const s of d.sessions) s.tag_ids = s.tag_ids.filter((t) => t !== id);
      return { dangling_session_count: 0 };
    },

    // --- prompts, providers ---
    list_prompts: () => d.prompts,
    save_prompt: ({ req }: { req: { id: string | null; name: string; directive_md: string; output: Prompt['output'] } }) => {
      const existing = req.id ? d.prompts.find((p) => p.id === req.id) : undefined;
      if (existing) {
        existing.name = req.name;
        existing.directive_md = req.directive_md;
        existing.output = req.output;
        return existing;
      }
      const prompt: Prompt = { id: `u${d.prompts.length + 1}`, name: req.name, output: req.output, directive_md: req.directive_md, builtin: false };
      d.prompts.push(prompt);
      return prompt;
    },
    delete_prompt: ({ id }: { id: string }) => {
      d.prompts = d.prompts.filter((p) => p.id !== id);
      return null;
    },
    reset_prompt: ({ id }: { id: string }) => d.prompts.find((p) => p.id === id) ?? d.prompts[0],
    set_default_summary_prompt: ({ id }: { id: string }) => {
      d.settings.default_summary_prompt_id = id;
      pushSettings();
      return null;
    },
    list_providers: () => d.providers,
    list_provider_models: () => ['model-a', 'model-b'],
    probe_context_length: ({ provider }: { provider: ProviderListEntry['name'] }) => (provider === 'lm_studio' ? 32768 : null),
    set_provider: ({ provider, config }: { provider: ProviderListEntry['name']; config: { api_key: string | null; model: string | null; base_url: string | null; context_tokens: number | null } }) => {
      const ctx = (prev: number | null) => (config.context_tokens === null ? prev : config.context_tokens === 0 ? null : config.context_tokens);
      const p = d.providers.find((x) => x.name === provider);
      if (p) {
        p.has_key = config.api_key !== null ? config.api_key !== '' : p.has_key;
        p.model = config.model;
        p.base_url = config.base_url;
        p.context_tokens = ctx(p.context_tokens);
      } else {
        d.providers.push({ name: provider, has_key: !!config.api_key, model: config.model, base_url: config.base_url, context_tokens: ctx(null) });
      }
      pushBuiltin();
      return null;
    },

    // --- audio, ASR ---
    list_audio_sources: () => d.audioSources,
    start_mic_meter: () => null,
    stop_mic_meter: () => null,
    speech_levels_list: () => d.speechLevels,
    // Clearing an override reverts to the learned level; the learned baseline is
    // kept aside while an override is in force.
    speech_level_set_override: ({ device, dbfs }: { device: string; dbfs: number | null }) => {
      const l = d.speechLevels.find((x) => x.device === device);
      if (l) {
        if (l.source === 'learned' && l.effective_dbfs !== null) learnedDbfs.set(device, l.effective_dbfs);
        l.override_dbfs = dbfs;
        l.effective_dbfs = dbfs ?? learnedDbfs.get(device) ?? l.effective_dbfs;
        l.source = dbfs === null ? 'learned' : 'override';
      }
      return null;
    },
    calibrate_speech_level: () => null,
    list_asr_models: () => d.asrModels,
    set_active_asr_model: ({ id }: { id: string }) => {
      for (const m of d.asrModels) m.active = m.id === id;
      d.machine.settings.asr.active = id;
      pushMachine();
      pushModels();
      return null;
    },
    delete_asr_model: ({ id }: { id: string }) => {
      const m = d.asrModels.find((x) => x.id === id);
      if (m) { m.installed = m.bundled_floor; m.profile_copy = false; }
      dropJob(id, 'cancelled');
      pushModels();
      return null;
    },
    download_queue_status: () => d.downloadQueue,
    download_queue_enqueue: ({ kind, id }: { kind: 'llm' | 'asr' | 'sa-assets'; id: string }) => {
      if (kind === 'llm' && d.builtinModel.capability === 'unsupported') {
        throw { kind: 'config', code: 'E_CONFIG', message: "This computer can't run the built-in model.", friendly: "This computer can't run the built-in model." };
      }
      if (d.downloadQueue.some((j) => j.id === id)) return null;
      void (kind === 'llm' ? runLlmJob(id) : runAsrJob(kind, id));
      return null;
    },
    download_queue_cancel: ({ id }: { id: string }) => {
      // Cancelling the built-in model's download deletes what it left.
      if (id === d.builtinModel.id) { deleteBuiltin(); return null; }
      queueCancelled.add(id);
      dropJob(id, 'cancelled');
      return null;
    },
    download_queue_retry: ({ id }: { id: string }) => {
      const kind = d.downloadQueue.find((j) => j.id === id)?.kind ?? 'llm';
      dropJob(id);
      void (kind === 'llm' ? runLlmJob(id) : runAsrJob(kind, id));
      return null;
    },
    builtin_model_delete: () => { deleteBuiltin(); return null; },

    // --- recording ---
    recording_snapshot: () => d.recording,
    start_recording: ({ req }: { req: { session_id: string | null; title: string | null } }) => {
      if (d.license.state === 'expired') {
        throw {
          kind: 'license', code: 'DAISY-E015',
          message: 'Activate a license to record or import meetings. Your existing meetings keep working.',
          friendly: 'Activate a license to record or import meetings. Your existing meetings keep working.',
        };
      }
      d.recording = {
        state: 'recording', session_id: req?.session_id ?? 's-live',
        session_root: `${d.profileDir}/sessions/s-live`,
        started_at_unix_seconds: stamp(), recorded_ms: 0,
        clock_running_since_unix_ms: Date.now(), live_mode_label: 'Nemotron Speech Streaming 0.6B (int8)',
        single_local_speaker: (req as { single_local_speaker?: boolean | null } | undefined)?.single_local_speaker ?? true,
      };
      emitEvent('recording:snapshot', d.recording);
      return d.recording;
    },
    pause_recording: () => {
      if (d.recording) {
        const since = d.recording.clock_running_since_unix_ms;
        if (since !== null) d.recording.recorded_ms += Math.max(0, Date.now() - since);
        d.recording.clock_running_since_unix_ms = null;
        d.recording.state = 'paused';
      }
      emitEvent('recording:snapshot', d.recording);
      return 'paused';
    },
    resume_recording: () => {
      if (d.recording) {
        d.recording.clock_running_since_unix_ms = Date.now();
        d.recording.state = 'recording';
      }
      emitEvent('recording:snapshot', d.recording);
      return 'recording';
    },
    finish_recording: () => {
      if (!d.recording) throw new Error('not recording');
      emitEvent('daisy://recording/finish', null);
      return null;
    },
    stop_recording: () => {
      d.recording = null;
      emitEvent('recording:snapshot', null);
      return 'stopped';
    },
    cancel_recording: () => {
      d.recording = null;
      emitEvent('recording:snapshot', null);
      return null;
    },
    switch_recording_mic: () => null,
    set_mic_muted: () => null,
    transcribe: () => 42,
    dedup: () => ({ dropped: 3, kept: 39 }),
    polish: () => ({ batches: 2, segments_polished: 12, segments_unchanged: 27, failed_batches: 0 }),
    recording_finalize_and_summarize: ({ req }: { req: { session_id: string } }) => {
      const s = session(req.session_id);
      if (s) { s.has_transcript = true; s.has_dedup = true; s.has_summary = true; s.finalized_at_unix_seconds = stamp(); }
      d.recording = null;
      return { status: 'completed', summary: summaryFor(req.session_id) };
    },
    recording_resume_finalize: ({ req }: { req: { session_id: string } }) => ({ status: 'completed', summary: summaryFor(req.session_id) }),
    retry_finalize: () => null,
    finalize_recovery: () => ({ attempts: 0, failed: false, reason: null, updated_at_unix: stamp() }),
    recordings_stats: () => d.recordingsStats,
    recordings_delete_all: () => ({ deleted_sessions: 3, freed_bytes: 900_000_000 }),
    show_main_window: () => null,
    show_mini_window: () => null,
    show_reminder_window: ({ title }: { title: string }) => {
      d.reminderTitle = title;
      d.reminderKind = 'calendar';
      return null;
    },
    resolve_close_prompt: ({ choice }: { choice: string }) => {
      if (choice === 'minimize' || choice === 'quit') {
        d.machine.settings.minimize_on_close = choice === 'minimize';
        pushMachine();
      }
      return null;
    },
    reminder_payload: (): ReminderPayload | null => {
      if (d.reminderKind === 'meeting') return { kind: 'meeting' };
      if (d.reminderKind === 'meeting_ended') return { kind: 'meeting_ended', app: d.reminderApp };
      return d.reminderTitle === null ? null : { kind: 'calendar', title: d.reminderTitle };
    },
    reminder_action: ({ choice }: { choice: string }) => {
      d.reminderChoices.push(choice);
      const m = d.machine.settings.meeting_prompt;
      if (choice === 'never') m.enabled = false;
      if (choice === 'snooze_30m') m.snooze_until = stamp() + 1800;
      if (choice === 'snooze_1h') m.snooze_until = stamp() + 3600;
      if (choice === 'snooze_today') m.snooze_until = Math.floor(new Date().setHours(24, 0, 0, 0) / 1000);
      if (choice === 'stop') {
        d.recording = null;
        emitEvent('recording:snapshot', null);
      }
      if (choice !== 'start' && choice !== 'stop') pushMachine();
      if (choice === 'start') {
        // quick_start_recording: starts with the default mic and title, then
        // announces it the way the backend does.
        d.recording = {
          state: 'recording', session_id: 's-live',
          session_root: `${d.profileDir}/sessions/s-live`,
          started_at_unix_seconds: stamp(), recorded_ms: 0,
          clock_running_since_unix_ms: Date.now(), live_mode_label: 'Nemotron Speech Streaming 0.6B (int8)',
          single_local_speaker: true,
        };
        emitEvent('recording:snapshot', d.recording);
        emitEvent('hotkey:started', null);
      }
      return null;
    },

    // --- workflows, integrations ---
    workflows_list: () => d.workflows,
    workflow_upsert: ({ req }: { req: Workflow }) => {
      const i = d.workflows.findIndex((w) => w.id === req.id);
      if (i >= 0) { d.workflows[i] = req; return req; }
      const created = { ...req, id: `w-${d.workflows.length + 1}`, created_at_unix_seconds: stamp() };
      d.workflows.push(created);
      return created;
    },
    workflow_delete: ({ id }: { id: string }) => {
      d.workflows = d.workflows.filter((w) => w.id !== id);
      return null;
    },
    workflow_history_read: ({ limit, skip }: { limit: number; skip: number }) => d.runs.slice(skip ?? 0, (skip ?? 0) + (limit ?? d.runs.length)),
    workflow_queue_state: () => d.queue,
    list_integrations: () => d.integrations,
    upsert_integration: ({ req }: { req: { id: string | null; name: string; enabled: boolean; webhook_url: string; auth: { type: string; name?: string }; payloads: IntegrationPublic['payloads'] } }) => {
      const authKind = req.auth.type === 'keep' ? 'none' : (req.auth.type as IntegrationPublic['auth_kind']);
      const entry: IntegrationPublic = {
        id: req.id ?? `i-${d.integrations.length + 1}`, name: req.name, enabled: req.enabled,
        kind: 'webhook', webhook_url: req.webhook_url, auth_kind: authKind,
        auth_header_name: req.auth.name ?? null, payloads: req.payloads,
      };
      const i = d.integrations.findIndex((x) => x.id === entry.id);
      if (i >= 0) d.integrations[i] = entry; else d.integrations.push(entry);
      return entry;
    },
    delete_integration: ({ id }: { id: string }) => {
      d.integrations = d.integrations.filter((i) => i.id !== id);
      return null;
    },
    integration_push: ({ sessionId, integrationId }: { sessionId: string; integrationId: string }) => {
      const integration = d.integrations.find((i) => i.id === integrationId);
      d.integrationRuns.unshift({
        at_unix_seconds: stamp(), session_id: sessionId, meeting_id: sessionId,
        meeting_title: session(sessionId)?.title ?? null, integration_id: integrationId,
        integration_name: integration?.name ?? 'Unknown', kind: 'webhook',
        payloads_sent: ['summary'], status: 'ok',
      });
      return null;
    },
    integration_history: ({ limit, skip }: { limit: number; skip: number }) => d.integrationRuns.slice(skip ?? 0, (skip ?? 0) + (limit ?? d.integrationRuns.length)),

    // --- calendars ---
    list_calendar_subscriptions: () => d.calendars,
    add_calendar_subscription: ({ req }: { req: { name: string; url: string } }) => {
      const sub: CalendarSubscription = {
        id: `cal-${d.calendars.length + 1}`, name: req.name, url: req.url, enabled: true,
        color_hex: '#4c8a3c', tag_id: null, dismissed_event_uids: [],
      };
      d.calendars.push(sub);
      return sub;
    },
    update_calendar_subscription: ({ req }: { req: { id: string; name?: string; url?: string; enabled?: boolean; color_hex?: string; tag_id?: string } }) => {
      const sub = d.calendars.find((c) => c.id === req.id);
      if (!sub) throw { kind: 'calendar', code: 'E_CAL_MISSING', message: 'no such subscription', friendly: 'That calendar is gone.' };
      if (req.name !== undefined) sub.name = req.name;
      if (req.url !== undefined) sub.url = req.url;
      if (req.enabled !== undefined) sub.enabled = req.enabled;
      if (req.color_hex !== undefined) sub.color_hex = req.color_hex;
      if (req.tag_id !== undefined) sub.tag_id = req.tag_id === '' ? null : req.tag_id;
      return sub;
    },
    delete_calendar_subscription: ({ id }: { id: string }) => {
      d.calendars = d.calendars.filter((c) => c.id !== id);
      return null;
    },
    refresh_calendars: (): CalendarRefreshResult => ({ subscriptions_scanned: d.calendars.length, events_loaded: d.events.length, errors: [] }),
    list_upcoming_events: () => d.events.filter((e) => {
      const sub = d.calendars.find((c) => c.id === e.subscription_id);
      return !sub || !sub.dismissed_event_uids.includes(e.uid);
    }),
    dismiss_calendar_event: ({ subscriptionId, uid }: { subscriptionId: string; uid: string }) => {
      const sub = d.calendars.find((c) => c.id === subscriptionId);
      if (sub) sub.dismissed_event_uids.push(uid);
      return null;
    },

    // --- tauri plugins the frontend calls ---
    'plugin:dialog|open': () => null,
    'plugin:dialog|save': () => null,
    'plugin:dialog|message': () => null,
    'plugin:dialog|confirm': () => true,
    'plugin:dialog|ask': () => true,
    'plugin:clipboard-manager|write_text': () => null,
    'plugin:window|theme': () => 'light',
    'plugin:window|set_theme': () => null,
    'plugin:window|start_dragging': () => null,
    'plugin:webview|set_webview_zoom': () => null,
  };

  return { data: d, handlers };
}
