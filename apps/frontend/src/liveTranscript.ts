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
import type { LiveSpeakerTrack, LiveSpeakersPayload, UndiarizedNames } from './tauri';
import { closed, groupTurns, join, readsOn } from './turnRule';

/** Each track's speakers as the `live-speakers` events built them. */
export interface LiveSpeakersState {
  system: LiveSpeakerTrack[];
  mic: LiveSpeakerTrack[];
}

export const NO_SPEAKERS: LiveSpeakersState = { system: [], mic: [] };

/** One final as it arrived, inside a merged line. */
export interface LivePart {
  start_ms: number;
  end_ms: number;
  text: string;
}

export interface LiveTurn {
  track: 'mic' | 'system';
  text: string;
  start_ms: number;
  end_ms: number;
  isInterim: boolean;
  /** The finals a merged line holds, in order; absent on a single final. */
  parts?: LivePart[];
}

export interface PauseMarker {
  marker: 'pause';
  at: string;
  /** Replaces the "paused at" line, e.g. the system-sleep notice. */
  note?: string;
}

export type TurnEntry = LiveTurn | PauseMarker;

export function isPauseMarker(e: TurnEntry): e is PauseMarker {
  return (e as PauseMarker).marker === 'pause';
}

/** Any non-turn entry. */
export function isMarker(e: TurnEntry): e is PauseMarker {
  return 'marker' in e;
}

/**
 * The speaker slot whose turns overlap [start_ms, end_ms] the most, or null
 * when the span overlaps no turn. Turns are in session seconds; the first
 * slot wins a tie.
 */
export function slotForSpan(
  tracks: readonly LiveSpeakerTrack[],
  start_ms: number,
  end_ms: number,
): number | null {
  const a = start_ms / 1000;
  const b = end_ms / 1000;
  let best: number | null = null;
  let bestOverlap = 0;
  for (const t of tracks) {
    let overlap = 0;
    for (const turn of t.turns) {
      overlap += Math.max(0, Math.min(b, turn.end_s) - Math.max(a, turn.start_s));
    }
    if (overlap > bestOverlap) {
      bestOverlap = overlap;
      best = t.slot;
    }
  }
  return best;
}

/**
 * The caption label for a line: the mapped slot's display name or
 * "Speaker <ordinal>"; "Them" on the system track and "Me" on a mic that
 * carries one voice when no slot maps; null otherwise.
 */
export function speakerLabel(
  track: 'mic' | 'system',
  span: { start_ms: number; end_ms: number },
  speakers: LiveSpeakersState,
  singleLocalSpeaker: boolean,
): string | null {
  if (track === 'mic' && singleLocalSpeaker) return 'Me';
  const tracks = speakers[track];
  const slot = slotForSpan(tracks, span.start_ms, span.end_ms);
  const found = slot === null ? undefined : tracks.find((t) => t.slot === slot);
  if (found?.name) {
    return 'display_name' in found.name ? found.name.display_name : `Speaker ${found.name.ordinal}`;
  }
  return track === 'system' ? 'Them' : null;
}

/** A transcript turn labelled from a stopped session's overlay. */
export interface NamedTurn {
  ts: string;
  who: string;
  side: 'me' | 'them';
  text: string;
}

const clock = (ms: number): string => {
  const s = Math.floor(ms / 1000);
  const p = (n: number) => String(n).padStart(2, '0');
  return `${p(Math.floor(s / 3600))}:${p(Math.floor((s % 3600) / 60))}:${p(s % 60)}`;
};

/**
 * The transcript lines of an undiarized session named from its overlay:
 * each line takes the slot with the most overlap; a system line no turn
 * covers reads "Them", a mic line "Me". Lines group into turns as
 * `transcript.md` groups (`groupTurns`, per track), each turn keeping its
 * first line's time.
 */
export function namedTurns(names: UndiarizedNames): NamedTurn[] {
  const speakers: LiveSpeakersState = { system: names.system, mic: names.mic };
  const lines = [...names.lines]
    .sort((a, b) => a.start_ms - b.start_ms)
    .map((l) => ({ ...l, voice: voiceOf(l.track, l, speakers, names.single_local_speaker) }));
  return groupTurns(lines, true).map((t) => {
    const who = labelOf(t, speakers, names.single_local_speaker);
    return { ts: clock(t.start_ms), who, side: who === 'Me' ? 'me' : 'them', text: t.text };
  });
}

/** The slot a line is labelled as: none on a ticked mic, else most overlap. */
function voiceOf(
  track: 'mic' | 'system',
  span: { start_ms: number; end_ms: number },
  speakers: LiveSpeakersState,
  singleLocalSpeaker: boolean,
): number | null {
  if (track === 'mic' && singleLocalSpeaker) return null;
  return slotForSpan(speakers[track], span.start_ms, span.end_ms);
}

/** A turn's label: its voice's name, else "Them" on system and "Me" on mic. */
function labelOf(
  t: { track: 'mic' | 'system'; voice: number | null; lines: readonly { start_ms: number; end_ms: number }[] },
  speakers: LiveSpeakersState,
  singleLocalSpeaker: boolean,
): string {
  if (t.voice === null) return speakerLabel(t.track, t.lines[0], NO_SPEAKERS, singleLocalSpeaker) ?? 'Me';
  const found = speakers[t.track].find((s) => s.slot === t.voice);
  if (found?.name) return 'display_name' in found.name ? found.name.display_name : `Speaker ${found.name.ordinal}`;
  return t.track === 'system' ? 'Them' : 'Me';
}

/** A live line with the label printed above it. */
export interface LabelledLine {
  label: string | null;
  side: 'me' | 'them';
  text: string;
}

/**
 * Live-transcript lines labelled from a stopped session's overlay (null:
 * no overlay): the slot with the most overlap names a line; a system line no
 * turn covers reads "Them", a mic line "Me". Lines paragraph as the live
 * captions do (`groupTurns` onto the line before, on its track).
 */
export function labelLines(
  lines: readonly { track: 'mic' | 'system'; start_ms: number; end_ms: number; text: string }[],
  names: UndiarizedNames | null,
): LabelledLine[] {
  const speakers: LiveSpeakersState = names ? { system: names.system, mic: names.mic } : NO_SPEAKERS;
  const single = names?.single_local_speaker ?? true;
  const voiced = lines.map((l) => ({ ...l, voice: voiceOf(l.track, l, speakers, single) }));
  return groupTurns(voiced, false).map((t) => {
    const label = labelOf(t, speakers, single);
    return { label: label === 'Me' ? null : label, side: label === 'Me' ? 'me' : 'them', text: t.text };
  });
}

/**
 * One entry per `finals` entry: the label to print above that line, or null
 * when it matches the line above (or the entry is a marker). A marker resets
 * the comparison.
 */
export function captionLabels(
  finals: readonly TurnEntry[],
  speakers: LiveSpeakersState,
  singleLocalSpeaker: boolean,
): (string | null)[] {
  let prev: string | null | undefined;
  return finals.map((e) => {
    if (isMarker(e)) {
      prev = undefined;
      return null;
    }
    const label = speakerLabel(e.track, e, speakers, singleLocalSpeaker);
    const shown = label !== null && label !== prev ? label : null;
    prev = label;
    return shown;
  });
}

/**
 * Appends a final, merging it into the previous line when it reads on by
 * the transcript's rule (`readsOn` with each line's slot as its voice,
 * closed at three sentences or a caption's size) on the same track with nothing in between.
 * Breaks on a track change or a pause marker. The merged line keeps the
 * first segment's `start_ms` and only ever replaces the last entry; earlier
 * entries keep their references. Backend `live_transcript.jsonl` stays
 * granular (promotion is unaffected); this shapes the in-app display only.
 */
export function mergeFinalInto(
  finals: TurnEntry[],
  turn: LiveTurn,
  speakers: LiveSpeakersState = NO_SPEAKERS,
): TurnEntry[] {
  const last = finals[finals.length - 1];
  if (
    last &&
    !isMarker(last) &&
    last.track === turn.track &&
    !closed(last.text, true) &&
    readsOn(
      turn.start_ms,
      slotForSpan(speakers[turn.track], turn.start_ms, turn.end_ms),
      last.text,
      last.end_ms,
      lineSlot(last, speakers),
    )
  ) {
    const merged: LiveTurn = {
      ...last,
      text: join(last.text.trim(), turn.text.trim()),
      end_ms: Math.max(last.end_ms, turn.end_ms),
      parts: [...partsOf(last), ...partsOf(turn)],
    };
    return [...finals.slice(0, -1), merged];
  }
  return [...finals, turn];
}

/** A line's voice: the first slot any of its finals maps to. */
function lineSlot(t: LiveTurn, speakers: LiveSpeakersState): number | null {
  for (const p of partsOf(t)) {
    const slot = slotForSpan(speakers[t.track], p.start_ms, p.end_ms);
    if (slot !== null) return slot;
  }
  return null;
}

function partsOf(t: LiveTurn): LivePart[] {
  return t.parts ?? [{ start_ms: t.start_ms, end_ms: t.end_ms, text: t.text }];
}

/**
 * Re-paragraphs each merged line by `mergeFinalInto` after its finals' slots
 * change. Lines that stay whole keep their references.
 */
export function regroupFinals(finals: TurnEntry[], speakers: LiveSpeakersState): TurnEntry[] {
  let changed = false;
  const out: TurnEntry[] = [];
  for (const e of finals) {
    if (isMarker(e) || !e.parts || e.parts.length < 2) {
      out.push(e);
      continue;
    }
    let lines: TurnEntry[] = [];
    for (const p of e.parts) lines = mergeFinalInto(lines, { track: e.track, ...p, isInterim: false }, speakers);
    if (lines.length === 1) {
      out.push(e);
      continue;
    }
    changed = true;
    out.push(...lines);
  }
  return changed ? out : finals;
}

/** Closed turns per slot of one track, as the events delivered them. */
export type ClosedTurns = Map<number, { start_s: number; end_s: number }[]>;

/**
 * One track's speakers after a `live-speakers` event: closed turns kept and
 * extended (replaced on a `full` event), each slot's open turn and name
 * taken from the event.
 */
export function applySpeakers(
  closed: ClosedTurns,
  shown: readonly LiveSpeakerTrack[],
  p: LiveSpeakersPayload,
): { closed: ClosedTurns; shown: LiveSpeakerTrack[] } {
  const nextClosed: ClosedTurns = p.full ? new Map() : new Map(closed);
  const bySlot = new Map<number, LiveSpeakerTrack>(p.full ? [] : shown.map((t) => [t.slot, t]));
  for (const t of p.tracks) {
    const turns = p.full ? t.turns : [...(nextClosed.get(t.slot) ?? []), ...t.turns];
    nextClosed.set(t.slot, turns);
    bySlot.set(t.slot, { slot: t.slot, turns: [...turns, ...t.open], name: t.name });
  }
  return { closed: nextClosed, shown: [...bySlot.values()].sort((a, b) => a.slot - b.slot) };
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
  speakers: LiveSpeakersState;
}

const EMPTY: LiveTranscriptState = {
  sessionId: null,
  finals: [],
  interimMic: null,
  interimSystem: null,
  liveError: null,
  speakers: NO_SPEAKERS,
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
      patch({ finals: mergeFinalInto(state.finals, turn, state.speakers), interimSystem: null });
    } else {
      patch({ finals: mergeFinalInto(state.finals, turn, state.speakers), interimMic: null });
    }
  } else if (p.track === 'mic') {
    patch({ interimMic: turn });
  } else {
    patch({ interimSystem: turn });
  }
}

let closedTurns: Record<'system' | 'mic', ClosedTurns> = { system: new Map(), mic: new Map() };

function handleSpeakers(p: LiveSpeakersPayload): void {
  if (p.session_id !== state.sessionId) return;
  const next = applySpeakers(closedTurns[p.track], state.speakers[p.track], p);
  closedTurns = { ...closedTurns, [p.track]: next.closed };
  const speakers = { ...state.speakers, [p.track]: next.shown };
  patch({ speakers, finals: regroupFinals(state.finals, speakers) });
}

let speakersListener: Promise<UnlistenFn> | null = null;

function attachListenerOnce(): void {
  if (!speakersListener) {
    speakersListener = listen<LiveSpeakersPayload>('live-speakers', (ev) => {
      handleSpeakers(ev.payload);
    }).catch((e) => {
      speakersListener = null;
      throw e;
    });
  }
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
  closedTurns = { system: new Map(), mic: new Map() };
  emit();
}

/** Append a pause marker into the running transcript. */
export function appendPauseMarker(at: string): void {
  patch({ finals: [...state.finals, { marker: 'pause', at }] });
}

/** Append the system-sleep notice into `sessionId`'s running transcript. */
export function appendSleepMarker(sessionId: string, note: string): void {
  if (state.sessionId !== sessionId) return;
  patch({ finals: [...state.finals, { marker: 'pause', at: '', note }] });
}
