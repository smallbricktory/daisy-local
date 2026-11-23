// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, waitFor, act } from '@testing-library/react';

// Spies used by the vi.mock factory below.
const { mockReadSession, mockListSpeakers } = vi.hoisted(() => ({
  mockReadSession: vi.fn(),
  mockListSpeakers: vi.fn(),
}));

// Keeps the real named exports; overrides the driven IPC methods and sets
// provider 'None', which routes to the transcript tab.
vi.mock('../tauri', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../tauri')>();
  return {
    ...actual,
    tauri: {
      ...actual.tauri,
      builtinModelStatus: vi.fn().mockResolvedValue({ model: null, provider: { state: 'None', provider: null, hint: null } }),
      readSession: (id: string) => mockReadSession(id),
      sessionMetaGet: vi.fn().mockResolvedValue({ session_id: 's1', title: 'T', attendees: [], tag_ids: [] }),
      summaryLoad: vi.fn().mockResolvedValue(null),
      listTags: vi.fn().mockResolvedValue([]),
      loadSessionChapters: vi.fn().mockResolvedValue(null),
      listSessionSpeakers: () => mockListSpeakers(),
      sessionHasPlaybackAudio: vi.fn().mockResolvedValue(false),
    },
  };
});

// Controllable library bus; SummaryPane reloads through useSessionData, which
// subscribes via subscribeToLibrary.
let busHandlers: ((ev: { kind: string; session_id: string }) => void)[] = [];
vi.mock('../lib/libraryEvents', () => ({
  subscribeToLibrary: (h: (ev: { kind: string; session_id: string }) => void) => {
    busHandlers.push(h);
    return () => { busHandlers = busHandlers.filter((x) => x !== h); };
  },
}));

// Stub SpeakerLabeler.
vi.mock('../components/SpeakerLabeler', () => ({
  SpeakerLabeler: () => null,
  speakerNeedsReview: () => false,
}));

// SummaryPane's progress-only effect calls listen('daisy://…/progress'); the
// stub resolves to a no-op unlisten.
vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn().mockResolvedValue(() => {}),
}));

import { SummaryPane } from './SummaryPane';

function fireLibrary(session_id: string) {
  act(() => { for (const h of busHandlers) h({ kind: 'updated', session_id }); });
}

describe('SummaryPane library:changed refresh', () => {
  beforeEach(() => {
    busHandlers = [];
    mockReadSession.mockReset();
    mockListSpeakers.mockReset();
  });

  it('refetches session data when library:changed fires for this session', async () => {
    mockReadSession.mockResolvedValue({ session_id: 's1', transcript_md: 'old', manifest_json: {}, has_dedup: true, has_transcript: true, has_summary: false });
    mockListSpeakers.mockResolvedValue([]);

    render(<SummaryPane sessionId="s1" onStartSummarize={() => {}} />);
    await screen.findByText('old');  // initial transcript render (provider None → transcript tab)
    const before = mockReadSession.mock.calls.length;

    fireLibrary('s1');

    // The pane reloads via useSessionData, so readSession is called again.
    await waitFor(() => expect(mockReadSession.mock.calls.length).toBeGreaterThan(before));
  });

  it('ignores library:changed for a different session', async () => {
    mockReadSession.mockResolvedValue({ session_id: 's1', transcript_md: 'old', manifest_json: {}, has_dedup: true, has_transcript: true, has_summary: false });
    mockListSpeakers.mockResolvedValue([]);

    render(<SummaryPane sessionId="s1" onStartSummarize={() => {}} />);
    await screen.findByText('old');
    const before = mockReadSession.mock.calls.length;

    fireLibrary('other');
    await new Promise((r) => setTimeout(r, 20));
    expect(mockReadSession.mock.calls.length).toBe(before);
  });
});
