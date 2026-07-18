import { useEffect, useRef, useState, useSyncExternalStore } from 'react';
import './../mini-window.css';
import { tauri, formatDurationSeconds, type RecordingSnapshot } from '../tauri';
import { useVisibleInterval } from '../lib/useVisibleInterval';
import { confirm } from '../lib/confirm';
import {
  subscribeLiveTranscript,
  getLiveTranscriptState,
  isPauseMarker,
  resetLiveTranscript,
  type LiveTurn,
} from '../liveTranscript';

/**
 * Floating mini-window (Card layout). Rendered in place of <App/> when the
 * webview label is "mini". Reuses the global liveTranscript store (fed by the
 * app-wide `transcript:segment` event) and the recording_snapshot command.
 * The transcript backlog from before the mini opened is not carried over —
 * only segments that arrive while the mini is up are shown.
 */
export function MiniWindow() {
  const live = useSyncExternalStore(subscribeLiveTranscript, getLiveTranscriptState);
  const { finals, interimMic, interimSystem } = live;
  const [snap, setSnap] = useState<RecordingSnapshot | null>(null);
  const [elapsed, setElapsed] = useState(0);
  const endRef = useRef<HTMLDivElement>(null);

  // Poll the snapshot on mount + every 2s as a visibility-paused fallback.
  // The push event below is the primary signal; this just covers misses.
  useEffect(() => {
    let alive = true;
    tauri.recordingSnapshot().then((s) => { if (alive) setSnap(s); }).catch(() => {});
    return () => { alive = false; };
  }, []);
  useVisibleInterval(() => {
    tauri.recordingSnapshot().then(setSnap).catch(() => {});
  }, 2000);

  // Push-based refresh: reacts instantly to the app-wide `recording:state`
  // event; the 2s poll above is the fallback.
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let cancelled = false;
    import('@tauri-apps/api/event')
      .then(({ listen }) =>
        listen('recording:state', () => {
          tauri.recordingSnapshot().then(setSnap).catch(() => {});
        }),
      )
      .then((fn) => {
        if (cancelled) fn();
        else unlisten = fn;
      })
      .catch(() => {});
    return () => { cancelled = true; unlisten?.(); };
  }, []);

  // Elapsed timer derived from snapshot start time. Sync once on snapshot
  // change, then visibility-paused 1s ticker.
  useEffect(() => {
    if (snap?.state === 'recording') {
      setElapsed(Math.max(0, Math.floor(Date.now() / 1000) - snap.started_at_unix_seconds));
    }
  }, [snap?.state, snap?.started_at_unix_seconds]);
  useVisibleInterval(() => {
    if (snap?.state !== 'recording') return;
    setElapsed(Math.max(0, Math.floor(Date.now() / 1000) - snap.started_at_unix_seconds));
  }, 1000, snap?.state === 'recording');

  // Auto-scroll transcript to newest line.
  useEffect(() => { endRef.current?.scrollIntoView({ block: 'end' }); }, [finals, interimMic, interimSystem]);

  const state = snap?.state ?? 'idle';
  const dotClass = state === 'recording' ? 'mini__dot--rec' : state === 'paused' ? 'mini__dot--paused' : '';

  const onPauseToggle = async () => {
    try {
      if (state === 'paused') await tauri.resumeRecording();
      else await tauri.pauseRecording();
      const s = await tauri.recordingSnapshot();
      setSnap(s);
    } catch { /* ignore — snapshot poll will reconcile */ }
  };

  const onStop = async () => {
    // Guard: stopping ends the meeting. Confirm before destroying capture.
    if (state === 'recording' || state === 'paused') {
      const ok = await confirm({
        title: 'Stop recording?',
        body: 'Stops capture and starts finalizing this meeting.',
        confirmLabel: 'Stop', danger: true,
      });
      if (!ok) return;
    }
    try { await tauri.stopRecording(); } catch { /* ignore */ }
    await tauri.showMainWindow().catch(() => {});
  };

  const onRestore = () => { tauri.showMainWindow().catch(() => {}); };

  // Starts a recording straight from the mini, using the default mic.
  // Mirrors the record screen's guards (provider configured + vault
  // unlocked); a misconfigured start surfaces a message here.
  const [starting, setStarting] = useState(false);
  const [startErr, setStartErr] = useState<string | null>(null);
  const onRecord = async () => {
    setStartErr(null);
    setStarting(true);
    try {
      const [sources, settings] = await Promise.all([tauri.listAudioSources(), tauri.readSettings()]);
      const mics = sources.filter((s) => s.kind === 'mic');
      const def = settings.default_mic_source_id;
      const micId = (def != null && mics.some((m) => m.id === def)) ? def : (mics[0]?.id ?? null);
      if (micId == null || micId <= 0) { setStartErr('No microphone detected.'); return; }
      resetLiveTranscript(null);
      const s = await tauri.startRecording({
        mic_source_id: micId,
        system_source_id: null,
        session_id: null,
        title: null,
      });
      setSnap(s); // flips the mini into the live recording view
    } catch (e) {
      setStartErr(e instanceof Error ? e.message : String(e));
    } finally {
      setStarting(false);
    }
  };

  // Last few final turns (Card shows a tail, not the whole transcript).
  const tail = finals.filter((e): e is LiveTurn => !isPauseMarker(e)).slice(-6);
  const interims = [interimMic, interimSystem].filter((t): t is LiveTurn => t != null);
  const empty = tail.length === 0 && interims.length === 0;
  // Only a live/paused recording gets the transcript + Pause/Stop controls.
  // When idle or finished, the mini is just a small "back to Daisy" shell —
  // showing recording buttons + "Waiting for transcript…" there is wrong.
  const isActive = state === 'recording' || state === 'paused';

  return (
    <div className="mini">
      <div className="mini__bar" data-tauri-drag-region>
        <span className={`mini__dot ${dotClass}`} role="status" aria-label={`Recording ${state}`} />
        <span className="mini__time">{state === 'recording' || state === 'paused' ? formatDurationSeconds(elapsed) : '--:--'}</span>
        <span className="mini__mode" aria-hidden="true" title="microphone">
          <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor"
               strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
            <rect x="9" y="2" width="6" height="12" rx="3" />
            <path d="M5 11a7 7 0 0 0 14 0" />
            <line x1="12" y1="18" x2="12" y2="22" />
            <line x1="9" y1="22" x2="15" y2="22" />
          </svg>
        </span>
        <button className="mini__expand" title="Restore main window" aria-label="Restore main window" onClick={onRestore}>⤢</button>
      </div>
      <div className="mini__transcript" role="log" aria-live="polite">
        {!isActive && (
          <p className="mini__placeholder" style={startErr ? { color: 'var(--danger)' } : undefined}>
            {startErr ?? 'No active recording'}
          </p>
        )}
        {isActive && empty && <p className="mini__placeholder">Waiting for transcript…</p>}
        {isActive && tail.map((t) => <p key={t.start_ms}>{t.text}</p>)}
        {isActive && interims.map((t) => <p key={`int-${t.track}-${t.start_ms}`} className="mini__interim">{t.text}</p>)}
        <div ref={endRef} />
      </div>
      <div className="mini__controls">
        {isActive ? (
          <>
            <button className="mini__btn" onClick={onPauseToggle}>
              {state === 'paused' ? 'Resume' : 'Pause'}
            </button>
            <button className="mini__btn mini__btn--stop" onClick={onStop}>Stop</button>
          </>
        ) : (
          <button className="mini__btn mini__btn--rec" onClick={() => void onRecord()} disabled={starting}>
            {starting ? 'Starting…' : '● Record'}
          </button>
        )}
      </div>
    </div>
  );
}
