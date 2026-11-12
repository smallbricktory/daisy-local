// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! How transcript lines read as turns, as `transcript::reading` renders
//! `transcript.md`: sentence ends, sentence counts, and the rule that joins
//! a line onto the turn before it.

/** Two lines of one voice this close read as one turn. */
const MERGE_GAP_MS = 2000;
/** A turn holds at most this many sentences; the next starts a new turn. */
const MAX_TURN_SENTENCES = 3;

/** Sentence ends in scripts that put a space after them. */
const SPACED_ENDS = new Set(['.', '?', '!', '؟', '۔', '।', '॥']);
/** Full-width sentence ends, in scripts written without spaces. */
const CLOSED_ENDS = new Set(['。', '？', '！', '｡']);
/** What may follow a sentence end and still belong to its sentence. */
const CLOSERS = new Set(['"', "'", ')', ']', '”', '’', '」', '』', '）', '》', '〉']);
/** Words a full stop shortens: titles and in-sentence Latin abbreviations. */
const ABBREVIATIONS = new Set([
  'mr', 'mrs', 'ms', 'dr', 'prof', 'st', 'mt', 'jr', 'sr', 'rev', 'fr', 'gen', 'col', 'lt', 'sgt',
  'capt', 'gov', 'sen', 'rep', 'vs', 'e.g', 'i.e', 'cf', 'approx',
]);

const isSpace = (c: string) => /\s/u.test(c);
const isLetter = (c: string) => /\p{L}/u.test(c);

/**
 * Whether `text` ends on a sentence end: a full stop that closes no
 * abbreviation or initial ("John F."), a question or exclamation mark, or a
 * full-width end.
 */
export function endsSentence(text: string): boolean {
  const chars = [...text];
  while (chars.length && (isSpace(chars[chars.length - 1]) || CLOSERS.has(chars[chars.length - 1]))) chars.pop();
  const end = chars.pop();
  if (end === undefined) return false;
  if (CLOSED_ENDS.has(end)) return true;
  if (!SPACED_ENDS.has(end)) return false;
  if (end !== '.') return true;
  let from = chars.length;
  while (from > 0 && !isSpace(chars[from - 1])) from--;
  let word = chars.slice(from);
  while (word.length && !isLetter(word[0])) word = word.slice(1);
  // "I" ends sentences in speech far more often than it stands for a name.
  if (word.length === 1 && word[0] !== word[0].toLowerCase() && word[0] !== 'I') return false;
  return !ABBREVIATIONS.has(word.join('').toLowerCase());
}

/**
 * How many sentences `text` ends: a full-width end counts where it stands,
 * any other end where a space or the text's end follows its word.
 */
export function sentenceCount(text: string): number {
  const chars = [...text];
  let count = 0;
  let start = 0;
  let i = 0;
  const close = (end: number) => {
    if (endsSentence(chars.slice(start, end).join(''))) count += 1;
  };
  while (i < chars.length) {
    if (CLOSED_ENDS.has(chars[i])) {
      let end = i + 1;
      while (end < chars.length && CLOSERS.has(chars[end])) end++;
      count += 1;
      start = end;
      i = end;
    } else if (isSpace(chars[i])) {
      close(i);
      start = i;
      i++;
    } else {
      i++;
    }
  }
  close(chars.length);
  return count;
}

/** Whether `text` already holds the turn's last whole sentence. */
function full(text: string): boolean {
  return endsSentence(text) && sentenceCount(text) >= MAX_TURN_SENTENCES;
}

/** Past this many UTF-8 bytes a caption paragraph closes at its next sentence end. */
const CAPTION_BYTES = 1500;

/** Whether a turn takes no more lines: `full`, or for a caption, past CAPTION_BYTES at a sentence end. */
export function closed(text: string, caption: boolean): boolean {
  if (full(text)) return true;
  return caption && endsSentence(text) && new TextEncoder().encode(text).length >= CAPTION_BYTES;
}

/**
 * Whether a line starting at `startMs` reads on from `text`, which ends at
 * `endMs`; a voice is whoever a line is labelled as (null: nobody). A line
 * labelled as nobody joins. The rest of an unfinished sentence joins however
 * long the pause, unless another voice says it. Whole sentences of one voice
 * join within MERGE_GAP_MS. The sentence cap is `closed`.
 */
export function readsOn<V>(startMs: number, after: V | null, text: string, endMs: number, before: V | null): boolean {
  if (!endsSentence(text)) return before === null || after === null || before === after;
  return startMs <= endMs + MERGE_GAP_MS && (after === null || before === after);
}

/**
 * Two pieces of transcript as one line: a space between them, except where
 * either side is Chinese, Japanese, Thai, Lao, Khmer or Myanmar.
 */
export function join(left: string, right: string): string {
  if (!left) return right;
  if (!right) return left;
  const end = [...left].pop()!.codePointAt(0)!;
  const start = right.codePointAt(0)!;
  return unspaced(end) || unspaced(start) ? left + right : `${left} ${right}`;
}

function unspaced(c: number): boolean {
  return (
    (c >= 0x3000 && c <= 0x303f) || // CJK symbols and punctuation
    (c >= 0x3040 && c <= 0x30ff) || // hiragana, katakana
    (c >= 0x3100 && c <= 0x312f) || // bopomofo
    (c >= 0x31f0 && c <= 0x31ff) || // katakana extension
    (c >= 0x3400 && c <= 0x4dbf) || // CJK extension A
    (c >= 0x4e00 && c <= 0x9fff) || // CJK unified ideographs
    (c >= 0xf900 && c <= 0xfaff) || // CJK compatibility ideographs
    (c >= 0xff00 && c <= 0xffef) || // full-width and half-width forms
    (c >= 0x0e00 && c <= 0x0eff) || // Thai, Lao
    (c >= 0x1000 && c <= 0x109f) || // Myanmar
    (c >= 0x1780 && c <= 0x17ff) || // Khmer
    (c >= 0x20000 && c <= 0x2fa1f) // CJK extensions B and on
  );
}

/** A transcript line as the rule reads it. */
export interface RuleLine {
  track: 'mic' | 'system';
  start_ms: number;
  end_ms: number;
  text: string;
  /** Who the line is labelled as; null: nobody. */
  voice: number | null;
}

/** Lines grouped into one turn. `voice` is the first any line holds. */
export interface RuleTurn<L extends RuleLine> {
  lines: L[];
  text: string;
  start_ms: number;
  end_ms: number;
  track: 'mic' | 'system';
  voice: number | null;
}

/**
 * `lines` as turns, in start order. `perTrack`: a line joins the open turn
 * of its own track, as the saved transcript groups. Otherwise only the turn
 * just before it, on its track, as captions paragraph (`closed` as a caption). An unlabelled line
 * that starts a new turn only because the turn before is full takes that
 * turn's voice.
 */
export function groupTurns<L extends RuleLine>(lines: readonly L[], perTrack: boolean): RuleTurn<L>[] {
  const turns: RuleTurn<L>[] = [];
  const open = new Map<'mic' | 'system', number>();
  for (const line of lines) {
    const at = perTrack ? open.get(line.track) : turns.length > 0 && turns[turns.length - 1].track === line.track ? turns.length - 1 : undefined;
    const last = at === undefined ? undefined : turns[at];
    const reads = !!last && readsOn(line.start_ms, line.voice, last.text, last.end_ms, last.voice);
    if (last && reads && !closed(last.text, !perTrack)) {
      last.lines.push(line);
      last.text = join(last.text, line.text.trim());
      last.end_ms = Math.max(last.end_ms, line.end_ms);
      if (last.voice === null) last.voice = line.voice;
      continue;
    }
    open.set(line.track, turns.length);
    turns.push({
      lines: [line],
      text: line.text.trim(),
      start_ms: line.start_ms,
      end_ms: line.end_ms,
      track: line.track,
      voice: line.voice ?? (last && reads ? last.voice : null),
    });
  }
  return turns;
}
