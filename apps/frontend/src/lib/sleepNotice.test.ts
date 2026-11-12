// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { describe, it, expect } from 'vitest';
import { sleepNoticeText, sleepNotices, sleptToast } from './sleepNotice';
import { fmtTime } from './format';
import { appendSleepMarker, getLiveTranscriptState, resetLiveTranscript } from '../liveTranscript';

const SLEPT_AT = 1_790_888_552;
const WOKE_AT = SLEPT_AT + 212;

describe('sleepNoticeText', () => {
  it('spans the sleep in local clock time', () => {
    expect(sleepNoticeText(SLEPT_AT, WOKE_AT)).toBe(
      `Recording paused while the computer slept (${fmtTime(SLEPT_AT)}–${fmtTime(WOKE_AT)})`,
    );
  });
});

describe('sleepNotices', () => {
  it('reads one notice per segment opened after a system sleep', () => {
    const notices = sleepNotices([
      { started_at_unix_seconds: SLEPT_AT - 600, stopped_at_unix_seconds: SLEPT_AT },
      { started_at_unix_seconds: WOKE_AT, stopped_at_unix_seconds: WOKE_AT + 60, reason: 'system_sleep' },
      { started_at_unix_seconds: WOKE_AT + 300, stopped_at_unix_seconds: null, reason: 'user' },
    ]);
    expect(notices).toEqual([sleepNoticeText(SLEPT_AT, WOKE_AT)]);
  });

  it('is empty for a session without a sleep, and for no manifest', () => {
    expect(sleepNotices([{ started_at_unix_seconds: 1, stopped_at_unix_seconds: 2 }])).toEqual([]);
    expect(sleepNotices(undefined)).toEqual([]);
  });
});

describe('sleptToast', () => {
  it('reports an automatic resume as info', () => {
    const t = sleptToast({
      session_id: 's', paused_at_unix_seconds: SLEPT_AT, resumed_at_unix_seconds: WOKE_AT, resumed: true,
    });
    expect(t.severity).toBe('info');
    expect(t.title).toBe(sleepNoticeText(SLEPT_AT, WOKE_AT));
  });

  it('asks for a manual resume when the restart failed', () => {
    const t = sleptToast({
      session_id: 's', paused_at_unix_seconds: SLEPT_AT, resumed_at_unix_seconds: WOKE_AT, resumed: false,
    });
    expect(t.severity).toBe('warning');
    expect(t.body).toContain('Resume');
  });
});

describe('appendSleepMarker', () => {
  it('adds the notice to the matching session only', () => {
    resetLiveTranscript('s1');
    appendSleepMarker('other', 'x');
    expect(getLiveTranscriptState().finals).toEqual([]);
    appendSleepMarker('s1', 'note');
    expect(getLiveTranscriptState().finals).toEqual([{ marker: 'pause', at: '', note: 'note' }]);
    resetLiveTranscript(null);
  });
});
