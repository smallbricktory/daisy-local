// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Module-level store for the live transcript stream.
//!
//! Backend emits `transcript:segment` while a recording is active. The store
//! owns the segments + interim turns at module scope, attaches the event
//! listener once on first subscribe, and lets components read via
//! `useSyncExternalStore`. State survives navigation away from the recording
//! screen.

import { listen, type UnlistenFn } from '@tauri-apps/api/event';

export interface LiveTurn {
  track: 'mic' | 'system';
  text: string;
  start_ms: number;
  end_ms: number;
  isInterim: boolean;
}

export interface PauseMarker {
  marker: 'pause';
  at: string;
}

/** Faint-audio annotation: the engine heard energy but decoded no words. */
export interface MissedSpeechMarker {
  marker: 'missed-speech';
  track: 'mic' | 'system';
  start_ms: number;
  end_ms: number;
}

export type TurnEntry = LiveTurn | PauseMarker | MissedSpeechMarker;

export function isPauseMarker(e: TurnEntry): e is PauseMarker {
  return (e as PauseMarker).marker === 'pause';
}

export function isMissedSpeechMarker(e: TurnEntry): e is MissedSpeechMarker {
  return (e as MissedSpeechMarker).marker === 'missed-speech';
}

/** Any non-turn entry (pause or missed-speech marker). */
export function isMarker(e: TurnEntry): e is PauseMarker | MissedSpeechMarker {
  return 'marker' in e;
}

/**
 * Appends a final, merging it into the previous line when it's the same
 * speaker (track) with nothing in between. Breaks on a track change or a
 * pause marker. The merged line keeps the first segment's `start_ms` (stable
 * React key as it grows) and only ever replaces the last entry; earlier
 * entries keep their references. Backend `live_transcript.jsonl` stays
 * granular (promotion is unaffected); this shapes the in-app display only.
 */
export function mergeFinalInto(finals: TurnEntry[], turn: LiveTurn): TurnEntry[] {
  const last = finals[finals.length - 1];
  if (last && !isMarker(last) && last.track === turn.track) {
    const a = last.text.trim();
    const b = turn.text.trim();
    const merged: LiveTurn = {
      ...last,
      text: a && b ? `${a} ${b}` : a || b,
      end_ms: Math.max(last.end_ms, turn.end_ms),
    };
    return [...finals.slice(0, -1), merged];
  }
  return [...finals, turn];
}

type TranscriptKind =
  | { type: 'interim'; start_ms: number; end_ms: number; text: string; confidence: number | null }
  | { type: 'final'; start_ms: number; end_ms: number; text: string; confidence: number | null }
  | { type: 'possible_missed_speech'; start_ms: number; end_ms: number }
  | { type: 'error'; message: string };

interface TranscriptSegmentPayload {
  track: 'mic' | 'system';
  kind: TranscriptKind;
}

export interface LiveTranscriptState {
  /** Session this state belongs to. Null when no recording is active. */
  sessionId: string | null;
  finals: TurnEntry[];
  interimMic: LiveTurn | null;
  interimSystem: LiveTurn | null;
  liveError: string | null;
}

const EMPTY: LiveTranscriptState = {
  sessionId: null,
  finals: [],
  interimMic: null,
  interimSystem: null,
  liveError: null,
};

let state: LiveTranscriptState = EMPTY;
const subscribers = new Set<() => void>();
let listenerPromise: Promise<UnlistenFn> | null = null;
let errorTimer: ReturnType<typeof setTimeout> | null = null;

function emit(): void {
  for (const cb of subscribers) cb();
}

function patch(next: Partial<LiveTranscriptState>): void {
  state = { ...state, ...next };
  emit();
}

function handlePayload(p: TranscriptSegmentPayload): void {
  if (p.kind.type === 'possible_missed_speech') {
    // Coalesce back-to-back markers from consecutive empty hops on the same
    // track into one growing span.
    const last = state.finals[state.finals.length - 1];
    if (
      last &&
      isMissedSpeechMarker(last) &&
      last.track === p.track &&
      p.kind.start_ms <= last.end_ms + 2000
    ) {
      const grown: MissedSpeechMarker = { ...last, end_ms: Math.max(last.end_ms, p.kind.end_ms) };
      patch({ finals: [...state.finals.slice(0, -1), grown] });
    } else {
      patch({
        finals: [
          ...state.finals,
          { marker: 'missed-speech', track: p.track, start_ms: p.kind.start_ms, end_ms: p.kind.end_ms },
        ],
      });
    }
    return;
  }
  if (p.kind.type === 'error') {
    if (errorTimer) clearTimeout(errorTimer);
    patch({ liveError: p.kind.message });
    errorTimer = setTimeout(() => {
      errorTimer = null;
      patch({ liveError: null });
    }, 5000);
    return;
  }
  const turn: LiveTurn = {
    track: p.track,
    text: p.kind.text,
    start_ms: p.kind.start_ms,
    end_ms: p.kind.end_ms,
    isInterim: p.kind.type === 'interim',
  };
  if (p.kind.type === 'final') {
    // Mic-bleed (echo) suppression happens in the backend live pipeline
    // before emit — every final arriving here is display-worthy.
    if (p.track === 'system') {
      patch({ finals: mergeFinalInto(state.finals, turn), interimSystem: null });
    } else {
      patch({ finals: mergeFinalInto(state.finals, turn), interimMic: null });
    }
  } else if (p.track === 'mic') {
    patch({ interimMic: turn });
  } else {
    patch({ interimSystem: turn });
  }
}

function attachListenerOnce(): void {
  if (listenerPromise) return;
  listenerPromise = listen<TranscriptSegmentPayload>('transcript:segment', (ev) => {
    handlePayload(ev.payload);
  }).catch((e) => {
    listenerPromise = null;
    throw e;
  });
}

export function subscribeLiveTranscript(cb: () => void): () => void {
  attachListenerOnce();
  subscribers.add(cb);
  return () => {
    subscribers.delete(cb);
  };
}

export function getLiveTranscriptState(): LiveTranscriptState {
  return state;
}

/** Reset for a brand-new recording session (clears all transcript history). */
export function resetLiveTranscript(sessionId: string | null): void {
  if (errorTimer) {
    clearTimeout(errorTimer);
    errorTimer = null;
  }
  state = { ...EMPTY, sessionId };
  emit();
}

/** Append a pause marker into the running transcript. */
export function appendPauseMarker(at: string): void {
  patch({ finals: [...state.finals, { marker: 'pause', at }] });
}
