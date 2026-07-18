import { useEffect, useRef, useState } from 'react';
import { useVisibleInterval } from './lib/useVisibleInterval';
import { useRecordingState } from './lib/recordingState';
import { ToastStack } from './components/NotificationToast';
import { pushToast, dismissToast } from './lib/toastStore';
import { GlobalConfirm } from './components/GlobalConfirm';
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
import { tauri, type EventSeed, type CalendarEvent, type UpdateInfo, type LicenseStatus, type SessionFocus } from './tauri';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { LicenseGate } from './routes/LicenseGate';
import { ProfileBlocked } from './routes/ProfileBlocked';
import { expiryBannerState } from './lib/expiry-banner';
import { trialBannerState, PRICING_URL, type TrialStage } from './lib/trial-banner';
import { beginFinalizeWatch } from './lib/sessionLifecycle';
import { runFinalize } from './lib/finalizeRunner';
import { canNavigate } from './lib/navGuard';
import { initUiZoom } from './lib/uiZoomController';
import type { JobKind } from './lib/sessionPhase';

const IGNORED_UPDATE_KEY = 'daisy:ignoredUpdate';

type Phase = 'loading' | 'eula' | 'consent' | 'welcome' | 'unlock' | 'profile_blocked' | 'expired' | 'main';

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

// Reorderable nav-rail items. Settings is not listed — it stays anchored at
// the bottom. `record` uses the route name "recording". The Workflows item
// keeps the key `tasks`; persisted nav_order values use that key.
export const NAV_DEFS: { key: string; label: string; icon?: string; route?: Route; externalUrl?: string; recordStyle?: boolean }[] = [
  { key: 'record', label: 'Record', route: { name: 'recording' }, recordStyle: true },
  { key: 'library', label: 'Library', icon: '📚', route: { name: 'library' } },
  { key: 'calendar', label: 'Calendar', icon: '📅', route: { name: 'calendar' } },
  { key: 'search', label: 'Search', icon: '🔍', route: { name: 'search' } },
  { key: 'tasks', label: 'Workflows', icon: '🔁', route: { name: 'workflows' } },
  { key: 'analyzer', label: 'Analyzer', icon: '🎯', route: { name: 'analyzer' } },
  { key: 'history', label: 'History', icon: '🕘', route: { name: 'history' } },
  { key: 'help', label: 'Help ↗', icon: '❓', externalUrl: 'https://www.daisylocal.app/help' },
];
const DEFAULT_NAV_ORDER = NAV_DEFS.map((d) => d.key);

export function App() {
  const [phase, setPhase] = useState<Phase>('loading');
  const [route, setRoute] = useState<Route>({ name: 'library' });
  // Drives whether the Calendar nav entry appears above Library. Set on
  // mount + refreshed any time the Settings → Calendars section is
  // touched (via the `refreshCalendarPresence` callback passed down).
  const [hasCalendar, setHasCalendar] = useState(false);

  // Nav-rail ordering (persisted in settings). dragKey tracks the item being
  // dragged for HTML5 drag-and-drop reordering.
  const [navOrder, setNavOrder] = useState<string[]>([]);
  const [dragKey, setDragKey] = useState<string | null>(null);
  // Item currently hovered as a drop target — drives the drop-indicator line.
  const [dragOverKey, setDragOverKey] = useState<string | null>(null);

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

  // Chunk rotation tick. The backend gates on Recording state + chunk age;
  // calls outside an active recording are no-ops. 60s poll with a 300s
  // rotation interval gives ~5-min chunks bounded by the polling interval.
  // Rotation is one atomic OpenChunk dispatch in the audio worker; no audio
  // frames are dropped.
  useVisibleInterval(
    () => { void tauri.maybeRotateChunk(300).catch(() => { /* idle */ }); },
    60_000,
    phase === 'main',
  );

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

  // Dead-mic warning. The backend's autogain tap fires `recording:mic-silent`
  // once per session when the mic capture stream delivers no signal in the
  // first few seconds. The level meter can still move (separate stream).
  // Shows a persistent warning toast.
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    void listen('recording:mic-silent', () => {
      pushToast({
        id: 'mic-silent',
        severity: 'warning',
        title: 'Your mic isn’t being recorded',
        body: 'No microphone signal detected. Another app (e.g. Teams or Zoom) may be using the mic. Stop and restart this recording — or start Daisy before joining the call.',
        dismissible: true,
      });
    }).then((fn) => { unlisten = fn; });
    return () => { unlisten?.(); };
  }, []);

  // Pausing detaches the capture — "your mic isn't being recorded" is
  // meaningless (and alarming) while paused. The backend re-checks after
  // resume and re-fires if the mic is genuinely dead.
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    void listen<{ state: string } | null>('recording:snapshot', (ev) => {
      const st = ev.payload?.state;
      if (st === 'paused' || st === 'stopped' || !st) dismissToast('mic-silent');
    }).then((fn) => { unlisten = fn; });
    return () => { unlisten?.(); };
  }, []);

  // Saved order first (known keys only), then any defaults not yet listed.
  const orderedNavKeys = (() => {
    const known = new Set(DEFAULT_NAV_ORDER);
    const saved = navOrder.map((k) => (k === 'coaching' ? 'analyzer' : k)).filter((k) => known.has(k));
    const rest = DEFAULT_NAV_ORDER.filter((k) => !saved.includes(k));
    return [...saved, ...rest];
  })();

  async function persistNavOrder(order: string[]) {
    setNavOrder(order);
    try {
      const s = await tauri.readSettings();
      await tauri.writeSettings({ ...s, nav_order: order });
    } catch { /* non-fatal — order still applies for this session */ }
  }

  function onNavDrop(targetKey: string) {
    const from = orderedNavKeys.indexOf(dragKey ?? '');
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
    beginFinalizeWatch(sessionId, title);
    void runFinalize(sessionId, 'cascade');
    setRoute({ name: 'library', sessionId });
  }

  /** Manual Summarize / Regen from the Library SummaryPane. The session is
   *  already stopped (not the active recording slot); the stop prelude is skipped.
   *  Arms the watch + runs the chosen job kind; the session-status toast reflects it. */
  function handleSummarizeStarted(sessionId: string, title: string, kind: JobKind = 'cascade', promptId?: string) {
    // Only the full cascade writes a finalize.status.json sidecar. regen-summary
    // and regen-transcript write no sidecar and surface their own toasts
    // (finalizeRunner); arming the poll for them reads the prior finalize's
    // stale sidecar.
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
    // Profile binding: a trial install can't carry forward another install's
    // data (licensed installs self-bind to the license → always ok).
    const binding = await tauri.profileBindingCheck().catch(() => ({ state: 'ok' as const }));
    if (binding.state === 'foreign') {
      setPhase('profile_blocked');
      return;
    }
    setPhase(lic && lic.state === 'expired' ? 'expired' : 'main');
  }

  useEffect(() => {
    resolvePhase().catch(console.error);
  }, []);

  // Apply the persisted UI zoom and install Cmd/Ctrl +/-/0 shortcuts (once).
  useEffect(() => { void initUiZoom(); }, []);

  // Once we're in 'main', also pick up an active recording if any.
  useEffect(() => {
    if (phase !== 'main') return;
    let cancelled = false;
    tauri.currentRecording().then((s) => {
      if (!cancelled && (s === 'recording' || s === 'paused')) {
        setRoute({ name: 'recording' });
      }
    }).catch(() => { /* ignored */ });
    refreshCalendarPresence();
    tauri.readSettings().then((s) => setNavOrder(s.nav_order ?? [])).catch(() => { /* defaults */ });
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
      // License heartbeat — throttled to ~once/day server-side; runs
      // independently of the update-check preference.
      tauri.licenseCheckin().then((s) => { if (!stop) setLicense(s); }).catch(() => {});
      try {
        const s = await tauri.readSettings();
        if (!s.auto_update_check) return;
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

  if (phase === 'loading') return <p className="meta" style={{ padding: 32 }}>Loading...</p>;
  if (phase === 'eula') return <EulaGate onAccepted={() => resolvePhase()} />;
  if (phase === 'consent') return <Consent onAccepted={() => resolvePhase()} />;
  if (phase === 'welcome') return <Wizard mode="first-run" onComplete={() => resolvePhase()} />;
  if (phase === 'unlock') return <Unlock onUnlocked={() => resolvePhase()} />;
  if (phase === 'profile_blocked') return <ProfileBlocked onResolved={() => resolvePhase()} />;
  if (phase === 'expired') return <LicenseGate onActivated={() => resolvePhase()} />;

  return (
    <div className="app">
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
          if (key === 'calendar' && !hasCalendar) return null;
          const active = def.route != null && route.name === def.route.name;
          const cls = def.recordStyle
            ? `nav-item nav-item--record ${active ? 'active' : ''}`
            : `nav-item ${active ? 'active' : ''}`;
          return (
            <button
              key={key}
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
              {def.recordStyle && <span className="nav-item--record__dot" aria-hidden="true" />}
              {def.icon && <span className="nav-item__icon" aria-hidden="true">{def.icon}</span>}
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
        <button className="nav-item" title="Minimize to floating mini-window"
                onClick={() => tauri.showMiniWindow().catch(() => {})}>
          <span className="nav-item__icon" aria-hidden="true">🪟</span><span>Mini</span>
        </button>
        <button className={`nav-item ${route.name === 'settings' ? 'active' : ''}`}
                onClick={() => { void canNavigate().then((ok) => { if (ok) setRoute({ name: 'settings' }); }); }}>
          <span className="nav-item__icon" aria-hidden="true">⚙️</span><span>Settings</span>
        </button>
      </aside>
      <main className="main">
        {(() => {
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
                <button className="btn btn--primary"
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
        {update && (
          <TopBanner
            kind="info"
            onDismiss={() => { localStorage.setItem(IGNORED_UPDATE_KEY, update.latest); setUpdate(null); }}
          >
            <strong>Daisy {update.latest} is available.</strong>{' '}
            <span className="meta">
              You're currently on {update.current}. Release notes and download link available{' '}
              <a
                href="https://www.daisylocal.app/changelog"
                onClick={(e) => { e.preventDefault(); void tauri.openExternal('https://www.daisylocal.app/changelog'); }}
              >
                here
              </a>.
            </span>
          </TopBanner>
        )}
        <div className={'main__body' + (route.name === 'library' || route.name === 'recording' || route.name === 'calendar' ? ' main__body--bleed' : '')}>
        {route.name === 'library' && (
          <Library
            selectedSessionId={route.sessionId}
            focus={route.focus}
            onSelect={(id) => setRoute({ name: 'library', sessionId: id })}
            onOpenSearch={(query) => setRoute({ name: 'search', query })}
            onStartSummarize={handleSummarizeStarted}
            processingSessionIds={[]}
            onNavigateToProviders={() => setRoute({ name: 'settings', section: 'Providers' })}
          />
        )}
        {route.name === 'recording' && (
          <ActiveSession
            onProcessingStarted={handleProcessingStarted}
            eventSeed={route.eventSeed}
            onDiscarded={() => setRoute({ name: 'library' })}
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
            onOpenSession={(id, focus) => setRoute({ name: 'library', sessionId: id, focus })}
            onNavigateToProviders={() => setRoute({ name: 'settings', section: 'Providers' })}
          />
        )}
        {route.name === 'history' && (
          <History onOpenSession={(id) => setRoute({ name: 'library', sessionId: id })} />
        )}
        {route.name === 'workflows' && <Workflows />}
        {route.name === 'analyzer' && (
          <Analyzer
            onOpenSession={(id) => setRoute({ name: 'library', sessionId: id })}
            onNavigateToProviders={() => setRoute({ name: 'settings', section: 'Providers' })}
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
        onOpenSession={(sessionId) => {
          setRoute({ name: 'library', sessionId });
          // Force the summary tab even if we're already on this session's detail
          // page (e.g. looking at Participants) — the SummaryPane listens.
          window.dispatchEvent(new CustomEvent('daisy:show-summary', { detail: { sessionId } }));
        }}
        onResume={(sessionId, title) => {
          // Interrupted (crash mid-finalize) → re-run missing stages via the
          // integrity audit, and re-arm the finalize progress poll.
          beginFinalizeWatch(sessionId, title);
          void tauri.repairSession(sessionId);
        }}
        onCopyPrompt={(sessionId) => {
          // Copy-with-prompt needs the loaded transcript (lives in SummaryPane);
          // routes to the meeting where the Copy control is.
          setRoute({ name: 'library', sessionId });
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
          setRoute({ name: 'library', sessionId: id });
          window.dispatchEvent(new CustomEvent('daisy:show-summary', { detail: { sessionId: id } }));
        }}
      />
      <QaToast onOpenSearch={() => setRoute({ name: 'search' })} />
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
        const [events, settings, snap] = await Promise.all([
          tauri.listUpcomingEvents(1),
          tauri.readSettings(),
          tauri.recordingSnapshot().catch(() => null),
        ]);
        const lead = settings.reminder_lead_seconds ?? 60;
        if (lead <= 0) return;
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
