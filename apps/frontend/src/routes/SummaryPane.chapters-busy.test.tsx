// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
/**
 * Chapter extraction and the summary generator contend for the same
 * single-worker local model, so each must disable the other's buttons —
 * and a double-click on Extract must not fire two requests. The backend job
 * registry gates them across sessions and holds the running state while the
 * pane is unmounted.
 */
import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent, act, waitFor } from '@testing-library/react';
import type { AiJob } from '../tauri';

let emitAiJobs: ((e: { payload: AiJob[] }) => void) | null = null;
vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn((event: string, cb: (e: { payload: AiJob[] }) => void) => {
    if (event === 'ai-jobs:changed') emitAiJobs = cb;
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

vi.mock('../tauri', () => ({
  tauri: {
    builtinModelStatus: vi.fn(() => Promise.resolve({ model: null, provider: { state: 'Configured', provider: 'groq', hint: null } })),
    readSession: vi.fn(),
    sessionMetaGet: vi.fn(),
    summaryLoad: vi.fn(),
    listTags: vi.fn(),
    loadSessionChapters: vi.fn(),
    sessionNotesLoad: vi.fn(),
    liveChatLoad: vi.fn(() => Promise.resolve({ messages: [], transcript_cursor_ms: 0 })),
    sessionHasPlaybackAudio: vi.fn(),
    sessionPlaybackAudioBytes: vi.fn(() => Promise.resolve(new ArrayBuffer(0))),
    listIntegrations: vi.fn(() => Promise.resolve([])),
    listSessionSpeakers: vi.fn(() => Promise.resolve([])),
    listPrompts: vi.fn(() => Promise.resolve([])),
    extractSessionChapters: vi.fn(),
    aiJobsList: vi.fn(() => Promise.resolve([])),
  },
  errStr: vi.fn((e: unknown) => String(e)),
  formatDurationHm: vi.fn(() => '—'),
  LANGUAGES: [],
  copyToClipboard: vi.fn(),
}));

import { SummaryPane } from './SummaryPane';
import { tauri as mockTauri } from '../tauri';
import { __resetAiJobsForTests } from '../lib/aiJobs';

const AI_BUSY_TITLE = 'Another AI task is still running';
const aiJob = (sessionId: string, kind: AiJob['kind']): AiJob => ({
  session_id: sessionId, kind, started_at_unix_seconds: 1_700_000_000,
});

/** Pushes `jobs` into the store the pane subscribed, as the backend would. */
async function seedAiJobs(jobs: AiJob[]): Promise<void> {
  await waitFor(() => expect(emitAiJobs).not.toBeNull());
  emitAiJobs!({ payload: jobs });
}

const VIEW = {
  session_id: 'sess-1',
  manifest_json: {
    schema_version: 2,
    created_at_unix_seconds: 1_700_000_000,
    finalized_at_unix_seconds: 1_700_001_000,
    chunks: [{ duration_seconds: 600 }],
  },
  transcript_md: '[00:00:01] **Me**: Hello',
  has_transcript: true,
  has_dedup: true,
  has_summary: false,
};

const META = {
  session_id: 'sess-1',
  title: 'Test meeting',
  tag_ids: [],
  attendees: [],
  has_notes: false,
  has_summary: false,
  recording_segments: 1,
};

beforeEach(() => {
  vi.clearAllMocks();
  __resetAiJobsForTests();
  emitAiJobs = null;
  (mockTauri.readSession as ReturnType<typeof vi.fn>).mockResolvedValue(VIEW);
  (mockTauri.sessionMetaGet as ReturnType<typeof vi.fn>).mockResolvedValue(META);
  // No summary yet — the Summary tab renders "Generate summary".
  (mockTauri.summaryLoad as ReturnType<typeof vi.fn>).mockResolvedValue(null);
  (mockTauri.listTags as ReturnType<typeof vi.fn>).mockResolvedValue([]);
  (mockTauri.loadSessionChapters as ReturnType<typeof vi.fn>).mockResolvedValue(null);
  (mockTauri.sessionNotesLoad as ReturnType<typeof vi.fn>).mockResolvedValue('');
  (mockTauri.sessionHasPlaybackAudio as ReturnType<typeof vi.fn>).mockResolvedValue(false);
  (mockTauri.listIntegrations as ReturnType<typeof vi.fn>).mockResolvedValue([]);
  (mockTauri.listSessionSpeakers as ReturnType<typeof vi.fn>).mockResolvedValue([]);
  (mockTauri.sessionPlaybackAudioBytes as ReturnType<typeof vi.fn>).mockResolvedValue(new ArrayBuffer(0));
  (mockTauri.listPrompts as ReturnType<typeof vi.fn>).mockResolvedValue([]);
});

/** A promise plus its resolver, for holding an async call open mid-test. */
function deferred<T>(): { promise: Promise<T>; resolve: (v: T) => void } {
  let resolve!: (v: T) => void;
  const promise = new Promise<T>((res) => { resolve = res; });
  return { promise, resolve };
}

describe('SummaryPane: chapters/summary single-worker busy interlock', () => {
  it('extraction in progress disables Generate summary; finishing re-enables it', async () => {
    const { promise, resolve } = deferred<{ chapters: []; skipped: boolean; reason: string | null }>();
    // The backend registers the job before it starts, and drops it when done.
    (mockTauri.extractSessionChapters as ReturnType<typeof vi.fn>).mockImplementation(() => {
      emitAiJobs?.({ payload: [aiJob('sess-1', 'chapters')] });
      return promise.finally(() => emitAiJobs?.({ payload: [] }));
    });

    await act(async () => {
      render(<SummaryPane sessionId="sess-1" onStartSummarize={() => {}} />);
    });
    await waitFor(() => expect(emitAiJobs).not.toBeNull());

    // No summary yet → the auto-router lands on the Transcript tab.
    const extractBtn = await screen.findByTestId('session-chapters-extract');
    await act(async () => {
      fireEvent.click(extractBtn);
    });
    expect(extractBtn).toHaveTextContent('Extracting…');

    await act(async () => {
      fireEvent.click(screen.getByTestId('session-tab-summary'));
    });
    const generateBtn = screen.getByTestId('session-summary-generate');
    expect(generateBtn).toBeDisabled();

    await act(async () => {
      resolve({ chapters: [], skipped: true, reason: 'no transcript' });
      await promise;
    });

    expect(screen.getByTestId('session-summary-generate')).not.toBeDisabled();
  });

  it('a summary job in progress (isProcessing) disables Extract chapters', async () => {
    await act(async () => {
      render(<SummaryPane sessionId="sess-1" onStartSummarize={() => {}} isProcessing />);
    });
    const extractBtn = await screen.findByTestId('session-chapters-extract');
    expect(extractBtn).toBeDisabled();
  });

  it('a job for another session disables Extract chapters and says why', async () => {
    await act(async () => {
      render(<SummaryPane sessionId="sess-1" onStartSummarize={() => {}} />);
    });
    await act(async () => { await seedAiJobs([aiJob('sess-other', 'summary')]); });

    const extractBtn = screen.getByTestId('session-chapters-extract');
    expect(extractBtn).toBeDisabled();
    expect(extractBtn).toHaveAttribute('title', AI_BUSY_TITLE);

    await act(async () => {
      fireEvent.click(screen.getByTestId('session-tab-summary'));
    });
    const generateBtn = screen.getByTestId('session-summary-generate');
    expect(generateBtn).toBeDisabled();
    expect(generateBtn).toHaveAttribute('title', AI_BUSY_TITLE);
  });

  it('a job for this session shows the running label on a freshly mounted pane', async () => {
    await act(async () => {
      render(<SummaryPane sessionId="sess-1" onStartSummarize={() => {}} />);
    });
    await act(async () => { await seedAiJobs([aiJob('sess-1', 'chapters')]); });

    expect(screen.getByTestId('session-chapters-extract')).toHaveTextContent('Extracting…');
  });

  it('a job for this session leaving the registry re-reads the session', async () => {
    await act(async () => {
      render(<SummaryPane sessionId="sess-1" onStartSummarize={() => {}} />);
    });
    await act(async () => { await seedAiJobs([aiJob('sess-1', 'chapters')]); });
    const readsWhileRunning = (mockTauri.readSession as ReturnType<typeof vi.fn>).mock.calls.length;

    await act(async () => { emitAiJobs?.({ payload: [] }); });

    expect((mockTauri.readSession as ReturnType<typeof vi.fn>).mock.calls.length)
      .toBeGreaterThan(readsWhileRunning);
  });

  it('a second click on Extract chapters while one is in flight is a no-op', async () => {
    const { promise } = deferred<{ chapters: []; skipped: boolean; reason: string | null }>();
    (mockTauri.extractSessionChapters as ReturnType<typeof vi.fn>).mockReturnValue(promise);

    await act(async () => {
      render(<SummaryPane sessionId="sess-1" onStartSummarize={() => {}} />);
    });
    const extractBtn = await screen.findByTestId('session-chapters-extract');

    await act(async () => {
      fireEvent.click(extractBtn);
      fireEvent.click(extractBtn);
    });

    expect(mockTauri.extractSessionChapters).toHaveBeenCalledTimes(1);
  });
});
