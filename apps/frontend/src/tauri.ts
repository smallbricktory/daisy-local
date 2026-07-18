import { invoke, Channel } from '@tauri-apps/api/core';
import { writeText as clipboardWriteText } from '@tauri-apps/plugin-clipboard-manager';

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
}

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

/** Which track(s) to re-diarize. 'others' = system/attendees (default),
 *  'mic' = local mic (in-person), 'both' = system + mic. */
export type DiarizeTrack = 'others' | 'mic' | 'both';

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
}

export interface Contact {
  id: string;
  display_name: string;
  emails: string[];
  created_at_unix_seconds: number;
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
}

export interface SessionMetaUpdate {
  session_id: string;
  title?: string | null;        // undefined = leave; null = clear; string = set
  tag_ids?: string[];
  attendees?: Attendee[];
  created_at_unix_seconds?: number; // undefined = leave; user-corrected recording date
}

export interface ActionItem {
  text: string;
  owner?: string | null;
  due?: string | null;
}

export interface SummaryStructured {
  tldr: string;
  action_items: ActionItem[];
  decisions: string[];
  open_questions: string[];
  key_topics: string[];
}

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
  schema_version: number;
  session_id: string;
  provider: string;
  model: string;
  generated_at_unix_seconds: number;
  source_inputs_hash: string;
  structured: SummaryStructured;
  markdown: string;
  user_edited: boolean;
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
  stage: 'finalizing' | 'echo-cancelling' | 'transcribing' | 'deduping' | 'polishing' | 'awaiting-labels' | 'summarizing' | 'chaptering' | 'compressing' | 'resuming' | 'done' | 'error';
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
  stage: 'finalizing' | 'echo-cancelling' | 'transcribing' | 'deduping' | 'polishing' | 'awaiting-labels' | 'summarizing' | 'chaptering' | 'compressing' | 'resuming' | 'done' | 'error';
  progress: number;
  message: string | null;
  updated_at_unix: number;
}

export interface RecordingSnapshot {
  state: 'idle' | 'recording' | 'paused' | 'stopped';
  session_id: string;
  session_root: string;
  started_at_unix_seconds: number;
  /** Short label for the live transcription mode in use, e.g. "off", "ggml-base.en.bin". */
  live_mode_label: string;
}

/** Payload of the app-wide `recording:state` event. */
export type RecordingStateEvent = 'idle' | 'recording' | 'paused' | 'stopped';

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
  | 'daisy_gateway';

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
    default:               return p;
  }
}

export interface Settings {
  schema_version: number;
  default_mic_source_id: number | null;
  aec_mode_override: AecModeOverride;
  denoise_enabled: boolean;
  /** AI features provider. `null` means "no provider — use copy-paste". */
  default_summary_provider: ProviderId | null;
  /** Summary Style applied to new summaries. `null`/unknown → Daisy built-in. */
  default_summary_prompt_id: string | null;
  /** Optional side-load override for the Whisper ggml model. Bundled path
   *  is resolved via DAISY_WHISPER_MODEL_DIR by the backend when this is null. */
  whisper_model_path: string | null;
  user_display_name: string | null;
  /** Ordered nav-rail item keys (e.g. ["record","library",…]). Empty = default
   *  order. Settings is always anchored at the bottom and not included. */
  nav_order: string[];
  /** Check for app updates on launch + periodically (notify-only). */
  auto_update_check: boolean;
  /** Diagnostic logging level. 'off' = INFO+, 'basic' = DEBUG+, 'full' = DEBUG+
   *  with live-Whisper decode + audio gain/clip tracing. App restart required. */
  debug_level: 'off' | 'basic' | 'full';
  /** Known speaker count (0/null = auto-detect). Set to force exactly N. Applied
   *  on the next Diarize run — no restart needed. */
  diarize_max_speakers?: number | null;
  /** Diarizer the finalize/re-diarize path uses: "kmeans" or "speakrs".
   *  Platform-aware default (speakrs on macOS, k-means elsewhere). */
  diarizer?: string;
  /** Live-caption catch-up hop ladder (ms), ascending. settings.json only
   *  (no UI control). The field round-trips through save. */
  live_hop_ladder_ms?: number[] | null;
  /** Whole-UI zoom factor (1.0 = 100%), clamped 0.5..=2.0. */
  ui_zoom?: number;
  /** Seconds before a meeting's start to pop the reminder window (no UI; edit
   *  settings.json). Default 60; 0 disables. */
  reminder_lead_seconds?: number;
  /** Exposes the loopback MCP server; local MCP clients (Claude Code, …)
   *  query the meeting library (read-only). Off by default. */
  mcp_enabled?: boolean;
  /** Loopback MCP server port (no UI — edit settings.json). */
  mcp_port?: number;
  /** Allow MCP write tools (text-only session import). Off by default;
   *  separate, deliberate opt-in from `mcp_enabled`. */
  mcp_allow_write?: boolean;
}

export interface McpStatus {
  enabled: boolean;
  running: boolean;
  port: number;
  token: string;
  endpoint: string;
  /** Paste-ready `claude mcp add …` command (embeds the secret token). */
  claude_command: string;
}

export interface WhisperModelInfo {
  size: string;
  installed: boolean;
  active: boolean;
  /** Ships with the app — can't be deleted. */
  bundled: boolean;
  /** Multilingual (not an English-only `.en` build). */
  multilingual: boolean;
  size_bytes: number | null;
}

export interface WhisperDownloadProgress {
  request_id: string;
  downloaded: number;
  total: number | null;
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
}

export type LiveCaptionsChoice = 'auto' | 'on' | 'off';

/** Mirror of `hardware::LiveCaptionsResolution` + this machine's choice. */
export interface LiveCaptionsStatus {
  enabled: boolean;
  /** Where the decision came from. */
  source: 'override' | 'manual' | 'bench' | 'hardware';
  /** This machine's name — the key in live_captions_by_machine. */
  machine: string;
  /** Measured batch xRT, when a benchmark has run on this machine. */
  bench_xrt: number | null;
  choice: LiveCaptionsChoice;
}

export interface ProviderConfigInput {
  api_key: string | null;
  model: string | null;
  base_url: string | null;
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
  maybeRotateChunk: (intervalSecs: number) =>
    invoke<boolean>('maybe_rotate_chunk', { intervalSecs }),
  vaultStatus: () => invoke<VaultStatus>('vault_status'),
  initVault: (passphrase: string) => invoke<void>('init_vault', { passphrase }),
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
  cancelRecording: () => invoke<void>('cancel_recording'),
  buildInfo: () => invoke<{ version: string; sha: string; tagged: boolean }>('build_info'),
  // macOS capture (microphone) TCC status: 0 not-determined, 1 granted,
  // 2 denied. Non-macOS always returns 1.
  capturePermissionStatus: () => invoke<number>('capture_permission_status'),
  /** This machine's live-captions resolution + stored preference. */
  liveCaptionsStatus: () => invoke<LiveCaptionsStatus>('live_captions_status'),
  /** Stores this machine's live-captions preference. */
  setLiveCaptionsChoice: (choice: LiveCaptionsChoice) =>
    invoke<LiveCaptionsStatus>('set_live_captions_choice', { choice }),
  /** Runs the whisper speed benchmark (blocking, ~1 min on slow machines)
   *  and stores the verdict for this machine. */
  runLiveCaptionsBench: () => invoke<LiveCaptionsStatus>('run_live_captions_bench'),
  currentRecording: () => invoke<string | null>('current_recording'),
  recordingSnapshot: () => invoke<RecordingSnapshot | null>('recording_snapshot'),
  showMiniWindow: () => invoke<void>('show_mini_window'),
  showMainWindow: () => invoke<void>('show_main_window'),
  showReminderWindow: (title: string) => invoke<void>('show_reminder_window', { title }),
  reminderPayload: () => invoke<string | null>('reminder_payload'),
  // open=true → start recording the pending meeting; either way hides the popup.
  reminderAction: (open: boolean) => invoke<void>('reminder_action', { open }),
  // On-device Whisper; `model` optionally overrides the ggml file path.
  transcribe: (req: { session_id: string; model?: string }) =>
    invoke<number>('transcribe', { req }),
  dedup: (req: { session_id: string }) =>
    invoke<DedupSummary>('dedup', { req }),
  polish: (req: { session_id: string; provider?: ProviderId; model?: string }) =>
    invoke<PolishSummary>('polish', { req }),
  readSettings: () => invoke<Settings>('read_settings'),
  writeSettings: (settings: Settings) => invoke<void>('write_settings', { settings }),
  // Loopback MCP server (Settings → MCP).
  mcpStatus: () => invoke<McpStatus>('mcp_status'),
  mcpApply: () => invoke<McpStatus>('mcp_apply'),
  mcpRegenerateToken: () => invoke<McpStatus>('mcp_regenerate_token'),
  /** True if `port` is free for the MCP loopback server (or already ours). */
  mcpPortAvailable: (port: number) => invoke<boolean>('mcp_port_available', { port }),
  // Local Whisper model management (Settings → Providers → Advanced).
  listWhisperModels: () => invoke<WhisperModelInfo[]>('list_whisper_models'),
  setActiveWhisperModel: (size: string) => invoke<void>('set_active_whisper_model', { size }),
  deleteWhisperModel: (size: string) => invoke<void>('delete_whisper_model', { size }),
  downloadWhisperModel: (requestId: string, size: string) =>
    invoke<string>('download_whisper_model', { requestId, size }),
  cancelWhisperDownload: (requestId: string) =>
    invoke<void>('cancel_whisper_download', { requestId }),
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
  /** Enable Daisy Cloud: generate the install keypair (vault) if needed and
   *  register its public key with the license server. Call before selecting
   *  daisy_gateway as the provider. */
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
  qaAsk: (query: string) => invoke<QaAnswer>('qa_ask', { req: { query } }),
  /** Streaming Q&A: `onToken` fires per answer delta; resolves with the full
   *  answer + citations when complete. */
  qaAskStream: (query: string, onToken: (delta: string) => void): Promise<QaAnswer> => {
    const channel = new Channel<string>();
    channel.onmessage = onToken;
    return invoke<QaAnswer>('qa_ask_stream', { req: { query }, onToken: channel });
  },
  /** In-call chat: send a turn with the transcript delta since the last reply. */
  liveChatSend: (req: { session_id: string; user_text: string; transcript_tail: string; tail_end_ms: number }) =>
    invoke<LiveChatReply>('live_chat_send', { req }),
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
  rerenderSessionTranscript: (sessionId: string) =>
    invoke<void>('rerender_session_transcript', { sessionId }),
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
  ) =>
    invoke<EnrollResult>('enroll_voiceprint_from_speaker', {
      req: {
        session_id: sessionId,
        cluster_id: clusterId,
        display_name: displayName,
        email: email ?? null,
      },
    }),
  rematchAllSessions: () =>
    invoke<{ sessions_scanned: number; clusters_matched: number }>(
      'rematch_all_sessions',
    ),
  diarizeSession: (sessionId: string, expectedSpeakers?: number | null, track?: DiarizeTrack | null) =>
    invoke<{ speakers: number; segments_labeled: number }>('diarize_session', { sessionId, expectedSpeakers: expectedSpeakers ?? null, track: track ?? null }),
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
  /** Returns the meeting.opus bytes via the IPC bridge — frontend wraps these
   *  in a Blob URL. Bypasses asset://, which fails under WebKitGTK for paths
   *  containing `@` (Insync sync directories etc.). */
  sessionPlaybackAudioBytes: (sessionId: string) => invoke<ArrayBuffer>('session_playback_audio_bytes', { sessionId }),
  saveTextFile: (path: string, contents: string) => invoke<void>('save_text_file', { path, contents }),
  listIntegrations: () => invoke<IntegrationPublic[]>('list_integrations'),
  upsertIntegration: (req: UpsertIntegration) => invoke<IntegrationPublic>('upsert_integration', { req }),
  deleteIntegration: (id: string) => invoke<void>('delete_integration', { id }),
  integrationPush: (sessionId: string, integrationId: string) =>
    invoke<void>('integration_push', { sessionId, integrationId }),
  integrationHistory: (limit?: number) =>
    invoke<IntegrationHistoryEntry[]>('integration_history', { limit: limit ?? null }),
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
  /** Committed live-caption segments, for showing the live transcript while finalize runs. */
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


export type SummaryProviderStatusKind = 'Configured' | 'Missing' | 'VaultLocked' | 'Unreachable' | 'None';
export const summaryProviderStatus = () =>
  invoke<{ state: SummaryProviderStatusKind; provider: string | null; hint: string | null }>(
    'summary_provider_status',
  );

export const copyToClipboard = (text: string) => clipboardWriteText(text);

/** Returns the value of `DAISY_PROFILE_DIR` if set and non-empty, else null.
 *  Used by the wizard to prefill the profile-dir field with the env override. */
export const envProfileDir = () => invoke<string | null>('env_profile_dir');
