// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { describe, it, expect, vi } from 'vitest';
import { render, screen, fireEvent } from '@testing-library/react';

const { mockSave } = vi.hoisted(() => ({ mockSave: vi.fn().mockResolvedValue(undefined) }));

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
      sessionNotesLoad: vi.fn().mockRejectedValue('sealed data did not authenticate'),
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

import { SummaryPane } from './SummaryPane';
import { NOTES_UNREADABLE } from '../encryptionCopy';

describe('SummaryPane notes that do not open', () => {
  it('shows that they did not open and offers no editor to write over them', async () => {
    render(<SummaryPane sessionId="s1" onStartSummarize={() => {}} />);
    fireEvent.click(await screen.findByTestId('session-tab-notes'));
    const shown = await screen.findByTestId('notes-unreadable');
    expect(shown.textContent).toContain(NOTES_UNREADABLE);
    expect(screen.queryByTestId('md-editor')).toBeNull();
    expect(mockSave).not.toHaveBeenCalled();
  });
});
