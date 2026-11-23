// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { describe, it, expect, vi } from 'vitest';
import { render, screen, fireEvent, act } from '@testing-library/react';

const { mockSave, loadControl } = vi.hoisted(() => ({
  mockSave: vi.fn().mockResolvedValue(undefined),
  loadControl: { reject: (_e: unknown) => {} },
}));

vi.mock('../tauri', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../tauri')>();
  return {
    ...actual,
    tauri: {
      ...actual.tauri,
      builtinModelStatus: vi.fn().mockResolvedValue({ model: null, provider: { state: 'None', provider: null, hint: null } }),
      readSession: vi.fn().mockResolvedValue({ session_id: 's1', transcript_md: 'hello', manifest_json: {}, has_dedup: true, has_transcript: true, has_summary: false }),
      sessionMetaGet: vi.fn().mockResolvedValue({ session_id: 's1', title: 'T', attendees: [], tag_ids: [] }),
      summaryLoad: vi.fn().mockResolvedValue(null),
      listTags: vi.fn().mockResolvedValue([]),
      loadSessionChapters: vi.fn().mockResolvedValue(null),
      listSessionSpeakers: vi.fn().mockResolvedValue([]),
      sessionHasPlaybackAudio: vi.fn().mockResolvedValue(false),
      sessionNotesLoad: vi.fn(() => new Promise((_res, rej) => { loadControl.reject = rej; })),
      sessionNotesSave: mockSave,
    },
  };
});

vi.mock('../lib/libraryEvents', () => ({ subscribeToLibrary: () => () => {} }));
vi.mock('../components/SpeakerLabeler', () => ({
  SpeakerLabeler: () => null,
  speakerNeedsReview: () => false,
}));
vi.mock('@tauri-apps/api/event', () => ({ listen: () => Promise.resolve(() => {}) }));
// The editor stands in as a textarea that reports whether it accepts edits.
vi.mock('../components/MarkdownEditor', () => ({
  MarkdownEditor: ({ value, onChange, editable }: { value: string; onChange: (v: string) => void; editable?: boolean }) => (
    <textarea data-testid="md-editor" data-editable={String(editable !== false)} value={value} onChange={(e) => onChange(e.target.value)} />
  ),
}));

import { SummaryPane } from './SummaryPane';

describe('SummaryPane notes keystroke before the load resolves', () => {
  it('is read-only until the load resolves, and a rejected load saves nothing', async () => {
    render(<SummaryPane sessionId="s1" onStartSummarize={() => {}} />);
    fireEvent.click(await screen.findByTestId('session-tab-notes'));
    const editor = await screen.findByTestId('md-editor');
    expect(editor.getAttribute('data-editable')).toBe('false');
    vi.useFakeTimers();
    try {
      fireEvent.change(editor, { target: { value: 'typed too early' } });
      await act(async () => { loadControl.reject('sealed data did not authenticate'); });
      await act(async () => { vi.advanceTimersByTime(5000); });
    } finally {
      vi.useRealTimers();
    }
    expect(await screen.findByTestId('notes-unreadable')).toBeTruthy();
    expect(mockSave).not.toHaveBeenCalled();
  });
});
