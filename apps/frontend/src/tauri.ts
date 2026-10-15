// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { invoke, Channel } from '@tauri-apps/api/core';
import { writeText as clipboardWriteText } from '@tauri-apps/plugin-clipboard-manager';

/// Serialized `recording::supervisor::Notice` (the emitted variants).
export type AudioWarning =
  | { kind: 'mic-failed-over'; to: string; id: number }
  | { kind: 'mic-muted-externally' }
  | { kind: 'mic-very-quiet' }
  | { kind: 'mic-retrying' }
  | { kind: 'mic-dead-air' }
  | { kind: 'mic-held-by-call' }
  | { kind: 'mic-recovered' }
  | { kind: 'system-audio-lost' }
  | { kind: 'system-audio-recovered' }
  | { kind: 'reconnect-failed'; track: 'Mic' | 'System' };

/// `recording:slept` payload: a recording the computer's sleep paused.
export interface RecordingSlept {
  session_id: string;
  paused_at_unix_seconds: number;
  resumed_at_unix_seconds: number;
  /** False when the recording failed to restart and stays paused. */
  resumed: boolean;
}

export interface SessionListEntry {
  session_id: string;
  created_at_unix_seconds: number;
  finalized_at_unix_seconds: number | null;
  duration_seconds: number | null;
  title: string | null;
  tag_ids: string[];
  has_transcript: boolean;
  has_dedup: boolean;
  has_summary: boolean;
  /** Recovered from a force-quit/crash interruption (not a clean Stop). */
  interrupted?: boolean;
  /** `phone_room` = synced from a phone; `imported_file` = an audio file;
   *  absent = recorded here. */
  recording_kind?: RecordingKind | null;
}

export type RecordingKind = 'phone_room' | 'imported_file';

/** Extract a human-readable message from an unknown thrown value. Backend
 *  AppErrors arrive as `{ kind, code, message, friendly }` — prefer the
 *  friendly message and append the help-referenceable code. Falls back to
 *  `.message`, then the stringified value. */
export function errStr(e: unknown): string {
  const o = e as { friendly?: unknown; code?: unknown; message?: unknown };
  if (o && typeof o.friendly === 'string' && o.friendly) {
    return typeof o.code === 'string' && o.code ? `${o.friendly} (${o.code})` : o.friendly;
  }
  return String(o?.message ?? e);
}

/** Format a duration in seconds as "M:SS" or "H:MM:SS". */
export function formatDurationSeconds(total: number | null | undefined): string {
  if (total == null || !Number.isFinite(total) || total <= 0) return '—';
  const t = Math.round(total);
  const h = Math.floor(t / 3600);
  const m = Math.floor((t % 3600) / 60);
  const s = t % 60;
  return h > 0
    ? `${h}:${String(m).padStart(2, '0')}:${String(s).padStart(2, '0')}`
    : `${m}:${String(s).padStart(2, '0')}`;
}

/** Coarse duration for headers: rounds to whole minutes ("1h 5m" / "12m"). */
export function formatDurationHm(total: number | null | undefined): string {
  if (total == null || !Number.isFinite(total) || total <= 0) return '—';
  const mins = Math.max(1, Math.round(total / 60));
  const h = Math.floor(mins / 60);
  const m = mins % 60;
  if (h > 0) return m > 0 ? `${h}h ${m}m` : `${h}h`;
  return `${m}m`;
}

/** Format a byte count as "N B" / "N KB" / "N MB" / "N GB" (binary units). */
export function formatBytes(n: number | null | undefined): string {
  if (n == null || !Number.isFinite(n) || n <= 0) return '0 B';
  const units = ['B', 'KB', 'MB', 'GB', 'TB'];
  let v = n;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i++;
  }
  return `${i === 0 ? v : v.toFixed(v < 10 ? 1 : 0)} ${units[i]}`;
}

export interface RecordingsStats {
  session_count: number;
  wav_bytes: number;
  opus_bytes: number;
  deletable_session_count: number;
  deletable_bytes: number;
}

export interface DeleteSummary {
  deleted_sessions: number;
  freed_bytes: number;
}

export type WebhookAuthKind = 'none' | 'header' | 'bearer';

/** What set a diarization run's speaker count, highest priority first. */
export type CountSource = 'typed' | 'voiceprints' | 'expected' | 'attendees' | 'auto';

export interface DiarizeResult {
  /** One above the highest cluster id the run assigned. */
  speakers: number;
  /** Speakers the run grouped, across the tracks it read. */
  clustered: number;
  /** Speakers the run grouped on the track the count applied to; null exactly when `count_source` is. */
  count: number | null;
  segments_labeled: number;
  segments_seen: number;
  /** Null when nothing was grouped, or a solo-mic session took one speaker from the recording setup. */
  count_source: CountSource | null;
}

/** How a session's last diarization run settled its speaker count; persisted on the session. */
export interface DiarizeCount {
  source: CountSource;
  /** The number a person stated (`typed` or `expected` source), as stated; null for the other sources. */
  requested: number | null;
  /** Speakers the run grouped on the track the count applied to. */
  found: number;
}

/** Which track(s) to re-diarize. 'others' = system/attendees (default),
 *  'mic' = local mic (in-person), 'both' = system + mic. */
export type DiarizeTrack = 'others' | 'mic' | 'both';

/** The on-device speaker-encoder model, as voice grouping finds it. An
 *  `unreadable` model may be on disk; `error` is the OS error reading it. */
export interface VoiceModelStatus {
  state: 'ready' | 'missing' | 'unreadable';
  path: string;
  error: string | null;
}

export interface PayloadSelection {
  summary: boolean;
  notes: boolean;
  transcript: boolean;
}

export interface IntegrationPublic {
  id: string;
  name: string;
  enabled: boolean;
  kind: string; // "webhook"
  webhook_url: string | null;
  auth_kind: WebhookAuthKind;
  auth_header_name: string | null; // header NAME only — never the value
  payloads: PayloadSelection;
}

/** `keep` = leave the stored auth unchanged (edit without re-typing the secret). */
export type WebhookAuthInput =
  | { type: 'keep' }
  | { type: 'none' }
  | { type: 'header'; name: string; value: string }
  | { type: 'bearer'; token: string };

export interface UpsertIntegration {
  id: string | null; // null = create
  name: string;
  enabled: boolean;
  webhook_url: string;
  auth: WebhookAuthInput;
  payloads: PayloadSelection;
}

export interface IntegrationHistoryEntry {
  at_unix_seconds: number;
  session_id: string;
  meeting_id: string;
  meeting_title: string | null;
  integration_id: string;
  integration_name: string;
  kind: string;
  payloads_sent: string[];
  status: string; // "ok" | "error: ..."
}

// --- workflows (mirrors Rust serde output exactly) ---
export type WorkflowTrigger = 'finalized' | 'deleted' | 'imported' | 'finalize_failed';

export type ConditionNode =
  | { type: 'all'; children: ConditionNode[] }
  | { type: 'any'; children: ConditionNode[] }
  | { type: 'has_tag'; tag_id: string }
  | { type: 'has_participant'; contact_id: string }
  | { type: 'title_contains'; needle: string };

export type ActionStep =
  | { type: 'run_prompt'; prompt_id: string; send_to_integration: string | null; write_to_dir: string | null }
  | { type: 'push_integration'; integration_id: string };

export interface Workflow {
  id: string; // '' = create on upsert
  name: string;
  enabled: boolean;
  trigger: WorkflowTrigger;
  condition: ConditionNode;
  actions: ActionStep[];
  created_at_unix_seconds: number;
}

export interface WorkflowStepRecord { label: string; status: string; duration_ms: number }

export interface WorkflowRunRecord {
  run_id: string;
  at_unix_seconds: number;
  workflow_id: string;
  workflow_name: string;
  session_id: string;
  session_title: string | null;
  trigger: WorkflowTrigger;
  status: 'ok' | 'partial' | 'error' | 'gave-up';
  steps: WorkflowStepRecord[];
}

export interface QueuedWorkflowRun {
  run_id: string;
  workflow_id: string;
  workflow_name: string;
  session_id: string;
  session_title: string | null;
  tag_ids: string[];
  trigger: WorkflowTrigger;
  actions: ActionStep[];
  attempts: number;
  created_at_unix_seconds: number;
}

export interface WorkflowRunEvent {
  run_id: string;
  workflow_name: string;
  session_id: string;
  step_index: number;
  step_count: number;
  step_label: string;
  status: 'running' | 'ok' | 'partial' | 'error';
}

export interface SessionView {
  session_id: string;
  manifest_json: unknown;
  transcript_md: string | null;
  has_transcript: boolean;
  has_dedup: boolean;
  has_summary: boolean;
}

export interface StartRequest {
  mic_source_id: number;
  system_source_id: number | null;
  session_id: string | null;
  title: string | null;
  tag_ids?: string[];
  notes_md?: string | null;
  meeting_id?: string | null;
  calendar_link?: CalendarLink | null;
  attendees?: Attendee[];
  single_local_speaker?: boolean;
}

export interface Tag {
  id: string;
  name: string;
  color_hex: string;             // "#RRGGBB" or "#RGB"
  prompt_md: string | null;
  vocab_md: string | null;
  created_at_unix_seconds: number;
  use_count: number;
  updated_at_unix_seconds?: number | null;
}

export interface TermEvidence { term: string; count: number }
export interface VocabSuggestion { tag_id: string; total_hits: number; evidence: TermEvidence[] }
export interface LastTimeSuggestion { source_session_id: string; tag_ids: string[] }
export interface SessionSuggestions { last_time: LastTimeSuggestion | null; vocab: VocabSuggestion[] }

export interface Contact {
  id: string;
  display_name: string;
  emails: string[];
  created_at_unix_seconds: number;
  updated_at_unix_seconds?: number | null;
}

export interface Attendee {
  display_name: string;
  role: 'self' | 'other';
}

export interface SessionMeta {
  session_id: string;
  title: string | null;
  tag_ids: string[];
  attendees: Attendee[];
  has_notes: boolean;
  has_summary: boolean;
  recording_segments: number;
  sent_integration_ids: string[];
  recording_kind?: RecordingKind | null;
}

export interface SessionMetaUpdate {
  session_id: string;
  title?: string | null;        // undefined = leave; null = clear; string = set
  tag_ids?: string[];
  attendees?: Attendee[];
  created_at_unix_seconds?: number; // undefined = leave; user-corrected recording date
}

export interface ActionItem {
  task: string;
  owner?: string | null;
  due?: string | null;
}

export interface ProducedBy { device: string; provider: string; model: string; }
export interface SummaryChapter { title: string; start_ms?: number | null; summary?: string | null; }

export type PromptOutput = 'classic' | 'sectioned';
export interface Prompt {
  id: string;
  name: string;
  output: PromptOutput;
  directive_md: string;
  builtin: boolean;
}

export interface AnalysisResult {
  prompt_id: string;
  prompt_name: string;
  markdown: string;
  generated_at_unix_seconds: number;
}

export interface SessionSummary {
  v: number;
  session_id: string;
  generated_at_unix_seconds: number;
  produced_by: ProducedBy;
  tldr: string;
  action_items: ActionItem[];
  decisions: string[];
  open_questions: string[];
  key_topics: string[];
  chapters: SummaryChapter[];
  markdown: string;
  user_edited: boolean;
  skipped: { start_ms: number; end_ms: number }[];
  turn_count: number;
  source_inputs_hash?: string | null;
  language?: string | null;
}

export type SessionHitSource =
  | 'title' | 'transcript' | 'summary' | 'notes' | 'action' | 'attendee' | 'tag' | 'metadata';

export interface MatchSnippet {
  source: SessionHitSource;
  snippet: string;
}

export interface SessionHit {
  session_id: string;
  title: string | null;
  created_at_unix_seconds: number;
  tag_ids: string[];
  snippet: string | null;
  match_source: SessionHitSource;
  matches: MatchSnippet[];
}

/** Where to land when opening a session — which detail tab, and (for
 *  transcript) a query string to scroll to + highlight. Carried on the
 *  library route; a search result jumps to the matched line. */
export type SessionFocusTab = 'summary' | 'transcript' | 'notes' | 'participants' | 'chat';
export interface SessionFocus {
  tab?: SessionFocusTab;
  query?: string;
  /** Jump the transcript + audio player to this offset on arrival. */
  seekMs?: number;
}

export interface FinalizeProgress {
  stage: 'finalizing' | 'echo-cancelling' | 'transcribing' | 'deduping' | 'diarizing' | 'polishing' | 'awaiting-labels' | 'summarizing' | 'chaptering' | 'compressing' | 'resuming' | 'done' | 'error';
  progress: number;             // 0.0 .. 1.0
  message: string | null;
}

/** Mirror of `tauri_app_core::commands::finalize::FinalizeOutcome`. The
 *  cascade either completed (with or without a summary), or paused at the
 *  speaker-label gate and is waiting for the listed clusters to be
 *  named/enrolled before the summary runs (resume via
 *  `recordingResumeFinalize`). `summary === null` means the transcript was
 *  saved but the summary generation failed (e.g. no provider configured) —
 *  user can re-summarize from SummaryPane. */
export type FinalizeOutcome =
  | { status: 'completed'; summary: SessionSummary | null }
  | { status: 'needs_labels'; clusters: number[] };

/** Mirror of `tauri_app_core::commands::finalize::FinalizeStatus` — the on-disk
 *  status sidecar written at each finalize stage boundary. Single source for
 *  the live-progress widget and the startup crash-recovery audit. */
export interface FinalizeStatus {
  stage: 'finalizing' | 'echo-cancelling' | 'transcribing' | 'deduping' | 'diarizing' | 'polishing' | 'awaiting-labels' | 'summarizing' | 'chaptering' | 'compressing' | 'resuming' | 'done' | 'error';
  progress: number;
  message: string | null;
  updated_at_unix: number;
}

export interface RecordingSnapshot {
  state: 'idle' | 'recording' | 'paused' | 'stopped';
  session_id: string;
  session_root: string;
  started_at_unix_seconds: number;
  /** The recording clock: ms of audio recorded (paused spans and capture
   *  gaps excluded), read at `clock_running_since_unix_ms`. */
  recorded_ms: number;
  /** Unix ms `recorded_ms` was read while recording; the clock runs on from
   *  there. Null while paused or while capture has stalled, when the clock
   *  holds still. */
  clock_running_since_unix_ms: number | null;
  /** Short label for the live transcription mode in use, e.g. "off", "ggml-base.en.bin". */
  live_mode_label: string;
  /** The mic carries one voice: the value the recording started with. */
  single_local_speaker: boolean;
}

export interface DedupSummary {
  dropped: number;
  kept: number;
}

export interface PolishSummary {
  batches: number;
  segments_polished: number;
  segments_unchanged: number;
  failed_batches: number;
}

export interface BootstrapStatus {
  has_bootstrap: boolean;
  profile_dir: string | null;
  platform_default: string;
  /** Set when DAISY_PROFILE_DIR overrides the saved location this session. */
  env_override: string | null;
}

export interface VaultStatus {
  vault_exists: boolean;
  unlocked: boolean;
}

/** How the profile's vault opens. `trusted_on` names the computer whose key
 *  opens a machine-mode vault; null in passphrase mode or when unrecorded. */
export interface VaultTrust {
  kind: 'passphrase' | 'machine' | string;
  trusted_on: string | null;
  this_machine: string;
}

export type AecModeOverride = 'auto' | 'always' | 'never';

/** Mirror of `tauri_app_core::state::ProviderId` — the AI (summarization)
 *  providers. Wire format is the snake_case string; this union keeps that
 *  exact spelling. Transcription is on-device and not a provider choice. */
export type ProviderId =
  | 'groq'
  | 'openai'
  | 'anthropic'
  | 'lm_studio'
  | 'ollama'
  | 'daisy_gateway'
  | 'local';

/** The AI features provider on this machine: its own choice when it has one,
 *  else the profile's. */
export function aiProvider(settings: Settings, machine: MachineSettings | null): ProviderId | null {
  return machine?.ai_provider !== undefined ? machine.ai_provider : settings.default_summary_provider;
}

/** Human-readable label for a ProviderId. Use anywhere a raw provider id is
 *  shown in the UI ("anthropic" → "Anthropic", etc.). */
export function providerLabel(p: ProviderId | string): string {
  switch (p) {
    case 'groq':           return 'Groq';
    case 'openai':         return 'OpenAI';
    case 'anthropic':      return 'Anthropic';
    case 'lm_studio':      return 'LM Studio';
    case 'ollama':         return 'Ollama';
    case 'daisy_gateway':  return 'Daisy Cloud (Internal use only)';
    case 'local':          return 'On this computer';
    default:               return p;
  }
}

export interface Settings {
  schema_version: number;
  /** Per-machine settings blocks keyed by machine name. Read/write THIS
   *  machine's block via machineSettings/updateMachineSettings — never
   *  another machine's. */
  machines?: Record<string, MachineSettings>;
  /** AI features provider. `null` means "no provider — use copy-paste". A
   *  machine's own `ai_provider` takes precedence; read it with aiProvider. */
  default_summary_provider: ProviderId | null;
  /** Summary Style applied to new summaries. `null`/unknown → Daisy built-in. */
  default_summary_prompt_id: string | null;
  user_display_name: string | null;
  /** Ordered nav-rail item keys (e.g. ["record","library",…]). Empty = default
   *  order. Settings is always anchored at the bottom and not included. */
  nav_order: string[];
  /** Check for app updates on launch + periodically (notify-only). */
  auto_update_check: boolean;
  /** Seconds before a meeting's start to pop the reminder window (no UI; edit
   *  settings.json). Default 60; 0 disables. */
  reminder_lead_seconds?: number;
  /** How much variation within one voice Daisy accepts before calling a
   *  speaker group two people, 0–100. Default 65. */
  voice_match_tolerance?: number;
}

/** Whisper tunables; backend re-clamps on read. */
export interface WhisperTune {
  /** ISO 639-1 hint ("en"); null = autodetect. */
  language: string | null;
  window_cap_s: number;
  /** 0 = auto. */
  threads: number;
  /** Catch-up hop ladder override (ms); settings-file only, no UI. */
  hop_ladder_ms?: number[] | null;
}

/** Local ASR engine selection + tunables (per machine). Omitting a tune
 *  block resets it to the backend's platform default. */
export interface AsrSettings {
  active: string | null;
  whisper?: WhisperTune;
}

/** Meeting-started prompt state on this machine. */
export interface MeetingPrompt {
  enabled: boolean;
  /** Unix seconds the prompt stays quiet until. */
  snooze_until: number | null;
  /** Offer to stop the recording when the meeting ends. */
  stop_enabled: boolean;
}

/** What the reminder popup is showing. */
export type ReminderPayload =
  | { kind: 'calendar'; title: string }
  | { kind: 'meeting' }
  | { kind: 'meeting_ended'; app: string };

/** A button the reminder popup offers. */
export type ReminderChoice =
  | 'open' | 'dismiss' | 'start' | 'stop'
  | 'snooze_30m' | 'snooze_1h' | 'snooze_today' | 'never';

/** Whether this machine can tell when another app opens the microphone. */
export interface MeetingDetectionSupport {
  supported: boolean;
  reason: string | null;
}

/** One machine's settings block — devices, paths, environment, performance.
 *  The profile syncs across machines; each machine edits only its own. */
export interface MachineSettings {
  /** Stable persisted identity; the numeric id below is a resolved view. */
  default_mic_key?: string | null;
  default_mic_source_id: number | null;
  aec_mode_override: AecModeOverride;
  denoise_enabled: boolean;
  /** Diagnostic logging level. App restart required. */
  debug_level: 'off' | 'basic' | 'full';
  /** Whole-UI zoom factor (1.0 = 100%), clamped 0.5..=2.0. */
  ui_zoom: number;
  /** Theme override: stem of a file in <profile>/css injected after the
   *  built-in stylesheet. null = built-in look. */
  css_theme?: string | null;
  /** 'light' | 'dark' | 'system' — how the app is painted on this machine. */
  appearance?: string;
  /** Daisy's icon in the system tray (the menu bar on macOS). */
  show_tray_icon: boolean;
  /** What closing the main window does: true minimizes, false quits, null
   *  asks on the next close. */
  minimize_on_close: boolean | null;
  mcp_enabled: boolean;
  mcp_port: number;
  mcp_allow_write: boolean;
  /** Shows every screen and setting. Off hides the ones most people never
   *  need; what is hidden keeps running. */
  nerd_mode: boolean;
  /** Show the captions panel while recording. Display only — the live
   *  decode always runs. */
  show_live_captions: boolean;
  /** Names the speakers above the live captions; loads a diarization model
   *  while recording. */
  live_speaker_names: boolean;
  asr: AsrSettings;
  meeting_prompt: MeetingPrompt;
  /** AI features provider on this machine (written by setAiProvider).
   *  Absent: the profile's default_summary_provider. null: none. */
  ai_provider?: ProviderId | null;
  /** Global accelerator that starts a recording ("Control+Alt+R"). null = no
   *  hotkey. */
  record_hotkey?: string | null;
}

export type CloseChoice = 'minimize' | 'quit' | 'cancel';

/** The Settings-UI-owned fields of this machine's block. An update changes
 *  only the fields present; `default_mic_source_id: null` clears the mic. */
export type MachinePatch = Partial<Pick<MachineSettings,
  | 'default_mic_source_id' | 'aec_mode_override' | 'show_live_captions' | 'live_speaker_names' | 'asr'
  | 'denoise_enabled' | 'debug_level' | 'ui_zoom' | 'css_theme' | 'appearance'
  | 'show_tray_icon' | 'mcp_enabled' | 'mcp_port' | 'mcp_allow_write' | 'meeting_prompt'
  | 'record_hotkey' | 'nerd_mode'>> & { minimize_on_close?: boolean };

/** The Settings-UI-owned fields of the profile's settings. An update changes
 *  only the fields present. */
export type SettingsPatch = Partial<Pick<Settings,
  | 'default_summary_prompt_id' | 'user_display_name' | 'nav_order'
  | 'auto_update_check' | 'voice_match_tolerance'>>;

/** This machine's settings block + the machine name it is stored under. */
export interface MachineSettingsStatus {
  machine: string;
  settings: MachineSettings;
}

export type PairedPhoneView = {
  /** `name` is what this computer calls the phone; `phone_name` the name it paired with. */
  device_id: string; name: string; phone_name: string; platform: string;
  paired_at_unix: number; last_hello_unix: number | null;
  /** Set while this device's syncs are refused at the version gate: the side
   *  whose Daisy is out of date. */
  out_of_date: OutOfDate | null;
};
/** The side of a sync whose protocol version is the lower one. */
export type OutOfDate = 'desktop' | 'phone';
/** The `sync:gated` event: a paired device's sync refused at the version gate. */
export type SyncGated = { device_id: string; platform: string; out_of_date: OutOfDate };
export type PairingView = { qr_text: string; key_text: string; expires_unix: number };
/** Why a phone session cannot be opened here: sealed to a key this computer
 *  does not hold, written by a newer Daisy than this one reads, or holding
 *  audio that does not decode. */
export type RefusalReason = 'sealed' | 'unsupported' | 'undecodable';
export type RefusedView = { session: string; device_id: string; reason: RefusalReason };
/** The `sync:refused` event: a phone session this computer gave up on, with
 *  its title and start time when known. */
export type SyncRefused = {
  device_id: string;
  session: string;
  reason: RefusalReason;
  title: string | null;
  recorded_at_unix_seconds: number | null;
};
/** The `sync:withdrawn` event: a phone deleted a session it was sending. */
export type SyncWithdrawn = { device_id: string; session: string };
/** How far a phone's recording has come. `receiving` repeats as each file
 *  lands; the rest fire once each, in this order. */
export type SyncImportStage =
  | 'receiving' | 'importing' | 'audio' | 'transcript' | 'summary' | 'chapters' | 'done';
/** The `sync:import` event: where a phone's recording has got to. `received`
 *  and `total` count audio pieces and are present once the manifest has
 *  arrived; `title` is filled in at `done`. */
export type SyncImport = {
  device_id: string; device_name: string; session: string; stage: SyncImportStage;
  received: number | null; total: number | null; title: string | null;
  /** The id this library holds the recording under, from the moment it is
   *  there. It differs from `session` when this computer already held a
   *  recording under the phone's id and minted another. */
  library_session: string | null;
};
export type FirewallHint = {
  firewall: 'ufw' | 'firewalld';
  command: string;
};

export type SyncStatus = {
  device_id: string; machine_name: string; paired: PairedPhoneView[]; window: PairingView | null;
  /** The name and port a phone reaches this computer at; `host` is null
   *  when this computer has no name or address a phone accepts. */
  host: string | null; port: number;
  /** Whether the sync server is accepting connections right now. */
  listening: boolean;
  /** Whether this computer can wake a phone over Bluetooth. */
  bluetooth: 'on' | 'off' | 'denied' | 'unsupported';
  /** Phone sessions this computer cannot open, in id order. */
  refused: RefusedView[];
  platform: 'windows' | 'macos' | 'linux';
};

export interface McpStatus {
  enabled: boolean;
  running: boolean;
  port: number;
  token: string;
  endpoint: string;
  /** Paste-ready `claude mcp add …` command (embeds the secret token). */
  claude_command: string;
}

export type AsrEngine = 'whisper' | 'nemotron' | 'speechanalyzer';

export interface AsrModelInfo {
  id: string;
  engine: AsrEngine;
  display: string;
  installed: boolean;
  active: boolean;
  /** Ships in this platform's bundle — the bundled copy can't be deleted. */
  bundled_floor: boolean;
  /** A profile-side copy exists (what Delete removes; shadows the bundle on
   *  floor rows). */
  profile_copy: boolean;
  multilingual: boolean;
  /** Approximate install size. */
  bytes: number;
  license: string;
  /** Where the license text lives; empty when there is none to open. */
  license_url: string;
}

export type LlmCapability = 'fast' | 'unsupported';

/** Mirror of `tauri_app_core::builtin_model::BuiltinRow`. */
export interface BuiltinRow {
  id: string;
  display: string;
  installed: boolean;
  /** Exact install size. */
  bytes: number;
  license: string;
  /** Where the license text lives; empty when there is none to open. */
  license_url: string;
  capability: LlmCapability;
  /** The line shown under the row: what this machine can do with it. */
  capability_line: string;
  /** Bytes a stopped download left staged on disk. */
  partial_bytes: number;
  /** The model is loading into its worker after an install. */
  warming: boolean;
  /** The last warm-up's error: installed, but its worker would not start
   *  on this computer. */
  warm_failed: string | null;
}

/** Mirror of `tauri_app_core::commands::summary::SummaryProviderStatus`. */
export interface SummaryProviderStatus {
  state: SummaryProviderStatusKind;
  provider: string | null;
  hint: string | null;
}

/** Mirror of `tauri_app_core::builtin_model::BuiltinStatus`: the built-in
 *  model's row and the AI provider status every gate reads. */
export interface BuiltinStatus {
  model: BuiltinRow;
  provider: SummaryProviderStatus;
}

export type DownloadKind = 'llm' | 'asr' | 'sa-assets';

export type DownloadPhase =
  | { state: 'queued' }
  | { state: 'downloading'; pct: number }
  /** An attempt failed and the next one is waiting out its delay. */
  | { state: 'retrying'; attempt: number; of: number; error: string }
  | { state: 'failed'; error: string };

/** Mirror of `tauri_app_core::downloads::DownloadJob`. */
export interface DownloadJob {
  kind: DownloadKind;
  id: string;
  /** The source reports no byte count. */
  indeterminate: boolean;
  phase: DownloadPhase;
}

/** Mirror of `tauri_app_core::downloads::Finished`, the `download:finished`
 *  event. `failed` is a job that spent its attempts; it stays in the queue,
 *  failed, until a retry or a cancel. */
export interface DownloadFinished {
  id: string;
  kind: DownloadKind;
  outcome: 'installed' | 'cancelled' | 'failed';
}

/** Mirror of `tauri_app_core::commands::onboarding::AiPlan`. */
export type AiPlan =
  | { kind: 'builtin'; capability: LlmCapability; installed: boolean }
  | { kind: 'none'; reason: string };

/** Mirror of `tauri_app_core::commands::onboarding::AsrPlan`. */
export type AsrPlan =
  | { engine: 'speech_analyzer'; ready: boolean }
  | { engine: 'nemotron'; installed: boolean };

/** What first-run setup does on this machine. */
export interface OnboardingPlan {
  ai: AiPlan;
  transcription: AsrPlan;
  profile_dir: string;
}

export type LicenseStatus =
  | { state: 'licensed'; name: string; email: string; key: string; expires: number | null; subscription_type: string; entitlements: string[] }
  | { state: 'trial'; days_left: number }
  | { state: 'expired' };

export interface UpdateInfo {
  current: string;
  latest: string;
  update_available: boolean;
  notes: string;
  url: string | null;
}

export interface AudioSourceInfo {
  id: number;
  kind: 'mic' | 'monitor';
  node_name: string;
  description: string;
}

export interface SpeechLevelInfo {
  device: string;
  effective_dbfs: number | null;
  source: 'override' | 'calibration' | 'learned' | null;
  samples: number;
  override_dbfs: number | null;
}

export interface ProviderListEntry {
  name: ProviderId;
  has_key: boolean;
  model: string | null;
  base_url: string | null;
  /** Context length of a local endpoint (LM Studio, Ollama), as entered. */
  context_tokens: number | null;
}

export interface ProviderConfigInput {
  api_key: string | null;
  model: string | null;
  base_url: string | null;
  /** null = keep the stored value; 0 = clear; n = set. */
  context_tokens: number | null;
}

export interface CalendarSubscription {
  id: string;
  name: string;
  url: string;
  enabled: boolean;
  color_hex: string;
  tag_id: string | null;
  dismissed_event_uids: string[];
}

export interface CalendarAttendee {
  display_name: string | null;
  email: string | null;
}

export interface CalendarEvent {
  id: string;
  uid: string;
  subscription_id: string;
  subscription_name: string;
  subscription_color: string;
  subscription_tag_id: string | null;
  title: string;
  start_unix_seconds: number;
  end_unix_seconds: number;
  attendees: CalendarAttendee[];
  location: string | null;
  description: string | null;
}

/** Mirror of `recording::manifest::CalendarLink`. Written into the
 *  manifest's `calendar` field when a recording is started from a
 *  Calendar event. */
export interface CalendarLink {
  provider: string;        // subscription_id
  event_id: string;        // CalendarEvent.uid
  recurrence_id: string | null;
  planned_start_unix_seconds: number;
  planned_end_unix_seconds: number;
}

/** Pre-filled-recording payload handed from the Calendar route to
 *  ActiveSession when a user clicks an event. Drives the start-screen
 *  title, the auto-tag, and the manifest.calendar back-reference. */
/** One diarized speaker on a track: turns in session seconds on the transcript clock. */
export interface LiveSpeakerTrack {
  slot: number;
  turns: { start_s: number; end_s: number }[];
  name?: { voice_id: string; display_name: string } | { ordinal: number };
}

/** One slot in a `live-speakers` event. `turns` are closed turns, which no
 *  later event changes: all of them on a `full` event, else those closed
 *  since the previous event. `open` is the turn still growing and replaces
 *  the previous event's. */
export interface LiveSpeakerDelta {
  slot: number;
  turns: { start_s: number; end_s: number }[];
  open: { start_s: number; end_s: number }[];
  name: { voice_id: string; display_name: string } | { ordinal: number };
}

/** Payload of the `live-speakers` event: every slot that has spoken on the
 *  track, with what changed since the previous event. A `full` event carries
 *  the whole history and replaces what was kept. */
export interface LiveSpeakersPayload {
  session_id: string;
  track: 'system' | 'mic';
  full: boolean;
  tracks: LiveSpeakerDelta[];
}

/** One transcript line on the session clock, with its track. */
export interface UndiarizedLine {
  track: 'mic' | 'system';
  start_ms: number;
  end_ms: number;
  text: string;
}

/** The overlay files of a stopped session whose transcript has no speakers yet. */
export interface UndiarizedNames {
  system: LiveSpeakerTrack[];
  mic: LiveSpeakerTrack[];
  /** The mic carries one voice. */
  single_local_speaker: boolean;
  lines: UndiarizedLine[];
}

export interface EventSeed {
  title: string;
  tag_id: string | null;
  calendar_link: CalendarLink;
  attendees: CalendarAttendee[];
}

export interface CalendarRefreshResult {
  subscriptions_scanned: number;
  events_loaded: number;
  errors: string[];
}

export interface Chapter {
  title: string;
  start_hms: string;
  summary?: string | null;
}

export interface SessionChapters {
  schema_version: number;
  session_id: string;
  model: string;
  generated_at_unix_seconds: number;
  chapters: Chapter[];
}

export interface ChaptersResult {
  chapters: Chapter[];
  skipped: boolean;
  reason: string | null;
}

/** The AI tasks that share the one local-LLM worker. */
export type AiJobKind = 'summary' | 'chapters';

/** One AI task in flight, as `ai_jobs_list` and `ai-jobs:changed` carry it. */
export interface AiJob {
  session_id: string;
  kind: AiJobKind;
  started_at_unix_seconds: number;
}

export interface QaCitation {
  session_id: string;
  session_title: string | null;
  created_at_unix_seconds: number | null;
  chunk_index: number;
  start_ms: number | null;
  excerpt: string;
  score: number;
}

export interface QaAnswer {
  query: string;
  answer: string;
  citations: QaCitation[];
  indexed_sessions: number;
  total_chunks: number;
}

/** In-call chat (this-meeting-only conversation). */
export interface CallChatMsg {
  role: 'user' | 'assistant';
  content: string;
  ts?: number;
}
export interface CallChat {
  messages: CallChatMsg[];
  transcript_cursor_ms: number;
}
export interface LiveChatReply {
  reply: string;
  chat: CallChat;
}

export interface SessionSpeaker {
  cluster_id: number;
  display_name: string;
  email: string | null;
  voiceprint_id: string | null;
  match_confidence: number | null;
  is_user_labeled: boolean;
  sample_text: string | null;
  speech_ms: number;
  side: 'room' | 'remote';
  /** Diarization could not read this cluster as one clear voice — a separable
   *  second voice, or too little agreement across the sample to tell.
   *  Enrollment refuses it unless the user saves it anyway. */
  mixed_voices: boolean;
}

export interface VoiceprintView {
  id: string;
  display_name: string;
  email: string | null;
  created_at_unix_seconds: number;
  session_count: number;
  vector_dim: number;
  sample_count: number;
}

export interface EnrollResult {
  voiceprint_id: string;
  vector_dim: number;
  samples_ms: number;
}

export const tauri = {
  bootstrapStatus: () => invoke<BootstrapStatus>('bootstrap_status'),
  bootstrapSet: (profileDir: string) => invoke<void>('bootstrap_set', { profileDir }),
  consentStatus: () => invoke<boolean>('consent_status'),
  acceptConsent: () => invoke<void>('accept_consent'),
  eulaStatus: () => invoke<boolean>('eula_status'),
  acceptEula: () => invoke<void>('accept_eula'),
  profileBindingCheck: () => invoke<{ state: 'ok' | 'foreign' }>('profile_binding_check'),
  licenseStatus: () => invoke<LicenseStatus>('license_status'),
  activateLicense: (key: string) => invoke<LicenseStatus>('activate_license', { key }),
  /** License heartbeat — throttled to ~once/day server-side. Returns status. */
  licenseCheckin: () => invoke<LicenseStatus>('license_checkin'),
  deactivateLicense: () => invoke<LicenseStatus>('deactivate_license'),
  checkForUpdate: () => invoke<UpdateInfo>('check_for_update'),
  openExternal: (url: string) => invoke<void>('open_external', { url }),
  openLogsDir: () => invoke<string>('open_logs_dir'),
  openProfileDir: () => invoke<string>('open_profile_dir'),
  vaultStatus: () => invoke<VaultStatus>('vault_status'),
  unlockVault: (passphrase: string) => invoke<void>('unlock_vault', { passphrase }),
  changeVaultPassphrase: (oldPassphrase: string, newPassphrase: string) =>
    invoke<void>('change_vault_passphrase', { oldPassphrase, newPassphrase }),
  /** Initialize a vault that auto-unlocks on launch (no passphrase prompt).
   *  Key is derived from the machine ID; warn the user before calling. */
  initVaultMachineMode: () => invoke<void>('init_vault_machine_mode'),
  /** Switch the vault mode in place (no data loss). Pass a passphrase to go
   *  passphrase-mode; pass null to go machine-mode. Vault must be unlocked.
   *  Returns the new kind ("passphrase" | "machine"). */
  switchVaultMode: (newPassphrase: string | null) =>
    invoke<string>('switch_vault_mode', { newPassphrase }),
  /** "passphrase" or "machine". */
  vaultKind: () => invoke<string>('vault_kind'),
  vaultTrust: () => invoke<VaultTrust>('vault_trust'),
  listSessions: () => invoke<SessionListEntry[]>('list_sessions'),
  readSession: (sessionId: string) => invoke<SessionView>('read_session', { sessionId }),
  startRecording: (req: StartRequest) => invoke<RecordingSnapshot>('start_recording', { req }),
  pauseRecording: () => invoke<string>('pause_recording'),
  resumeRecording: () => invoke<string>('resume_recording'),
  /** Switch the recording mic mid-session (fail-safe). */
  switchRecordingMic: (sourceId: number) => invoke<void>('switch_recording_mic', { sourceId }),
  /** Mute/unmute the local mic mid-recording (record system audio only). */
  setMicMuted: (muted: boolean) => invoke<void>('set_mic_muted', { muted }),
  stopRecording: () => invoke<string>('stop_recording'),
  /** Asks the main window to finish the active recording through the same
   *  path as the record screen's Finish. */
  finishRecording: () => invoke<void>('finish_recording'),
  cancelRecording: () => invoke<void>('cancel_recording'),
  buildInfo: () => invoke<{ version: string; sha: string; tagged: boolean }>('build_info'),
  // macOS capture (microphone) TCC status: 0 not-determined, 1 granted,
  // 2 denied. Non-macOS always returns 1.
  setAiProvider: (provider: ProviderId | null) => invoke<void>('set_ai_provider', { provider }),
  /** This machine's settings block (seeded from legacy globals on first read). */
  machineSettings: () => invoke<MachineSettingsStatus>('machine_settings'),
  /** Whether translucent surfaces are allowed on this system. */
  reduceTransparency: () => invoke<boolean>('reduce_transparency'),
  /** Stores the fields `patch` carries on this machine. */
  updateMachineSettings: (patch: MachinePatch) =>
    invoke<MachineSettingsStatus>('update_machine_settings', { patch }),
  /** Theme file stems in <profile>/css (creates the dir + default.css). */
  listCssThemes: () => invoke<string[]>('list_css_themes'),
  /** A theme file's contents by stem. */
  readCssTheme: (name: string) => invoke<string>('read_css_theme', { name }),
  recordingSnapshot: () => invoke<RecordingSnapshot | null>('recording_snapshot'),
  /** The title a new recording carries when nobody typed one ("3:42 PM"). */
  defaultRecordingTitle: () => invoke<string>('default_recording_title'),
  /** Sets the machine's clock-cycle preference for the default recording title. */
  setClockHour12: (hour12: boolean) => invoke<void>('set_clock_hour12', { hour12 }),
  showMiniWindow: () => invoke<void>('show_mini_window'),
  showMainWindow: () => invoke<void>('show_main_window'),
  showReminderWindow: (title: string) => invoke<void>('show_reminder_window', { title }),
  reminderPayload: () => invoke<ReminderPayload | null>('reminder_payload'),
  // Every choice hides the popup; `start` records, `stop` stops the recording,
  // the snoozes and `never` write meeting_prompt.
  reminderAction: (choice: ReminderChoice) => invoke<void>('reminder_action', { choice }),
  /** Answers the `daisy://close/ask` prompt. Minimize and quit are
   *  remembered; cancel leaves the window open. */
  resolveClosePrompt: (choice: CloseChoice) => invoke<void>('resolve_close_prompt', { choice }),
  meetingDetectionSupport: () => invoke<MeetingDetectionSupport>('meeting_detection_support'),
  // On-device Whisper; `model` optionally overrides the ggml file path.
  transcribe: (req: { session_id: string; model?: string }) =>
    invoke<number>('transcribe', { req }),
  dedup: (req: { session_id: string }) =>
    invoke<DedupSummary>('dedup', { req }),
  polish: (req: { session_id: string; provider?: ProviderId; model?: string }) =>
    invoke<PolishSummary>('polish', { req }),
  readSettings: () => invoke<Settings>('read_settings'),
  /** Stores the fields `patch` carries in the profile's settings. */
  updateSettings: (patch: SettingsPatch) => invoke<Settings>('update_settings', { patch }),
  // Loopback MCP server (Settings → MCP).
  mcpStatus: () => invoke<McpStatus>('mcp_status'),
  mcpApply: () => invoke<McpStatus>('mcp_apply'),
  mcpRegenerateToken: () => invoke<McpStatus>('mcp_regenerate_token'),
  /** True if `port` is free for the MCP loopback server (or already ours). */
  mcpPortAvailable: (port: number) => invoke<boolean>('mcp_port_available', { port }),
  syncStatus: () => invoke<SyncStatus>('sync_status'),
  /** The firewall command that opens Mobile Sync's ports; null off Linux or with no firewall running. */
  linuxFirewallHint: () => invoke<FirewallHint | null>('linux_firewall_hint'),
  syncPairingBegin: () => invoke<PairingView>('sync_pairing_begin'),
  syncPairingCancel: () => invoke<void>('sync_pairing_cancel'),
  syncUnpair: (deviceId: string) => invoke<SyncStatus>('sync_unpair', { deviceId }),
  /** A blank name, or the phone's own, clears the rename. */
  syncRenameDevice: (deviceId: string, name: string) => invoke<SyncStatus>('sync_rename_device', { deviceId, name }),
  // ASR model management (Settings → Providers → Local transcription).
  listAsrModels: () => invoke<AsrModelInfo[]>('list_asr_models'),
  setActiveAsrModel: (id: string) => invoke<void>('set_active_asr_model', { id }),
  deleteAsrModel: (id: string) => invoke<void>('delete_asr_model', { id }),
  // Built-in summary model (Settings → Providers → On this computer).
  builtinModelStatus: () => invoke<BuiltinStatus>('builtin_model_status'),
  builtinModelDelete: () => invoke<void>('builtin_model_delete'),
  // First-run setup (the wizard's plan page).
  onboardingPlan: () => invoke<OnboardingPlan>('onboarding_plan'),
  /** Selects the planned engines, queues what is missing, returns the plan. */
  onboardingApply: () => invoke<OnboardingPlan>('onboarding_apply'),
  /** Marks first-run setup finished — the wizard's "Start using Daisy" button. */
  onboardingFinish: () => invoke<void>('onboarding_finish'),
  // The download queue (Settings → Providers, the wizard, the toast).
  downloadQueueStatus: () => invoke<DownloadJob[]>('download_queue_status'),
  downloadQueueEnqueue: (kind: DownloadKind, id: string) =>
    invoke<void>('download_queue_enqueue', { kind, id }),
  downloadQueueCancel: (id: string) => invoke<void>('download_queue_cancel', { id }),
  downloadQueueRetry: (id: string) => invoke<void>('download_queue_retry', { id }),
  listAudioSources: () => invoke<AudioSourceInfo[]>('list_audio_sources'),
  startMicMeter: (requestId: string, sourceId: number) =>
    invoke<void>('start_mic_meter', { requestId, sourceId }),
  stopMicMeter: (requestId: string) => invoke<void>('stop_mic_meter', { requestId }),
  // Per-device speech level (Settings → Recordings → Speech level).
  speechLevelsList: () => invoke<SpeechLevelInfo[]>('speech_levels_list'),
  speechLevelSetOverride: (device: string, dbfs: number | null) =>
    invoke<void>('speech_level_set_override', { device, dbfs }),
  /** Records ~8 s from the mic and stores the measured speech level. */
  calibrateSpeechLevel: (sourceId: number, device: string) =>
    invoke<void>('calibrate_speech_level', { sourceId, device }),
  listProviders: () => invoke<ProviderListEntry[]>('list_providers'),
  listProviderModels: (provider: ProviderId, apiKey: string | null, baseUrl: string | null) =>
    invoke<string[]>('list_provider_models', { provider, apiKey, baseUrl }),
  setProvider: (provider: ProviderId, config: ProviderConfigInput) =>
    invoke<void>('set_provider', { provider, config }),
  /** The context length a local server has loaded `model` with (LM Studio
   *  reports it; other providers answer null). */
  probeContextLength: (provider: ProviderId, baseUrl: string | null, model: string) =>
    invoke<number | null>('probe_context_length', { provider, baseUrl, model }),
  /** Generates the install keypair if needed and registers its public key. */
  registerGateway: () => invoke<void>('register_gateway'),
  lockVault: () => invoke<void>('lock_vault'),
  resetVault: () => invoke<void>('reset_vault'),
  moveProfile: (newPath: string) => invoke<void>('move_profile', { newPath }),
  /** Classify a candidate profile dir for the switch flow's dialogs. */
  probeProfileDir: (path: string) =>
    invoke<{ is_current: boolean; has_profile: boolean; empty: boolean }>('probe_profile_dir', { path }),
  /** Repoint the bootstrap at `path` and restart into it. Never resolves on
   *  success — the process restarts. */
  switchProfile: (path: string) => invoke<void>('switch_profile', { path }),
  recordingsStats: () => invoke<RecordingsStats>('recordings_stats'),
  recordingsDeleteAll: () => invoke<DeleteSummary>('recordings_delete_all'),
  sessionHasPlaybackAudio: (sessionId: string) => invoke<boolean>('session_has_playback_audio', { sessionId }),
  deleteSession: (sessionId: string) => invoke<void>('delete_session', { sessionId }),
  /** Drop every chunk that starts after `cutMs`, plus the transcript and
   *  summary derived from them. Irreversible. */
  /** Move everything after `cutMs` into a new session. Returns its id. */
  splitSessionAt: (sessionId: string, cutMs: number) =>
    invoke<{ new_session_id: string; chunks_moved: number; chunks_kept: number }>(
      'split_session_at', { sessionId, cutMs }),
  truncateSessionAfter: (sessionId: string, cutMs: number) =>
    invoke<{ chunks_removed: number; chunks_kept: number; duration_seconds: number }>(
      'truncate_session_after', { sessionId, cutMs }),
  /** Streaming Q&A: `onToken` fires per answer delta; resolves with the full
   *  answer + citations when complete. */
  qaAskStream: (query: string, onToken: (delta: string) => void): Promise<QaAnswer> => {
    const channel = new Channel<string>();
    channel.onmessage = onToken;
    return invoke<QaAnswer>('qa_ask_stream', { req: { query }, onToken: channel });
  },
  /** Streaming in-call chat: `onToken` fires per reply delta; resolves with the
   *  persisted thread when complete. */
  liveChatSendStream: (
    req: { session_id: string; user_text: string; transcript_tail: string; tail_end_ms: number },
    onToken: (delta: string) => void,
  ): Promise<LiveChatReply> => {
    const channel = new Channel<string>();
    channel.onmessage = onToken;
    return invoke<LiveChatReply>('live_chat_send_stream', { req, onToken: channel });
  },
  /** Load the persisted in-call chat thread for a session. */
  liveChatLoad: (sessionId: string) => invoke<CallChat>('live_chat_load', { sessionId }),
  /** Delete a session's in-call chat thread (recording/transcript untouched). */
  liveChatDelete: (sessionId: string) => invoke<void>('live_chat_delete', { sessionId }),
  /** Live speaker names for a stopped, still undiarized session; null otherwise. */
  readLiveNames: (sessionId: string) =>
    invoke<UndiarizedNames | null>('read_live_names', { sessionId }),
  listSessionSpeakers: (sessionId: string) =>
    invoke<SessionSpeaker[]>('list_session_speakers', { sessionId }),
  setSessionSpeakerLabel: (
    sessionId: string,
    clusterId: number,
    displayName: string,
    email?: string | null,
  ) =>
    invoke<void>('set_session_speaker_label', {
      sessionId,
      clusterId,
      displayName,
      email: email ?? null,
    }),
  sessionSpeakerSampleAudioBytes: (sessionId: string, clusterId: number) =>
    invoke<ArrayBuffer>('session_speaker_sample_audio_bytes', { sessionId, clusterId }),
  // Text of the same segments the sample-audio clip plays; shown in the
  // labeler modal.
  sessionSpeakerSampleText: (sessionId: string, clusterId: number) =>
    invoke<string>('session_speaker_sample_text', { sessionId, clusterId }),
  createNoteSession: (req: { title?: string | null; notes_md: string; tag_ids?: string[] }) =>
    invoke<string>('create_note_session', { req }),
  importAudioMeeting: (req: { title?: string | null; notes_md?: string; tag_ids?: string[]; audio_path: string; expected_speakers?: number | null }) =>
    invoke<{ session_id: string; quality_ok: boolean; quality_note: string; duration_secs: number }>('import_audio_meeting', { req }),
  listVoiceprints: () => invoke<VoiceprintView[]>('list_voiceprints'),
  renameVoiceprint: (id: string, displayName: string, email?: string | null) =>
    invoke<void>('rename_voiceprint', {
      req: { id, display_name: displayName, email: email ?? null },
    }),
  deleteVoiceprint: (id: string) => invoke<void>('delete_voiceprint', { id }),
  detachSpeakerVoiceprint: (sessionId: string, clusterId: number) =>
    invoke<void>('detach_speaker_voiceprint', {
      sessionId,
      clusterId,
    }),
  removeSpeakerCluster: (sessionId: string, clusterId: number) =>
    invoke<void>('remove_speaker_cluster', { sessionId, clusterId }),
  addSessionSpeaker: (
    sessionId: string,
    displayName: string,
    email: string | null,
    voiceprintId: string | null,
  ) => invoke<number>('add_session_speaker', {
    sessionId,
    displayName,
    email,
    voiceprintId,
  }),
  enrollVoiceprintFromSpeaker: (
    sessionId: string,
    clusterId: number,
    displayName: string,
    email?: string | null,
    allowMixed = false,
  ) =>
    invoke<EnrollResult>('enroll_voiceprint_from_speaker', {
      req: {
        session_id: sessionId,
        cluster_id: clusterId,
        display_name: displayName,
        email: email ?? null,
        allow_mixed: allowMixed,
      },
    }),
  rematchAllSessions: () =>
    invoke<{ sessions_scanned: number; clusters_matched: number }>(
      'rematch_all_sessions',
    ),
  voiceModelStatus: () => invoke<VoiceModelStatus>('voice_model_status'),
  diarizeSession: (sessionId: string, expectedSpeakers?: number | null, track?: DiarizeTrack | null) =>
    invoke<DiarizeResult>('diarize_session', { sessionId, expectedSpeakers: expectedSpeakers ?? null, track: track ?? null }),
  /** The count the last diarize run settled on and where it came from; null when no run recorded one. */
  diarizeCount: (sessionId: string) => invoke<DiarizeCount | null>('diarize_count', { sessionId }),
  /** The invitee count a diarize run on `track` would rest on; null when there are none. */
  diarizeAttendeeCount: (sessionId: string, track?: DiarizeTrack | null) =>
    invoke<number | null>('diarize_attendee_count', { sessionId, track: track ?? null }),
  loadSessionChapters: (sessionId: string) =>
    invoke<SessionChapters | null>('load_session_chapters', { sessionId }),
  listCalendarSubscriptions: () =>
    invoke<CalendarSubscription[]>('list_calendar_subscriptions'),
  addCalendarSubscription: (name: string, url: string) =>
    invoke<CalendarSubscription>('add_calendar_subscription', { req: { name, url } }),
  updateCalendarSubscription: (req: {
    id: string;
    name?: string;
    url?: string;
    enabled?: boolean;
    color_hex?: string;
    /** "" = clear the tag link (no auto-tagging). */
    tag_id?: string;
  }) =>
    invoke<CalendarSubscription>('update_calendar_subscription', { req }),
  deleteCalendarSubscription: (id: string) =>
    invoke<void>('delete_calendar_subscription', { id }),
  refreshCalendars: () => invoke<CalendarRefreshResult>('refresh_calendars'),
  listUpcomingEvents: (days?: number) =>
    invoke<CalendarEvent[]>('list_upcoming_events', { req: { days: days ?? null } }),
  dismissCalendarEvent: (subscriptionId: string, uid: string) =>
    invoke<void>('dismiss_calendar_event', { subscriptionId, uid }),
  extractSessionChapters: (req: { session_id: string; provider?: ProviderId; model?: string }) =>
    invoke<ChaptersResult>('extract_session_chapters', { req }),
  /** The AI tasks running right now. The backend runs one at a time and
   *  emits `ai-jobs:changed` on every change. */
  aiJobsList: () => invoke<AiJob[]>('ai_jobs_list'),
  /** Returns the meeting.opus bytes via the IPC bridge; macOS and Windows
   *  wrap them in a Blob URL. Linux plays the file URL from
   *  `sessionPlaybackAudioUrl`. */
  sessionPlaybackAudioBytes: (sessionId: string) => invoke<ArrayBuffer>('session_playback_audio_bytes', { sessionId }),
  /** The file:// URL of meeting.opus on Linux; null elsewhere. */
  sessionPlaybackAudioUrl: (sessionId: string) => invoke<string | null>('session_playback_audio_url', { sessionId }),
  // Opens the OS save dialog and writes there; resolves the chosen path, or null when cancelled.
  saveTextFile: (opts: { title: string; defaultFileName: string; filterName: string; extensions: string[] }, contents: string) =>
    invoke<string | null>('save_text_file', { ...opts, contents }),
  // Opens the OS audio picker; resolves the chosen path, or null when cancelled.
  // Opens the OS folder picker; the backend accepts only the folder last picked for `purpose`
  // ('profile-move' | 'profile-switch' | 'profile-new' | 'workflow-output').
  pickDirectory: (purpose: 'profile-move' | 'profile-switch' | 'profile-new' | 'workflow-output') =>
    invoke<string | null>('pick_directory', { purpose }),
  pickImportAudio: () => invoke<string | null>('pick_import_audio'),
  listIntegrations: () => invoke<IntegrationPublic[]>('list_integrations'),
  upsertIntegration: (req: UpsertIntegration) => invoke<IntegrationPublic>('upsert_integration', { req }),
  deleteIntegration: (id: string) => invoke<void>('delete_integration', { id }),
  integrationPush: (sessionId: string, integrationId: string) =>
    invoke<void>('integration_push', { sessionId, integrationId }),
  integrationHistory: (limit: number, skip: number) =>
    invoke<IntegrationHistoryEntry[]>('integration_history', { limit, skip }),
  // --- workflows ---
  workflowsList: () => invoke<Workflow[]>('workflows_list'),
  workflowUpsert: (req: Workflow) => invoke<Workflow>('workflow_upsert', { req }),
  workflowDelete: (id: string) => invoke<void>('workflow_delete', { id }),
  workflowHistory: (limit: number, skip: number) =>
    invoke<WorkflowRunRecord[]>('workflow_history_read', { limit, skip }),
  workflowQueueState: () => invoke<QueuedWorkflowRun[]>('workflow_queue_state'),
  // --- tags ---
  listTags: () => invoke<Tag[]>('list_tags'),
  searchTags: (query: string) => invoke<Tag[]>('search_tags', { query }),
  createTag: (req: { name: string; color_hex: string; prompt_md?: string | null; vocab_md?: string | null }) => invoke<Tag>('create_tag', { req }),
  updateTag: (req: { id: string; name?: string; color_hex?: string; prompt_md?: string | null; vocab_md?: string | null }) => invoke<Tag>('update_tag', { req }),
  deleteTag: (id: string, force: boolean) => invoke<{ dangling_session_count: number }>('delete_tag', { id, force }),
  // --- contacts (people) ---
  listContacts: () => invoke<Contact[]>('list_contacts'),
  // --- session metadata + notes ---
  sessionMetaGet: (sessionId: string) => invoke<SessionMeta>('session_meta_get', { sessionId }),
  sessionMetaUpdate: (req: SessionMetaUpdate) => invoke<void>('session_meta_update', { req }),
  sessionNotesLoad: (sessionId: string) => invoke<string>('session_notes_load', { sessionId }),
  sessionNotesSave: (sessionId: string, markdown: string) => invoke<void>('session_notes_save', { sessionId, markdown }),
  sessionAssignTags: (sessionId: string, tagIds: string[]) => invoke<void>('session_assign_tags', { sessionId, tagIds }),
  // --- tag suggestions ---
  tagSuggestPrerecord: (title: string | null, calendar: CalendarLink | null) =>
    invoke<LastTimeSuggestion | null>('tag_suggest_prerecord', { title, calendar }),
  tagSuggestForSession: (sessionId: string) =>
    invoke<SessionSuggestions>('tag_suggest_for_session', { sessionId }),
  sessionDismissTagSuggestions: (sessionId: string, tagIds: string[]) =>
    invoke<void>('session_dismiss_tag_suggestions', { sessionId, tagIds }),
  sessionUndismissTagSuggestions: (sessionId: string, tagIds: string[]) =>
    invoke<void>('session_undismiss_tag_suggestions', { sessionId, tagIds }),
  /** Atomic accept for the last-time chip: sets tagIds as the session's full
   *  tag set and dismisses dismissTagIds, in one manifest write. */
  sessionApplyTagSuggestions: (sessionId: string, tagIds: string[], dismissTagIds: string[]) =>
    invoke<void>('session_apply_tag_suggestions', { sessionId, tagIds, dismissTagIds }),
  // --- summary ---
  summaryLoad: (sessionId: string) => invoke<SessionSummary | null>('summary_load', { sessionId }),
  summarySaveEdit: (sessionId: string, markdown: string) => invoke<void>('summary_save_edit', { sessionId, markdown }),
  summaryRegenerate: (sessionId: string, promptId?: string) => invoke<SessionSummary>('summary_regenerate', { sessionId, promptId: promptId ?? null }),
  // --- prompts + analysis ---
  listPrompts: () => invoke<Prompt[]>('list_prompts'),
  savePrompt: (req: { id: string | null; name: string; directive_md: string; output: PromptOutput }) => invoke<Prompt>('save_prompt', { req }),
  deletePrompt: (id: string) => invoke<void>('delete_prompt', { id }),
  resetPrompt: (id: string) => invoke<Prompt>('reset_prompt', { id }),
  setDefaultSummaryPrompt: (id: string) => invoke<void>('set_default_summary_prompt', { id }),
  runAnalysis: (req: { session_id: string; prompt_id?: string; directive_md?: string }) => invoke<AnalysisResult>('run_analysis', { req }),
  analysisLoad: (sessionId: string, promptId: string) => invoke<AnalysisResult | null>('analysis_load', { sessionId, promptId }),
  // --- search ---
  searchSessions: (req: { query?: string; tag_ids?: string[]; contact_ids?: string[]; date_from?: number; date_to?: number }) => invoke<SessionHit[]>('search_sessions', { req }),
  // --- finalize cascade ---
  recordingFinalizeAndSummarize: (req: { session_id: string; summary_provider?: string; model?: string; skip_label_gate?: boolean }) => invoke<FinalizeOutcome>('recording_finalize_and_summarize', { req }),
  recordingResumeFinalize: (req: { session_id: string; summary_provider?: string; model?: string }) => invoke<FinalizeOutcome>('recording_resume_finalize', { req: { ...req, skip_label_gate: true } }),
  /** Read the finalize-recovery sidecar: attempt count + terminal-failed flag
   *  (set when finalize was given up after repeated crashes, e.g. low memory). */
  finalizeRecovery: (sessionId: string) =>
    invoke<{ attempts: number; failed: boolean; reason?: string | null; updated_at_unix: number }>('finalize_recovery', { sessionId }),
  /** Retry a given-up finalize: clears the attempt cap + re-kicks the
   *  session-finalize subprocess (call after freeing memory). */
  retryFinalize: (sessionId: string) => invoke<void>('retry_finalize', { sessionId }),
  /** Read the on-disk finalize status sidecar for a session (live progress +
   *  crash recovery). Returns null if no finalize has run for the session. */
  readFinalizeStatus: (sessionId: string) => invoke<FinalizeStatus | null>('read_finalize_status', { sessionId }),
  /** Committed live-caption segments, for showing the live transcript while
   *  finalize runs. */
  readLiveTranscript: (sessionId: string) =>
    invoke<{ track: string; start_ms: number; end_ms: number; text: string }[]>('read_live_transcript', { sessionId }),
  /** Marks a session "finalized" without running the cascade. Used by
   *  Regen-Transcript (and any other non-cascade recovery path) on an
   *  orphan; the "Finalizing X…" toast clears. Idempotent. */
  markSessionComplete: (sessionId: string) => invoke<void>('mark_session_complete', { sessionId }),
  /** Audits + rebuilds any missing derived files for a session (dedup,
   *  transcript.md, meeting.opus, and — when a provider is set — summary +
   *  chapters). Only rebuilds what's absent. Returns the repair count. */
  repairSession: (sessionId: string) => invoke<number>('repair_session', { sessionId }),
};

export type SummaryProviderStatusKind = 'Configured' | 'Missing' | 'Installing' | 'Unsupported' | 'VaultLocked' | 'Unreachable' | 'None';

export const copyToClipboard = (text: string) => clipboardWriteText(text);

/** Returns the value of `DAISY_PROFILE_DIR` if set and non-empty, else null.
 *  Used by the wizard to prefill the profile-dir field with the env override. */
export const envProfileDir = () => invoke<string | null>('env_profile_dir');
