// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, waitFor, act } from '@testing-library/react';

const { mockHas, mockUrl } = vi.hoisted(() => ({
  mockHas: vi.fn(),
  mockUrl: vi.fn(),
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
      sessionHasPlaybackAudio: (id: string) => mockHas(id),
      sessionPlaybackAudioUrl: (id: string) => mockUrl(id),
    },
  };
});

vi.mock('../lib/libraryEvents', () => ({ subscribeToLibrary: () => () => {} }));
vi.mock('../components/SpeakerLabeler', () => ({
  SpeakerLabeler: () => null,
  speakerNeedsReview: () => false,
}));

let progress: Record<string, (ev: { payload: { stage: string } }) => void> = {};
vi.mock('@tauri-apps/api/event', () => ({
  listen: (name: string, h: (ev: { payload: { stage: string } }) => void) => {
    progress[name] = h;
    return Promise.resolve(() => { delete progress[name]; });
  },
}));

import { SummaryPane } from './SummaryPane';

describe('SummaryPane playback audio', () => {
  beforeEach(() => {
    progress = {};
    mockHas.mockReset().mockResolvedValue(true);
    mockUrl.mockReset();
  });

  it('shows the "can\'t play" state when the first load fails', async () => {
    mockUrl.mockRejectedValue('vault is locked');
    render(<SummaryPane sessionId="s1" onStartSummarize={() => {}} />);
    expect(await screen.findByText(/can't play/)).toBeTruthy();
    expect(screen.queryByTestId('session-playback')).toBeNull();
  });

  it('reloads on done and ignores compressing', async () => {
    mockUrl.mockResolvedValue('file:///a/meeting.opus');
    render(<SummaryPane sessionId="s1" onStartSummarize={() => {}} />);
    await screen.findByTestId('session-playback');
    await waitFor(() => expect(progress['daisy://session/s1/progress']).toBeTruthy());
    const calls = mockUrl.mock.calls.length;
    act(() => progress['daisy://session/s1/progress']({ payload: { stage: 'compressing' } }));
    await Promise.resolve();
    expect(mockUrl.mock.calls.length).toBe(calls);
    act(() => progress['daisy://session/s1/progress']({ payload: { stage: 'done' } }));
    await waitFor(() => expect(mockUrl.mock.calls.length).toBe(calls + 1));
  });

  it('drops a reload that resolves after the session switched', async () => {
    mockUrl.mockResolvedValue('file:///a/meeting.opus');
    const { rerender } = render(<SummaryPane sessionId="s1" onStartSummarize={() => {}} />);
    await screen.findByTestId('session-playback');
    await waitFor(() => expect(progress['daisy://session/s1/progress']).toBeTruthy());
    let release: (u: string) => void = () => {};
    mockUrl.mockImplementation((id: string) =>
      id === 's1' ? new Promise<string>((r) => { release = r; }) : Promise.resolve('file:///b/meeting.opus'));
    act(() => progress['daisy://session/s1/progress']({ payload: { stage: 'done' } }));
    rerender(<SummaryPane sessionId="s2" onStartSummarize={() => {}} />);
    await waitFor(() => expect(screen.getByTestId('session-playback').getAttribute('src')).toBe('file:///b/meeting.opus'));
    await act(async () => { release('file:///stale/meeting.opus'); });
    expect(screen.getByTestId('session-playback').getAttribute('src')).toBe('file:///b/meeting.opus');
  });
});
