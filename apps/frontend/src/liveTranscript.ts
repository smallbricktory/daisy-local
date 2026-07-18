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

export type TurnEntry = LiveTurn | PauseMarker;

export function isPauseMarker(e: TurnEntry): e is PauseMarker {
  return (e as PauseMarker).marker === 'pause';
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
  if (last && !isPauseMarker(last) && last.track === turn.track) {
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

// ---- Live cross-talk (bleed) suppression -----------------------------------
// On a built-in mic + speakers (no headphones) the mic picks up the remote
// voices coming out of the speakers; the same words show up on both the mic
// and system tracks. Live ASR bypasses AEC (AEC runs at finalize). A mic
// final whose word-bigrams are largely contained in a recent system final is
// speaker bleed and is dropped from the display only; the system/remote
// track is never dropped. The granular live_transcript.jsonl is filtered
// again at finalize (promotion) — see the Rust twin below.
// The two tracks decode independently; an echo's start_ms can drift several
// seconds from the original. Matching is by content (overlap + min words),
// not a tight time window.
//
// KEEP IN SYNC with the Rust twin: crates/transcript/src/promote.rs
// (filter_mic_bleed) — same overlap/window/min-words. Live display filters
// online in the browser; promote filters offline at finalize. Change a
// threshold in both places. Finalize additionally arbitrates echo DIRECTION
// from the track WAVs (echo_direction.rs) and may drop the system copy
// instead; the browser has no audio, so this twin stays one-directional and
// the final transcript corrects any live misattribution.
const BLEED_WINDOW_MS = 8000;
const BLEED_OVERLAP = 0.6; // bigram containment (|∩| / mic side's bigrams)
const BLEED_MIN_WORDS = 3; // finals shorter than this are never treated as bleed
const BLEED_BUFFER = 24;

interface RecentFinal {
  words: string[];
  start_ms: number;
}

function normWords(text: string): string[] {
  return text.toLowerCase().replace(/[^a-z0-9\s]/g, ' ').split(/\s+/).filter(Boolean);
}

function bigrams(words: string[]): Set<string> {
  const s = new Set<string>();
  for (let i = 0; i + 1 < words.length; i++) s.add(`${words[i]} ${words[i + 1]}`);
  return s;
}

// Fraction of `frag`'s bigrams present in `hay`. Directional on purpose:
// dividing by the smaller set let a 3-word system stub delete a full mic
// sentence because the stub's couple of bigrams matched.
function bigramContainment(frag: Set<string>, hay: Set<string>): number {
  if (frag.size === 0 || hay.size === 0) return 0;
  let inter = 0;
  for (const x of frag) if (hay.has(x)) inter++;
  return inter / frag.size;
}

const BLEED_SUBSTRING_MAX_WORDS = 6;
// Echo trails its source; a mic segment may lead its system twin only by
// decode jitter. KEEP IN SYNC with promote.rs (in_echo_window).
const BLEED_LEAD_MS = 2000;

function inEchoWindow(micStart: number, sysStart: number): boolean {
  return micStart >= sysStart
    ? micStart - sysStart <= BLEED_WINDOW_MS
    : sysStart - micStart <= BLEED_LEAD_MS;
}

// 4-char-prefix equality — tolerates plural/tense mangling in echo ASR.
function wordEq(a: string, b: string): boolean {
  const pa = a.slice(0, 4);
  return pa.length > 0 && pa === b.slice(0, 4);
}

/** Ordered, gap-tolerant containment; up to `misses` fragment words absent. */
function subseqContained(frag: string[], hay: string[], misses: number): boolean {
  let hi = 0;
  let missed = 0;
  for (const w of frag) {
    let found = false;
    while (hi < hay.length) {
      const ok = wordEq(w, hay[hi]);
      hi++;
      if (ok) { found = true; break; }
    }
    if (!found && ++missed > misses) return false;
  }
  return true;
}

// Order-free containment for 5+-word turns: reworded echoes defeat ordered
// matching; ≥75% multiset word containment in one nearby system turn = echo.
// KEEP IN SYNC with promote.rs (is_contained_echo).
const BLEED_CONTAIN_MIN_WORDS = 5;
const BLEED_CONTAIN_RATIO = 0.75;
// Pooled rule floor — short fragments against a large pool would
// false-positive on function words alone. KEEP IN SYNC with promote.rs.
const BLEED_POOL_MIN_WORDS = 4;

function containedEcho(frag: string[], recentSystem: RecentFinal[], micStart: number): boolean {
  if (frag.length < BLEED_CONTAIN_MIN_WORDS) return false;
  for (const r of recentSystem) {
    if (!inEchoWindow(micStart, r.start_ms)) continue;
    const hay = [...r.words];
    let matched = 0;
    for (const w of frag) {
      const i = hay.findIndex((x) => wordEq(w, x));
      if (i >= 0) { hay.splice(i, 1); matched++; }
    }
    if (matched / frag.length >= BLEED_CONTAIN_RATIO) return true;
  }
  return false;
}

/** True when `turn` (a mic final) is speaker bleed of a recent system final —
 *  high bigram overlap within the echo window, or a short fragment whose
 *  words appear (in order, prefix-matched) inside one. Pure; exported for
 *  tests. KEEP IN SYNC with promote.rs (is_substring_echo / is_mic_bleed). */
export function isMicBleedOfSystem(turn: LiveTurn, recentSystem: RecentFinal[]): boolean {
  const w = normWords(turn.text);
  if (w.length > 0 && w.length <= BLEED_SUBSTRING_MAX_WORDS) {
    for (const r of recentSystem) {
      if (!inEchoWindow(turn.start_ms, r.start_ms)) continue;
      if (w.length <= 2) {
        const needle = ` ${w.join(' ')} `;
        if (` ${r.words.join(' ')} `.includes(needle)) return true;
      } else if (subseqContained(w, r.words, w.length >= 5 ? 1 : 0)) {
        return true;
      }
    }
  }
  if (containedEcho(w, recentSystem, turn.start_ms)) return true;
  // Pooled window: the two decoders fragment the same speech at different
  // boundaries, so no single system final contains this turn — the ordered
  // concatenation of the window's system text does. KEEP IN SYNC with
  // promote.rs (pooled_echo_match).
  if (w.length >= BLEED_POOL_MIN_WORDS) {
    const members = recentSystem
      .filter((r) => inEchoWindow(turn.start_ms, r.start_ms))
      .sort((a, b) => a.start_ms - b.start_ms);
    if (members.length >= 2) {
      const pool = members.flatMap((r) => r.words);
      if (subseqContained(w, pool, Math.floor(w.length / 6))) return true;
    }
  }
  if (w.length < BLEED_MIN_WORDS) return false;
  const tb = bigrams(w);
  for (const r of recentSystem) {
    if (!inEchoWindow(turn.start_ms, r.start_ms)) continue;
    if (bigramContainment(tb, bigrams(r.words)) >= BLEED_OVERLAP) return true;
  }
  return false;
}

type TranscriptKind =
  | { type: 'interim'; start_ms: number; end_ms: number; text: string; confidence: number | null }
  | { type: 'final'; start_ms: number; end_ms: number; text: string; confidence: number | null }
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
// Trailing buffer of recent system finals, for mic-bleed matching. Not part of
// rendered state — cleared on session reset.
let recentSystemFinals: RecentFinal[] = [];
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
    if (p.track === 'system') {
      // Buffers recent system finals for mic-echo matching.
      recentSystemFinals.push({ words: normWords(turn.text), start_ms: turn.start_ms });
      if (recentSystemFinals.length > BLEED_BUFFER) recentSystemFinals.shift();
      patch({ finals: mergeFinalInto(state.finals, turn), interimSystem: null });
    } else if (isMicBleedOfSystem(turn, recentSystemFinals)) {
      // Speaker bleed of a remote turn — drop from the display only.
      patch({ interimMic: null });
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
  recentSystemFinals = [];
  state = { ...EMPTY, sessionId };
  emit();
}

/** Append a pause marker into the running transcript. */
export function appendPauseMarker(at: string): void {
  patch({ finals: [...state.finals, { marker: 'pause', at }] });
}
