import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, waitFor, act } from '@testing-library/react';

// Spies referenced inside the hoisted vi.mock factory must themselves be
// hoisted, or the factory (lifted to the top of the file) sees them
// uninitialized.
const { mockReadSession, mockListSpeakers } = vi.hoisted(() => ({
  mockReadSession: vi.fn(),
  mockListSpeakers: vi.fn(),
}));

// Preserves the real named exports (formatDurationHm, types, etc.);
// overrides only the driven IPC methods + forces provider 'None' — routing
// lands on the transcript tab deterministically.
vi.mock('../tauri', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../tauri')>();
  return {
    ...actual,
    summaryProviderStatus: vi.fn().mockResolvedValue({ state: 'None', provider: null, hint: null }),
    tauri: {
      ...actual.tauri,
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

// Controllable library bus — SummaryPane reloads through useSessionData, which
// subscribes via subscribeToLibrary.
let busHandlers: ((ev: { kind: string; session_id: string }) => void)[] = [];
vi.mock('../lib/libraryEvents', () => ({
  subscribeToLibrary: (h: (ev: { kind: string; session_id: string }) => void) => {
    busHandlers.push(h);
    return () => { busHandlers = busHandlers.filter((x) => x !== h); };
  },
}));

// Don't pull the real SpeakerLabeler (and its own tauri calls) into this test.
vi.mock('../components/SpeakerLabeler', () => ({
  SpeakerLabeler: () => null,
  speakerNeedsReview: () => false,
}));

// SummaryPane's progress-only effect calls listen('daisy://…/progress'); the
// real impl reaches window.__TAURI_INTERNALS__ (absent in jsdom) and
// rejects. Stubbed to a no-op unlisten; no unhandled rejection.
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

    // The pane reloads via useSessionData → readSession is called again. (State→
    // render of the new data is covered by useSessionData's own unit tests; here
    // we assert SummaryPane actually wires the signal to a reload.)
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
