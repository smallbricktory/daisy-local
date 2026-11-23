// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { useEffect, useRef, useState, useSyncExternalStore } from 'react';
import './../mini-window.css';
import { tauri, formatDurationSeconds } from '../tauri';
import { useRecordingState } from '../lib/recordingState';
import { useRecordedClock } from '../lib/useRecordedClock';
import { useMachineSettings } from '../lib/settingsStore';
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
 * app-wide `transcript:segment` event) and the shared recordingState store
 * (`recording:snapshot` event + cold fetch + visibility-resume refresh).
 * The transcript backlog from before the mini opened is not carried over —
 * only segments that arrive while the mini is up are shown.
 */
export function MiniWindow() {
  const live = useSyncExternalStore(subscribeLiveTranscript, getLiveTranscriptState);
  const { finals, interimSystem } = live;
  const snap = useRecordingState();
  const elapsed = useRecordedClock(snap);
  const endRef = useRef<HTMLDivElement>(null);

  // Auto-scroll transcript to newest line.
  useEffect(() => { endRef.current?.scrollIntoView({ block: 'end' }); }, [finals, interimSystem]);

  const state = snap?.state ?? 'idle';
  const dotClass = state === 'recording' ? 'mini__dot--rec' : state === 'paused' ? 'mini__dot--paused' : '';

  const onPauseToggle = async () => {
    try {
      if (state === 'paused') await tauri.resumeRecording();
      else await tauri.pauseRecording();
      // The backend emits `recording:snapshot`; the shared store picks it up.
    } catch { /* ignore */ }
  };

  const onStop = async () => {
    // Confirm before stopping an active recording.
    if (state === 'recording' || state === 'paused') {
      const ok = await confirm({
        title: 'Stop recording?',
        body: 'Stops capture and starts transcribing and summarizing this meeting.',
        confirmLabel: 'Stop', danger: true,
      });
      if (!ok) return;
    }
    try { await tauri.finishRecording(); } catch { await tauri.showMainWindow().catch(() => {}); }
  };

  const onRestore = () => { tauri.showMainWindow().catch(() => {}); };

  // Starts a recording straight from the mini, using the default mic.
  // Mirrors the record screen's guards (provider configured + vault
  // unlocked); a misconfigured start surfaces a message here.
  const [starting, setStarting] = useState(false);
  const [startErr, setStartErr] = useState<string | null>(null);
  const machine = useMachineSettings();
  const onRecord = async () => {
    setStartErr(null);
    setStarting(true);
    try {
      const sources = await tauri.listAudioSources();
      const mics = sources.filter((s) => s.kind === 'mic');
      const def = machine?.default_mic_source_id ?? null;
      const micId = (def != null && mics.some((m) => m.id === def)) ? def : (mics[0]?.id ?? null);
      if (micId == null || micId <= 0) { setStartErr('No microphone detected.'); return; }
      resetLiveTranscript(null);
      // The start emits `recording:snapshot`, which flips the mini into the
      // live recording view via the shared store.
      await tauri.startRecording({
        mic_source_id: micId,
        system_source_id: null,
        session_id: null,
        title: null,
      });
    } catch (e) {
      setStartErr(e instanceof Error ? e.message : String(e));
    } finally {
      setStarting(false);
    }
  };

  // Last few system-track turns.
  const tail = finals.filter((e): e is LiveTurn => !isPauseMarker(e) && e.track !== 'mic').slice(-6);
  const interims = [interimSystem].filter((t): t is LiveTurn => t != null);
  const empty = tail.length === 0 && interims.length === 0;
  // Only a live/paused recording shows the transcript and Pause/Stop controls.
  // Otherwise the mini is a small "back to Daisy" shell.
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
        <button data-testid="mini-expand" className="mini__expand" title="Restore main window" aria-label="Restore main window" onClick={onRestore}>⤢</button>
      </div>
      <div className="mini__transcript" role="log" aria-live="polite">
        {!isActive && (
          <p className="mini__placeholder" style={startErr ? { color: 'var(--danger)' } : undefined}>
            {startErr ?? 'No active recording'}
          </p>
        )}
        {isActive && empty && <p className="mini__placeholder">Waiting for others to speak…</p>}
        {isActive && tail.map((t) => <p key={t.start_ms} className="mini__bubble">{t.text}</p>)}
        {isActive && interims.map((t) => <p key={`int-${t.track}-${t.start_ms}`} className="mini__bubble mini__interim">{t.text}</p>)}
        <div ref={endRef} />
      </div>
      <div className="mini__controls">
        {isActive ? (
          <>
            <button data-testid="mini-pause-resume" className="mini__btn" onClick={onPauseToggle}>
              {state === 'paused' ? 'Resume' : 'Pause'}
            </button>
            <button data-testid="mini-stop" className="mini__btn mini__btn--stop" onClick={onStop}>Stop</button>
          </>
        ) : (
          <button data-testid="mini-record" className="mini__btn mini__btn--rec" onClick={() => void onRecord()} disabled={starting}>
            {starting ? 'Starting…' : '● Record'}
          </button>
        )}
      </div>
    </div>
  );
}
