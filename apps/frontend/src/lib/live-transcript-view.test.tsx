// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, act } from '@testing-library/react';

// Captured `transcript:segment` handler, so a test can push segments into the
// module-level live store the way the backend would.
const handlers: Record<string, (e: { payload: unknown }) => void> = {};

vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn((name: string, cb: (e: { payload: unknown }) => void) => {
    handlers[name] = cb;
    return Promise.resolve(() => {});
  }),
}));

vi.mock('@tauri-apps/plugin-dialog', () => ({ save: vi.fn() }));
vi.mock('../components/tags/TagChip', () => ({ TagChip: () => null }));
vi.mock('../components/tags/TagCombobox', () => ({ TagCombobox: () => null }));
vi.mock('../components/tags/TagPromptModal', () => ({ TagPromptModal: () => null }));
vi.mock('../components/ConfirmDialog', () => ({ ConfirmDialog: () => null }));
vi.mock('../components/MarkdownView', () => ({
  MarkdownView: ({ markdown }: { markdown: string }) => <pre>{markdown}</pre>,
}));

const mockReadLiveTranscript = vi.fn(() => Promise.resolve([] as { track: string; text: string }[]));
const mockReadLiveNames = vi.fn(() => Promise.resolve(null as unknown));
const mockRecordingSnapshot = vi.fn(() => Promise.resolve(null));

vi.mock('../tauri', () => ({
  tauri: {
    builtinModelStatus: vi.fn(() => Promise.resolve({ model: null, provider: null })),
    readLiveTranscript: (...a: unknown[]) => mockReadLiveTranscript(...(a as [])),
    readLiveNames: (...a: unknown[]) => mockReadLiveNames(...(a as [])),
    recordingSnapshot: () => mockRecordingSnapshot(),
  },
  errStr: vi.fn((e: unknown) => String(e)),
  formatDurationHm: vi.fn(() => '—'),
  formatDurationSeconds: vi.fn(() => '—'),
  LANGUAGES: [],
  copyToClipboard: vi.fn(),
}));

import { TranscriptTab } from '../routes/SummaryPane';
import { resetLiveTranscript } from '../liveTranscript';

function pushFinal(track: 'mic' | 'system', text: string, start_ms: number): void {
  act(() => {
    handlers['transcript:segment']({
      payload: { track, kind: { type: 'final', start_ms, end_ms: start_ms + 1000, text, confidence: null } },
    });
  });
}

beforeEach(() => {
  vi.clearAllMocks();
  resetLiveTranscript(null);
});

describe('<TranscriptTab /> live transcript while a recording is still in progress', () => {
  it('shows the live store instead of the empty state when no finalize is running', () => {
    resetLiveTranscript('s1');
    render(<TranscriptTab md={null} sessionId="s1" finalizing={false} />);
    pushFinal('system', 'Ferris said the harbour bell rang twice', 0);

    expect(screen.getByText(/harbour bell rang twice/)).toBeInTheDocument();
    expect(screen.queryByText(/No transcript yet/)).not.toBeInTheDocument();
  });

  it('still shows the live store when a finalize is running', () => {
    resetLiveTranscript('s2');
    render(<TranscriptTab md={null} sessionId="s2" finalizing />);
    pushFinal('mic', 'Wren answered from the pier', 0);

    expect(screen.getByText(/Wren answered from the pier/)).toBeInTheDocument();
  });

  it('keeps the empty state for a session with no live transcript at all', async () => {
    resetLiveTranscript(null);
    render(<TranscriptTab md={null} sessionId="s3" finalizing={false} />);
    await act(async () => { await Promise.resolve(); await Promise.resolve(); });

    expect(screen.getByText(/No transcript yet/)).toBeInTheDocument();
  });

  it('falls back to the on-disk live transcript when the store was cleared', async () => {
    mockReadLiveTranscript.mockResolvedValueOnce([
      { track: 'system', text: 'Ferris read the tide table aloud' },
    ]);
    resetLiveTranscript(null);
    render(<TranscriptTab md={null} sessionId="s4" finalizing={false} />);
    await act(async () => { await Promise.resolve(); await Promise.resolve(); });

    expect(screen.getByText(/tide table aloud/)).toBeInTheDocument();
    expect(screen.queryByText(/No transcript yet/)).not.toBeInTheDocument();
  });
});

describe('<TranscriptTab /> finalizing live transcript names', () => {
  const names = {
    system: [{ slot: 1, turns: [{ start_s: 0, end_s: 3 }], name: { voice_id: 'v-1', display_name: 'Ana Okafor' } }],
    mic: [],
    single_local_speaker: true,
    lines: [],
  };

  it('labels the in-memory lines from the overlay', async () => {
    mockReadLiveNames.mockResolvedValueOnce(names);
    resetLiveTranscript('s5');
    render(<TranscriptTab md={null} sessionId="s5" finalizing />);
    pushFinal('system', 'Ferris read the tide table', 0);
    pushFinal('mic', 'Noted, thanks', 3_000);
    pushFinal('system', 'Then a late remark', 30_000);
    await act(async () => { await Promise.resolve(); await Promise.resolve(); });

    expect(screen.getByText('Ana Okafor')).toBeInTheDocument();
    expect(screen.getByText('Them')).toBeInTheDocument();
  });

  it('labels the on-disk lines from the overlay', async () => {
    mockReadLiveNames.mockResolvedValueOnce(names);
    mockReadLiveTranscript.mockResolvedValueOnce([
      { track: 'system', start_ms: 500, end_ms: 2500, text: 'Wren checked the nets', kind: 'final' },
    ] as never);
    resetLiveTranscript(null);
    render(<TranscriptTab md={null} sessionId="s6" finalizing />);
    await act(async () => { await Promise.resolve(); await Promise.resolve(); await Promise.resolve(); });

    expect(screen.getByText('Ana Okafor')).toBeInTheDocument();
    expect(screen.queryByText('Them')).not.toBeInTheDocument();
  });
});
