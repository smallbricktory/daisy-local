// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { describe, it, expect } from 'vitest';
import { countNoteFor, countSourceLine } from './SpeakerLabeler';

describe('countSourceLine', () => {
  it('names each source', () => {
    expect(countSourceLine('typed', 3, true)).toBe('3 speakers, as entered');
    expect(countSourceLine('voiceprints', 2, true)).toBe('2 speakers, from saved voices');
    expect(countSourceLine('expected', 4, true)).toBe('4 speakers, from expected speakers');
    expect(countSourceLine('attendees', 5, true)).toBe('5 speakers, from calendar invitees');
  });

  it('points an estimate at the count field only when the field is on screen', () => {
    expect(countSourceLine('auto', 1, true)).toBe(
      '1 speaker, estimated. Enter the number of speakers in the count field to correct it.',
    );
    expect(countSourceLine('auto', 1, false)).toBe('1 speaker, estimated.');
  });
});

describe('countSourceLine over an explicit count', () => {
  it('says how many voices split off above a typed count', () => {
    expect(countSourceLine('typed', 11, true, 9)).toBe(
      'You asked for 9 speakers; found 11 — 2 voices split off as separate people.',
    );
    expect(countSourceLine('typed', 3, true, 2)).toBe(
      'You asked for 2 speakers; found 3 — 1 voice split off as a separate person.',
    );
  });

  it('says the same for expected speakers', () => {
    expect(countSourceLine('expected', 6, true, 4)).toBe(
      'You expected 4 speakers; found 6 — 2 voices split off as separate people.',
    );
  });

  it('keeps the plain line when the run held to the count or the source was not a stated number', () => {
    expect(countSourceLine('typed', 9, true, 9)).toBe('9 speakers, as entered');
    expect(countSourceLine('voiceprints', 5, true, null)).toBe('5 speakers, from saved voices');
    expect(countSourceLine('attendees', 5, true, 3)).toBe('5 speakers, from calendar invitees');
  });
});

describe('countNoteFor', () => {
  it('states the speakers grouped on the counted track', () => {
    const c = { source: 'typed' as const, requested: null, found: 2 };
    expect(countNoteFor(c)).toEqual({ source: 'typed', speakers: 2, requested: null });
    expect(countSourceLine('typed', countNoteFor(c)!.speakers, true)).toBe('2 speakers, as entered');
  });

  it('carries the stated number through', () => {
    const c = { source: 'typed' as const, requested: 9, found: 11 };
    expect(countNoteFor(c)).toEqual({ source: 'typed', speakers: 11, requested: 9 });
  });

  it('adds no line when no run recorded a count', () => {
    expect(countNoteFor(null)).toBeNull();
    expect(countNoteFor({ source: 'auto', requested: null, found: 0 })).toBeNull();
  });
});
