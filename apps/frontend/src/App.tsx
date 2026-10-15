// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { useCallback, useEffect, useRef, useState } from 'react';
import { getRecordingSnapshot, refreshRecordingSnapshot, releaseRecording, useRecordingState } from './lib/recordingState';
import { setSettings, settingsStore, useSettings } from './lib/settingsStore';
import { useAsrModels } from './asrModels';
import { ToastStack } from './components/NotificationToast';
import { DownloadToast } from './components/DownloadToast';
import { pushToast, dismissToast } from './lib/toastStore';
import { audioWarningToast } from './lib/audioWarningToast';
import { sleepNoticeText, sleptToast } from './lib/sleepNotice';
import { appendSleepMarker } from './liveTranscript';
import { syncRefusedToast } from './lib/syncRefused';
import { syncGatedToast } from './lib/syncGate';
import { clearAllImportStalls, dropImportSession, importStillGoing, syncImportToast, syncImportToastId, watchImportStall } from './lib/syncImport';
import { NavIcon } from './components/NavIcon';
import { GlobalConfirm } from './components/GlobalConfirm';
import { askCloseChoice } from './lib/closePrompt';
import { GatewayNoticeModal } from './components/GatewayNoticeModal';
import { FindBar } from './components/FindBar';
import { TopBanner } from './components/TopBanner';
import { Library } from './routes/Library';
import { ActiveSession } from './routes/ActiveSession';
import { Wizard } from './routes/Wizard';
import { Unlock } from './routes/Unlock';
import { Settings } from './routes/Settings';
import { Search } from './routes/Search';
import { History } from './routes/History';
import { Workflows } from './routes/Workflows';
import { Analyzer } from './routes/Analyzer';
import { Calendar, eventToSeed } from './routes/Calendar';
import { Consent } from './routes/Consent';
import { EulaGate } from './routes/EulaGate';
import { SessionStatusToasts } from './components/sessionStatusToasts';
import { WorkflowRunToasts } from './components/workflowRunToasts';
import { QaToast } from './components/qaToast';
import { SpeakerLabelModal } from './components/SpeakerLabelModal';
import { tauri, type AudioWarning, type RecordingSlept, type EventSeed, type CalendarEvent, type UpdateInfo, type LicenseStatus, type SessionFocus, type SyncRefused, type SyncGated, type SyncImport, type SyncWithdrawn } from './tauri';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { ProfileBlocked } from './routes/ProfileBlocked';
import { expiryBannerState } from './lib/expiry-banner';
import { expiredBannerState } from './lib/expired-banner';
import { trialBannerState, PRICING_URL, type TrialStage } from './lib/trial-banner';
import { beginFinalizeWatch } from './lib/sessionLifecycle';
import { runFinalize } from './lib/finalizeRunner';
import { finishActiveRecording } from './lib/finishRequest';
import { canNavigate } from './lib/navGuard';
import { initUiZoom } from './lib/uiZoomController';
import { followStoredTheme } from './lib/themeLoader';
import { providersPath, routeFallback, useNerdMode, visibleNavKeys, type NavKey } from './lib/nerdMode';
import type { JobKind } from './lib/sessionPhase';

const IGNORED_UPDATE_KEY = 'daisy:ignoredUpdate';

type Phase = 'loading' | 'eula' | 'consent' | 'welcome' | 'unlock' | 'profile_blocked' | 'main';

type Route =
  | { name: 'library'; sessionId?: string; focus?: SessionFocus }
  | { name: 'recording'; eventSeed?: EventSeed }
  | { name: 'search'; query?: string }
  | { name: 'history' }
  | { name: 'workflows' }
  | { name: 'analyzer' }
  | { name: 'calendar' }
  | { name: 'settings'; section?: string }
  | { name: 'wizard' };

// Reorderable nav-rail items. Settings is not listed; it stays anchored at
// the bottom. `record` uses the route name "recording".
export const NAV_DEFS: { key: NavKey; label: string; icon?: string; route?: Route; externalUrl?: string; recordStyle?: boolean }[] = [
  { key: 'record', label: 'Record', route: { name: 'recording' }, recordStyle: true },
  { key: 'library', label: 'Library', icon: 'library', route: { name: 'library' } },
  { key: 'calendar', label: 'Calendar', icon: 'calendar', route: { name: 'calendar' } },
  { key: 'search', label: 'Search', icon: 'search', route: { name: 'search' } },
  { key: 'tasks', label: 'Workflows', icon: 'tasks', route: { name: 'workflows' } },
  { key: 'analyzer', label: 'Analyzer', icon: 'analyzer', route: { name: 'analyzer' } },
  { key: 'history', label: 'History', icon: 'history', route: { name: 'history' } },
  { key: 'help', label: 'Help ↗', icon: 'help', externalUrl: 'https://www.daisylocal.app/help' },
];
const DEFAULT_NAV_ORDER = NAV_DEFS.map((d) => d.key);

export function App() {
  const [phase, setPhase] = useState<Phase>('loading');
  const [route, setRoute] = useState<Route>({ name: 'library' });
  // Drives whether the Calendar nav entry appears above Library. Set on
  // mount + refreshed any time the Settings → Calendars section is
  // touched (via the `refreshCalendarPresence` callback passed down).
  const [hasCalendar, setHasCalendar] = useState(false);
  const nerd = useNerdMode();

  // Nav-rail ordering (persisted in settings). dragKey tracks the item being
  // dragged for HTML5 drag-and-drop reordering.
  const navOrder = useSettings()?.nav_order ?? [];
  const [dragKey, setDragKey] = useState<NavKey | null>(null);
  // Item currently hovered as a drop target — drives the drop-indicator line.
  const [dragOverKey, setDragOverKey] = useState<NavKey | null>(null);

  // Available-update banner (notify-only). Null = nothing to show.
  const [update, setUpdate] = useState<UpdateInfo | null>(null);
  // Trial/license status — drives the expiry gate + trial banner.
  const [license, setLicense] = useState<LicenseStatus | null>(null);
  // Session-local dismissal of the "license expiring soon" banner. Not
  // persisted; the banner reappears on every launch.
  const [expiryBannerDismissed, setExpiryBannerDismissed] = useState(false);
  // Trial conversion banner: count of finalized sessions + per-stage dismissal
  // persisted in localStorage.
  const [meetingCount, setMeetingCount] = useState(0);
  const [trialDismissed, setTrialDismissed] = useState<TrialStage | null>(
    () => (localStorage.getItem('daisy:trial:dismissedStage') as TrialStage | null) ?? null,
  );

  // Active-recording state — drives the nav-rail "Record" label
  // (Record → In Progress → Paused). Sourced from the shared
  // useRecordingState() store, which subscribes to the backend's
  // `recording:snapshot` event.
  const snap = useRecordingState();
  const recState: 'recording' | 'paused' | null =
    snap && (snap.state === 'recording' || snap.state === 'paused')
      ? snap.state
      : null;

  /** Opens a session; the one being recorded opens on the recording screen. */
  const openSession = useCallback(
    (sessionId: string, focus?: SessionFocus) => {
      if (recState !== null && snap?.session_id === sessionId) {
        setRoute({ name: 'recording' });
        return;
      }
      setRoute({ name: 'library', sessionId, focus });
    },
    [recState, snap?.session_id],
  );
  // Latest openSession for the event listeners.
  const openSessionRef = useRef(openSession);
  openSessionRef.current = openSession;

  // The detail pane for the session being recorded moves to the recording screen.
  useEffect(() => {
    if (recState === null || !snap) return;
    if (route.name === 'library' && route.sessionId === snap.session_id) {
      setRoute({ name: 'recording' });
    }
  }, [recState, snap, route]);

  // Finalize progress + done/needs-labels state is projected onto the toast
  // stack by <SessionStatusToasts> (sidecar-polled via the lifecycle store).

  // Diarize toast tracker — driven by daisy:diarize-start/done CustomEvents
  // emitted by lib/diarizeBus. The toast stays on screen across navigation.
  const [diarizing, setDiarizing] = useState<{ sessionId: string; title: string | null }[]>([]);
  const [recentlyDiarized, setRecentlyDiarized] = useState<{ sessionId: string; title: string | null; ok: boolean; at: number }[]>([]);
  useEffect(() => {
    const onStart = (ev: Event) => {
      const d = (ev as CustomEvent<{ sessionId: string; title: string | null }>).detail;
      setDiarizing((cur) => cur.some((x) => x.sessionId === d.sessionId)
        ? cur
        : [...cur, { sessionId: d.sessionId, title: d.title }]);
    };
    const onDone = (ev: Event) => {
      const d = (ev as CustomEvent<{ sessionId: string; ok: boolean }>).detail;
      setDiarizing((cur) => {
        const match = cur.find((x) => x.sessionId === d.sessionId);
        if (match) {
          setRecentlyDiarized((rec) => [
            ...rec.filter((r) => r.sessionId !== d.sessionId),
            { sessionId: d.sessionId, title: match.title, ok: d.ok, at: Date.now() },
          ]);
        }
        return cur.filter((x) => x.sessionId !== d.sessionId);
      });
    };
    window.addEventListener('daisy:diarize-start', onStart as EventListener);
    window.addEventListener('daisy:diarize-done', onDone as EventListener);
    return () => {
      window.removeEventListener('daisy:diarize-start', onStart as EventListener);
      window.removeEventListener('daisy:diarize-done', onDone as EventListener);
    };
  }, []);
  // Auto-dismiss "diarized" success/error toasts after 6s.
  useEffect(() => {
    if (recentlyDiarized.length === 0) return;
    const id = window.setTimeout(() => {
      const cutoff = Date.now() - 6000;
      setRecentlyDiarized((prev) => prev.filter((p) => p.at >= cutoff));
    }, 6500);
    return () => window.clearTimeout(id);
  }, [recentlyDiarized]);

  // Record-only banner: no ASR model resolves on this machine, so none of the
  // rows is the active one.
  const asrModels = useAsrModels();
  const noAsrModel = phase === 'main' && asrModels !== null && !asrModels.some((m) => m.active);

  // Dead-mic warning: `recording:mic-silent` shows a persistent warning toast.
  // It stays suppressed while the supervisor's hold-and-retry owns the dead mic.
  const micRetryingRef = useRef(false);
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    void listen('recording:mic-silent', () => {
      if (micRetryingRef.current) return;
      pushToast({
        id: 'mic-silent',
        severity: 'warning',
        title: 'Your mic isn’t being recorded',
        body: 'No microphone signal detected. Another app may be using the mic. Stop and restart this recording — or start Daisy before joining the call.',
      });
    }).then((fn) => { unlisten = fn; });
    return () => { unlisten?.(); };
  }, []);

  // Topology warnings from the supervisor: device failover, external mute,
  // very-quiet mic, exhausted reconnects. One toast per kind, mapped
  // in audioWarningToast. (Mic silence has its own channel: the
  // recording:mic-silent event.) The payload is the serialized
  // recording::supervisor::Notice enum.
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    void listen<AudioWarning>('audio-warning', (ev) => {
      if (
        ev.payload.kind === 'mic-retrying' ||
        ev.payload.kind === 'mic-dead-air' ||
        ev.payload.kind === 'mic-held-by-call'
      ) {
        micRetryingRef.current = true;
        dismissToast('mic-silent');
      } else if (ev.payload.kind === 'mic-recovered') {
        micRetryingRef.current = false;
        dismissToast('mic-silent');
      }
      pushToast(audioWarningToast(ev.payload));
    }).then((fn) => { unlisten = fn; });
    return () => { unlisten?.(); };
  }, []);

  // A system sleep paused the recording: a toast, and the same notice as a
  // marker in the live transcript.
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    void listen<RecordingSlept>('recording:slept', (ev) => {
      const s = ev.payload;
      pushToast(sleptToast(s));
      appendSleepMarker(s.session_id, sleepNoticeText(s.paused_at_unix_seconds, s.resumed_at_unix_seconds));
    }).then((fn) => { unlisten = fn; });
    return () => { unlisten?.(); };
  }, []);

  // Stop from the mini window or the meeting-ended reminder: the same finish
  // the record screen's Finish button runs.
  const handleProcessingStartedRef = useRef(handleProcessingStarted);
  handleProcessingStartedRef.current = handleProcessingStarted;
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let cancelled = false;
    void listen('daisy://recording/finish', () => {
      const sid = getRecordingSnapshot()?.session_id;
      if (!sid) return;
      finishActiveRecording(sid, (id, title) => handleProcessingStartedRef.current(id, title));
    }).then((fn) => { if (cancelled) fn(); else unlisten = fn; });
    return () => { cancelled = true; unlisten?.(); };
  }, []);

  // Closing the window with no remembered choice: the backend holds the
  // close until the prompt is answered.
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let cancelled = false;
    void listen('daisy://close/ask', () => {
      void askCloseChoice().catch((e) => console.warn('close prompt:', e));
    }).then((fn) => {
      if (cancelled) { fn(); return; }
      unlisten = fn;
      // Clears a prompt flag left by an earlier page load.
      void tauri.resolveClosePrompt('cancel').catch(() => {});
    });
    return () => { cancelled = true; unlisten?.(); };
  }, []);

  // A quick start that began recording: hotkey or meeting prompt. The
  // window is already forward; this lands on the live transcript regardless
  // of whatever screen was open when the shortcut fired.
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    void listen('hotkey:started', () => {
      setRoute({ name: 'recording' });
    }).then((fn) => { unlisten = fn; });
    return () => { unlisten?.(); };
  }, []);

  // A quick start found no microphone: hotkey or meeting prompt. The window
  // is already forward; this lands on the picker and names the reason.
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    void listen('hotkey:no-mic', () => {
      setRoute({ name: 'recording' });
      pushToast({
        id: 'hotkey-no-mic',
        severity: 'warning',
        title: 'No microphone found',
        body: 'Plug one in, then pick it here to start recording.',
        autoDismissMs: 12000,
      });
    }).then((fn) => { unlisten = fn; });
    return () => { unlisten?.(); };
  }, []);

  // A quick start that could not begin: hotkey or meeting prompt (device
  // busy, permission denied, trial expired, a start already in flight). The
  // payload is the error's plain-language text.
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    void listen<string>('hotkey:start-failed', (ev) => {
      pushToast({
        id: 'hotkey-start-failed',
        severity: 'warning',
        title: 'Recording did not start',
        body: ev.payload,
        autoDismissMs: 12000,
      });
    }).then((fn) => { unlisten = fn; });
    return () => { unlisten?.(); };
  }, []);

  // A phone session this computer gave up on: sealed to a key it does not
  // hold, written by a newer Daisy, or holding audio that does not decode.
  // Settings → Mobile Sync lists it too.
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    void listen<SyncRefused>('sync:refused', (ev) => {
      const left = dropImportSession(ev.payload.device_id, ev.payload.session, (sessionId) => {
        openSessionRef.current(sessionId);
        window.dispatchEvent(new CustomEvent('daisy:show-summary', { detail: { sessionId } }));
      });
      if (left) pushToast(left);
      else dismissToast(syncImportToastId(ev.payload.device_id));
      pushToast(syncRefusedToast(ev.payload));
    }).then((fn) => { unlisten = fn; });
    return () => { unlisten?.(); };
  }, []);

  // A phone deleted a session it was sending: nothing more of it is coming,
  // so the import toast stops waiting for it.
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    void listen<SyncWithdrawn>('sync:withdrawn', (ev) => {
      if (!importStillGoing(ev.payload.device_id, ev.payload.session)) return;
      const left = dropImportSession(ev.payload.device_id, ev.payload.session, (sessionId) => {
        openSessionRef.current(sessionId);
        window.dispatchEvent(new CustomEvent('daisy:show-summary', { detail: { sessionId } }));
      });
      if (left) pushToast(left);
      else dismissToast(syncImportToastId(ev.payload.device_id));
    }).then((fn) => { unlisten = fn; });
    return () => { unlisten?.(); };
  }, []);

  // A paired device on another sync version: nothing was synced, and the
  // toast names which side to update. Settings → Mobile Sync shows it too.
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    void listen<SyncGated>('sync:gated', (ev) => {
      pushToast(syncGatedToast(ev.payload));
    }).then((fn) => { unlisten = fn; });
    return () => { unlisten?.(); };
  }, []);

  // A phone recording on its way in: a toast tracks it until the library entry exists.
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    void listen<SyncImport>('sync:import', (ev) => {
      watchImportStall(ev.payload);
      const toast = syncImportToast(ev.payload, (sessionId) => {
        openSessionRef.current(sessionId);
        window.dispatchEvent(new CustomEvent('daisy:show-summary', { detail: { sessionId } }));
      });
      if (toast) pushToast(toast);
    }).then((fn) => { unlisten = fn; });
    return () => { unlisten?.(); clearAllImportStalls(); };
  }, []);

  // Clears the mic-silent warning while paused; the backend re-checks after resume.
  useEffect(() => {
    const st = snap?.state;
    if (st === 'paused' || st === 'stopped' || !st) {
      micRetryingRef.current = false;
      dismissToast('mic-silent');
      dismissToast('audio-mic-very-quiet');
      dismissToast('audio-mic-retrying');
      dismissToast('audio-system-lost');
    }
  }, [snap?.state]);

  // Saved order first (known keys only), then any defaults not yet listed.
  const orderedNavKeys = (() => {
    const known = new Set<string>(DEFAULT_NAV_ORDER);
    const saved = navOrder
      .map((k) => (k === 'coaching' ? 'analyzer' : k))
      .filter((k): k is NavKey => known.has(k));
    const rest = DEFAULT_NAV_ORDER.filter((k) => !saved.includes(k));
    return [...saved, ...rest];
  })();

  function persistNavOrder(order: string[]) {
    // A refused write puts the stored order back on the rail.
    void setSettings({ nav_order: order }).catch(() => {});
  }

  function onNavDrop(targetKey: NavKey) {
    const from = dragKey === null ? -1 : orderedNavKeys.indexOf(dragKey);
    const to = orderedNavKeys.indexOf(targetKey);
    setDragKey(null);
    setDragOverKey(null);
    if (from < 0 || to < 0 || from === to) return;
    const next = orderedNavKeys.slice();
    const [moved] = next.splice(from, 1);
    next.splice(to, 0, moved);
    void persistNavOrder(next);
  }

  async function refreshCalendarPresence() {
    try {
      const subs = await tauri.listCalendarSubscriptions();
      setHasCalendar(subs.length > 0);
    } catch { /* ignored */ }
  }
  // Finalize/cascade progress is surfaced by <SessionStatusToasts>, which reads
  // the lifecycle store. `beginFinalizeWatch` arms that store's sidecar poll;
  // these handlers only arm the watch + navigate. The toast reflects progress.

  // Finish & summarize from the recording screen: arms the sidecar poll, starts
  // the cascade (stop_recording + finalize), and steps back to the library. The
  // session-status toast shows progress from the sidecar.
  function handleProcessingStarted(sessionId: string, title: string) {
    releaseRecording(sessionId);
    beginFinalizeWatch(sessionId, title);
    void runFinalize(sessionId, 'cascade');
    setRoute({ name: 'library', sessionId });
  }

  /** Manual Summarize / Regen from the Library SummaryPane. The session is
   *  already stopped (not the active recording slot); the stop prelude is skipped.
   *  Arms the watch + runs the chosen job kind; the session-status toast reflects it. */
  function handleSummarizeStarted(sessionId: string, title: string, kind: JobKind = 'cascade', promptId?: string) {
    // Only the full cascade writes a finalize.status.json sidecar.
    if (kind === 'cascade') beginFinalizeWatch(sessionId, title);
    void runFinalize(sessionId, kind, /* alreadyStopped */ true, promptId);
  }

  // Resolve startup phase: welcome (first run) / unlock (vault exists, locked)
  // / main (vault unlocked or no vault yet).
  async function resolvePhase() {
    // Legal gate first — Terms of Service + Privacy acceptance. Machine-local,
    // shows once per install and re-prompts when a document version bumps.
    if (!(await tauri.eulaStatus())) {
      setPhase('eula');
      return;
    }
    // Recording-consent gate next — machine-local, shows once per install
    // regardless of bootstrap/vault state.
    if (!(await tauri.consentStatus())) {
      setPhase('consent');
      return;
    }
    const bs = await tauri.bootstrapStatus();
    if (!bs.has_bootstrap) {
      setPhase('welcome');
      return;
    }
    const vs = await tauri.vaultStatus();
    if (!vs.vault_exists) {
      // Bootstrap exists but vault doesn't — treat as a partial first-run.
      setPhase('welcome');
      return;
    } else if (!vs.unlocked) {
      setPhase('unlock');
      return;
    }
    // Vault unlocked — gate on trial/license.
    const lic = await tauri.licenseStatus().catch(() => null);
    setLicense(lic);
    // Profile binding: a trial install is blocked from another install's data.
    const binding = await tauri.profileBindingCheck().catch(() => ({ state: 'ok' as const }));
    if (binding.state === 'foreign') {
      setPhase('profile_blocked');
      return;
    }
    setPhase('main');
  }

  useEffect(() => {
    resolvePhase().catch(console.error);
  }, []);

  // Apply the persisted UI zoom and theme file, follow them, and install the
  // Cmd/Ctrl +/-/0 shortcuts (once).
  useEffect(() => { initUiZoom(); followStoredTheme(); }, []);

  // Tells the backend this machine's clock-cycle preference, read from the
  // OS locale, so the default recording title matches it.
  useEffect(() => {
    tauri.setClockHour12(Intl.DateTimeFormat().resolvedOptions().hour12 ?? true).catch(() => {});
  }, []);

  // A route the switch just hid cannot stay on screen.
  useEffect(() => {
    const to = routeFallback(nerd, route.name);
    if (to) setRoute({ name: to });
  }, [nerd, route.name]);

  const openProviders = () => setRoute({ name: 'settings', section: 'Providers' });

  // Once we're in 'main', also pick up an active recording if any.
  useEffect(() => {
    if (phase !== 'main') return;
    let cancelled = false;
    void refreshRecordingSnapshot().catch(() => {}).then(() => {
      const s = getRecordingSnapshot()?.state;
      if (!cancelled && (s === 'recording' || s === 'paused')) {
        setRoute({ name: 'recording' });
      }
    });
    refreshCalendarPresence();
    // Trial-banner copy: count finalized sessions once.
    tauri.listSessions().then((s) => { if (!cancelled) setMeetingCount(s.length); }).catch(() => { /* ignored */ });
    return () => { cancelled = true; };
  }, [phase]);

  // Background calendar refresh every 10 minutes; runs at the App level
  // regardless of which page is showing. The backend refresh_calendars
  // command runs to completion in spawn_blocking; the Calendar route re-reads
  // the cache when next opened. Only fires when at least one subscription
  // exists.
  useEffect(() => {
    if (phase !== 'main') return;
    let stop = false;
    async function tick() {
      try {
        const subs = await tauri.listCalendarSubscriptions();
        if (stop || subs.length === 0) return;
        await tauri.refreshCalendars();
      } catch { /* errors surface in Calendar / Settings views */ }
    }
    // First tick shortly after entering main, then every 10 min.
    const kickoff = window.setTimeout(tick, 15_000);
    const id = window.setInterval(tick, 10 * 60 * 1000);
    return () => { stop = true; window.clearTimeout(kickoff); window.clearInterval(id); };
  }, [phase]);

  // Update check (notify-only): on launch + every 6h while in the app, if the
  // user left auto-update on. Soft-fails offline / before the manifest exists.
  useEffect(() => {
    if (phase !== 'main') return;
    let stop = false;
    async function run() {
      // License heartbeat; runs independently of the update-check preference.
      tauri.licenseCheckin().then((s) => { if (!stop) setLicense(s); }).catch(() => {});
      try {
        if (!settingsStore.get()?.auto_update_check) return;
        const info = await tauri.checkForUpdate();
        if (stop || !info.update_available) return;
        if (localStorage.getItem(IGNORED_UPDATE_KEY) === info.latest) return;
        setUpdate(info);
      } catch { /* offline / manifest not published — ignore */ }
    }
    const kickoff = window.setTimeout(run, 4000);
    const id = window.setInterval(run, 6 * 60 * 60 * 1000);
    return () => { stop = true; window.clearTimeout(kickoff); window.clearInterval(id); };
  }, [phase]);

  const gate =
    phase === 'loading' ? <p className="meta" style={{ padding: 32 }}>Loading...</p>
    : phase === 'eula' ? <EulaGate onAccepted={() => resolvePhase()} />
    : phase === 'consent' ? <Consent onAccepted={() => resolvePhase()} />
    : phase === 'welcome' ? <Wizard mode="first-run" onComplete={() => { void tauri.onboardingFinish(); resolvePhase(); }} />
    : phase === 'unlock' ? <Unlock onUnlocked={() => resolvePhase()} />
    : phase === 'profile_blocked' ? <ProfileBlocked onResolved={() => resolvePhase()} />
    : null;
  if (gate) return <>{gate}<GlobalConfirm /></>;

  const visibleNav = new Set<string>(visibleNavKeys(nerd, hasCalendar));
  const expiredBanner = expiredBannerState(license);

  return (
    <div className="app" data-recording={recState ?? undefined}>
      {recState !== null && <div className="rec-edge" aria-hidden="true" />}
      <aside className="rail">
        <div className="brand-bar">
          <div className="brand-mark" aria-hidden="true" />
          <div className="brand-wordmark">
            <span className="wm-name">Daisy</span>
          </div>
        </div>
        {orderedNavKeys.map((key) => {
          const def = NAV_DEFS.find((d) => d.key === key);
          if (!def) return null;
          if (!visibleNav.has(key)) return null;
          const active = def.route != null && route.name === def.route.name;
          const cls = def.recordStyle
            ? `nav-item nav-item--record ${active ? 'active' : ''}`
            : `nav-item ${active ? 'active' : ''}`;
          return (
            <button
              key={key}
              data-testid={`nav-${def.key}`}
              className={[
                cls,
                dragKey === key ? 'nav-item--dragging' : '',
                dragOverKey === key && dragKey !== null && dragKey !== key ? 'nav-item--drop-before' : '',
              ].filter(Boolean).join(' ')}
              draggable
              onDragStart={(e) => {
                setDragKey(key);
                // Some webviews won't begin a drag unless dataTransfer is set.
                e.dataTransfer.effectAllowed = 'move';
                e.dataTransfer.setData('text/plain', key);
              }}
              onDragOver={(e) => {
                e.preventDefault();
                e.dataTransfer.dropEffect = 'move';
                if (dragOverKey !== key) setDragOverKey(key);
              }}
              onDragLeave={() => { if (dragOverKey === key) setDragOverKey(null); }}
              onDrop={(e) => { e.preventDefault(); onNavDrop(key); }}
              onDragEnd={() => { setDragKey(null); setDragOverKey(null); }}
              onClick={() => {
                if (def.externalUrl) { void tauri.openExternal(def.externalUrl); return; }
                const route_ = def.route!;
                void canNavigate().then((ok) => { if (ok) setRoute(route_); });
              }}
              title="Drag to reorder"
            >
              {def.recordStyle && (
                <span
                  className={'nav-item--record__dot' + (recState === 'recording' ? ' nav-item--record__dot--live' : '')}
                  aria-hidden="true"
                />
              )}
              {def.icon && <NavIcon name={def.icon} />}
              <span>{
                def.recordStyle
                  ? (recState === 'recording' ? 'IN PROGRESS' : recState === 'paused' ? 'PAUSED' : def.label)
                  : def.label
              }</span>
            </button>
          );
        })}
        <div
          style={{ flex: 1 }}
          // Accepts drops in the empty gap below the items; the cursor stays
          // "move" and a drop here sends the item to the end of the list.
          onDragOver={(e) => { if (dragKey !== null) { e.preventDefault(); e.dataTransfer.dropEffect = 'move'; } }}
          onDrop={(e) => {
            if (dragKey === null) return;
            e.preventDefault();
            const last = orderedNavKeys[orderedNavKeys.length - 1];
            if (last) onNavDrop(last);
          }}
        />
        <button data-testid="nav-mini" className="nav-item" title="Minimize to floating mini-window"
                onClick={() => tauri.showMiniWindow().catch(() => {})}>
          <NavIcon name="mini" /><span>Mini</span>
        </button>
        <button data-testid="nav-settings" className={`nav-item ${route.name === 'settings' ? 'active' : ''}`}
                onClick={() => { void canNavigate().then((ok) => { if (ok) setRoute({ name: 'settings' }); }); }}>
          <NavIcon name="settings" /><span>Settings</span>
        </button>
      </aside>
      <main className="main">
        {(() => {
          if (expiredBanner.kind !== 'show') return null;
          return (
            <TopBanner
              kind="warning"
              testId="banner-expired"
              actions={
                <button
                  data-testid="banner-activate"
                  className="btn btn--primary"
                  onClick={() => setRoute({ name: 'settings', section: 'About' })}
                >
                  Activate
                </button>
              }
            >
              {expiredBanner.label}
            </TopBanner>
          );
        })()}
        {(() => {
          if (expiredBanner.kind === 'show') return null;
          const trialLic = license?.state === 'trial'
            ? { state: 'trial' as const, days_left: license.days_left }
            : null;
          const tb = trialBannerState(trialLic, { meetingCount }, trialDismissed);
          if (tb.kind !== 'show') return null;
          const onDismiss = () => {
            localStorage.setItem('daisy:trial:dismissedStage', tb.stage);
            setTrialDismissed(tb.stage);
          };
          return (
            <TopBanner
              kind={tb.bannerKind}
              onDismiss={onDismiss}
              actions={
                <button data-testid="banner-keep-daisy" className="btn btn--primary"
                  onClick={() => { void tauri.openExternal(PRICING_URL); }}>
                  Keep Daisy
                </button>
              }
            >
              {tb.label}
            </TopBanner>
          );
        })()}
        {(() => {
          const bannerState = expiryBannerState(license, Math.floor(Date.now() / 1000), expiryBannerDismissed);
          if (bannerState.kind === 'hidden') return null;
          const { label } = bannerState;
          return (
            <TopBanner
              kind="warning"
              onDismiss={() => setExpiryBannerDismissed(true)}
              actions={
                <button
                  data-testid="banner-resubscribe"
                  className="btn btn--primary"
                  onClick={() => { void tauri.openExternal('https://www.daisylocal.app/subscribe'); }}
                >Resubscribe</button>
              }
            >
              <strong>{label}</strong>{' '}
              <span className="meta">
                Auto-renew runs daily on launch. If your subscription is active and this stays, check Settings → About.
              </span>
            </TopBanner>
          );
        })()}
        {noAsrModel && (
          <TopBanner
            kind="warning"
            actions={
              <button
                data-testid="banner-open-settings"
                className="btn btn--primary"
                onClick={openProviders}
              >
                Open Settings
              </button>
            }
          >
            <strong>No transcription model installed.</strong>{' '}
            <span className="meta">
              Recordings are audio-only until one is installed in {providersPath()}.
            </span>
          </TopBanner>
        )}
        {update && (
          <TopBanner
            kind="info"
            onDismiss={() => { localStorage.setItem(IGNORED_UPDATE_KEY, update.latest); setUpdate(null); }}
          >
            <strong>Daisy {update.latest} is available.</strong>{' '}
            <span className="meta">
              You're currently on {update.current}. Release notes and download link available{' '}
              <a
                data-testid="app-open-changelog"
                href="https://www.daisylocal.app/changelog"
                onClick={(e) => { e.preventDefault(); void tauri.openExternal('https://www.daisylocal.app/changelog'); }}
              >
                here
              </a>.
            </span>
          </TopBanner>
        )}
        <div data-testid="app-body" className={'main__body' + (route.name === 'wizard' ? ' main__body--scroll' : '')}>
        {route.name === 'library' && (
          <Library
            selectedSessionId={route.sessionId}
            recordingSessionId={recState !== null ? snap?.session_id : undefined}
            focus={route.focus}
            onSelect={openSession}
            onStartRecording={() => setRoute({ name: 'recording' })}
            onStartSummarize={handleSummarizeStarted}
            processingSessionIds={[]}
            onNavigateToProviders={openProviders}
            license={license}
          />
        )}
        {route.name === 'recording' && (
          <ActiveSession
            onProcessingStarted={handleProcessingStarted}
            eventSeed={route.eventSeed}
            onDiscarded={() => setRoute({ name: 'library' })}
            license={license}
          />
        )}
        {route.name === 'calendar' && (
          <Calendar
            onStartRecording={(seed) => setRoute({ name: 'recording', eventSeed: seed })}
          />
        )}
        {route.name === 'search' && (
          <Search
            initialQuery={route.name === 'search' ? route.query : undefined}
            onOpenSession={openSession}
            onNavigateToProviders={openProviders}
          />
        )}
        {route.name === 'history' && (
          <History onOpenSession={openSession} onOpenWorkflows={() => setRoute({ name: 'workflows' })} />
        )}
        {route.name === 'workflows' && <Workflows />}
        {route.name === 'analyzer' && (
          <Analyzer
            onOpenSession={openSession}
            onNavigateToProviders={openProviders}
            onStartRecording={() => setRoute({ name: 'recording' })}
          />
        )}
        {route.name === 'settings' && (
          <Settings
            onLocked={() => resolvePhase()}
            onLaunchWizard={() => setRoute({ name: 'wizard' })}
            onCalendarsChanged={refreshCalendarPresence}
            initialSection={route.name === 'settings' ? route.section : undefined}
            onLicenseChanged={() => { void tauri.licenseStatus().then(setLicense).catch(() => {}); }}
          />
        )}
        {route.name === 'wizard' && (
          <Wizard
            mode="reconfigure"
            onComplete={() => setRoute({ name: 'settings' })}
            onCancel={() => setRoute({ name: 'settings' })}
          />
        )}
        </div>
      </main>
      <SessionStatusToasts
        onOpenRecording={() => setRoute({ name: 'recording' })}
        onRecordScreen={route.name === 'recording'}
        onOpenSession={(sessionId) => {
          openSession(sessionId);
          // Force the summary tab even if we're already on this session's detail
          // page (e.g. looking at Participants) — the SummaryPane listens.
          window.dispatchEvent(new CustomEvent('daisy:show-summary', { detail: { sessionId } }));
        }}
        onResume={(sessionId, title) => {
          // Interrupted finalize: clear the recovery attempt cap and re-run the
          // in-app orphan finalize, then arm the progress poll.
          void tauri.retryFinalize(sessionId).then(() => beginFinalizeWatch(sessionId, title));
        }}
        onCopyPrompt={(sessionId) => {
          // Copy-with-prompt needs the loaded transcript (lives in SummaryPane);
          // routes to the meeting where the Copy control is.
          openSession(sessionId);
        }}
        onWantAi={() => { void tauri.openExternal('https://www.daisylocal.app/help/getting-an-api-key'); }}
      />
      <WorkflowRunToasts onOpenHistory={() => setRoute({ name: 'history' })} />
      <SpeakerLabelModal onChanged={() => { /* SpeakerLabeler's own onChanged + diarize bus handle refresh */ }} />
      <ToastStack />
      <GlobalConfirm />
      <GatewayNoticeModal />
      <AppToastDriver
        diarizing={diarizing}
        recentlyDiarized={recentlyDiarized}
        onOpenSession={(id) => {
          openSession(id);
          window.dispatchEvent(new CustomEvent('daisy:show-summary', { detail: { sessionId: id } }));
        }}
      />
      <QaToast onOpenSearch={() => setRoute({ name: 'search' })} />
      <DownloadToast />
      <FindBar />
      <MeetingReminder
        hasCalendar={hasCalendar}
        onStart={(seed) => setRoute({ name: 'recording', eventSeed: seed })}
      />
    </div>
  );
}

/** Pops the meeting-reminder popup at `start − reminder_lead_seconds` for the
 *  soonest upcoming calendar event, and wires its [Open] back to the same
 *  "start recording from this event" path the Calendar page uses. Renders
 *  nothing in the main window — it drives a separate pre-rendered webview. */
function MeetingReminder({
  hasCalendar,
  onStart,
}: {
  hasCalendar: boolean;
  onStart: (seed: EventSeed) => void;
}): null {
  const pending = useRef<CalendarEvent | null>(null);
  const notified = useRef<Set<string>>(new Set());

  // [Open] in the reminder popup → start recording the pending meeting (same
  // path as clicking the calendar entry), surfacing the main window first.
  useEffect(() => {
    let un: UnlistenFn | undefined;
    let cancelled = false;
    listen('daisy://reminder/open', () => {
      const e = pending.current;
      if (e) {
        tauri.showMainWindow().catch(() => {});
        onStart(eventToSeed(e));
      }
    }).then((fn) => { if (cancelled) fn(); else un = fn; }).catch(() => {});
    return () => { cancelled = true; un?.(); };
  }, [onStart]);

  // Scheduler: every 30s pop the soonest event entering its lead window. Once
  // per event (tracked by uid); skipped while a recording is in progress.
  useEffect(() => {
    if (!hasCalendar) return;
    let stop = false;
    const tick = async () => {
      try {
        const events = await tauri.listUpcomingEvents(1);
        const lead = settingsStore.get()?.reminder_lead_seconds ?? 60;
        if (lead <= 0) return;
        const snap = getRecordingSnapshot();
        if (snap?.state === 'recording' || snap?.state === 'paused') return;
        const now = Math.floor(Date.now() / 1000);
        const due = events
          .filter((e) => !notified.current.has(e.uid))
          .filter((e) => {
            const dt = e.start_unix_seconds - now;
            return dt <= lead && dt > -30; // inside the lead window, not long past
          })
          .sort((a, b) => a.start_unix_seconds - b.start_unix_seconds)[0];
        if (due) {
          notified.current.add(due.uid);
          pending.current = due;
          await tauri.showReminderWindow(due.title);
        }
      } catch { /* ignore — next tick retries */ }
    };
    void tick();
    const id = window.setInterval(() => { if (!stop) void tick(); }, 30_000);
    return () => { stop = true; window.clearInterval(id); };
  }, [hasCalendar]);

  return null;
}

interface DiarizingItem { sessionId: string; title: string | null }
interface RecentDiarItem { sessionId: string; title: string | null; ok: boolean; at: number }

/** Bridge from the App-level diarize toast-state arrays into the unified
 *  toast store. (Finalize/lifecycle toasts come from <SessionStatusToasts>.) */
function AppToastDriver({
  diarizing, recentlyDiarized, onOpenSession,
}: {
  diarizing: DiarizingItem[];
  recentlyDiarized: RecentDiarItem[];
  onOpenSession: (id: string) => void;
}) {
  useEffect(() => {
    for (const d of diarizing) {
      pushToast({
        id: `diarizing:${d.sessionId}`, severity: 'working',
        title: `Re-diarizing "${d.title?.trim() || 'this meeting'}"…`,
      });
    }
  }, [diarizing]);

  useEffect(() => {
    for (const d of recentlyDiarized) {
      dismissToast(`diarizing:${d.sessionId}`);
      pushToast({
        id: `diarized:${d.sessionId}`,
        severity: d.ok ? 'done' : 'error',
        title: d.ok
          ? `Grouped voices — "${d.title?.trim() || 'this meeting'}"`
          : `Voice grouping failed — "${d.title?.trim() || 'this meeting'}"`,
        onClick: () => onOpenSession(d.sessionId),
        autoDismissMs: 6000,
      });
    }
  }, [recentlyDiarized, onOpenSession]);

  return null;
}
