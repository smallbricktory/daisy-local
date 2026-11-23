// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, waitFor, act } from '@testing-library/react';

const { mockHas, mockUrl, mockRelease } = vi.hoisted(() => ({
  mockHas: vi.fn(),
  mockUrl: vi.fn(),
  mockRelease: vi.fn(),
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
      sessionPlaybackUrl: (id: string) => mockUrl(id),
      sessionPlaybackRelease: (url: string) => mockRelease(url),
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

import { SummaryPane, untrimmedNote } from './SummaryPane';

describe('SummaryPane playback audio', () => {
  beforeEach(() => {
    progress = {};
    mockHas.mockReset().mockResolvedValue(true);
    mockUrl.mockReset();
    mockRelease.mockReset().mockResolvedValue(false);
  });

  it('shows the "can\'t play" state when the first load fails', async () => {
    mockUrl.mockRejectedValue('vault is locked');
    render(<SummaryPane sessionId="s1" onStartSummarize={() => {}} />);
    expect(await screen.findByText(/can't play/)).toBeTruthy();
    expect(screen.queryByTestId('session-playback')).toBeNull();
  });

  it('reloads on done and ignores compressing', async () => {
    mockUrl.mockResolvedValue('daisy-media://localhost/session/s1/meeting');
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
    mockUrl.mockResolvedValue('daisy-media://localhost/session/s1/meeting');
    const { rerender } = render(<SummaryPane sessionId="s1" onStartSummarize={() => {}} />);
    await screen.findByTestId('session-playback');
    await waitFor(() => expect(progress['daisy://session/s1/progress']).toBeTruthy());
    let release: (u: string) => void = () => {};
    mockUrl.mockImplementation((id: string) =>
      id === 's1' ? new Promise<string>((r) => { release = r; }) : Promise.resolve('daisy-media://localhost/session/s2/meeting'));
    act(() => progress['daisy://session/s1/progress']({ payload: { stage: 'done' } }));
    rerender(<SummaryPane sessionId="s2" onStartSummarize={() => {}} />);
    await waitFor(() => expect(screen.getByTestId('session-playback').getAttribute('src')).toBe('daisy-media://localhost/session/s2/meeting'));
    await act(async () => { release('daisy-media://localhost/session/stale/meeting'); });
    expect(screen.getByTestId('session-playback').getAttribute('src')).toBe('daisy-media://localhost/session/s2/meeting');
  });

  it('releases the decrypted file when the player moves to another session or closes', async () => {
    mockUrl.mockImplementation((id: string) => Promise.resolve(`file:///run/user/1000/daisy-media/${id}-x.opus`));
    const { rerender, unmount } = render(<SummaryPane sessionId="s1" onStartSummarize={() => {}} />);
    await screen.findByTestId('session-playback');
    rerender(<SummaryPane sessionId="s2" onStartSummarize={() => {}} />);
    await waitFor(() => expect(mockRelease).toHaveBeenCalledWith('file:///run/user/1000/daisy-media/s1-x.opus'));
    await waitFor(() => expect(screen.getByTestId('session-playback').getAttribute('src')).toBe('file:///run/user/1000/daisy-media/s2-x.opus'));
    unmount();
    expect(mockRelease).toHaveBeenLastCalledWith('file:///run/user/1000/daisy-media/s2-x.opus');
  });

  it('a stale file that replaced the open session\'s is released and the open session loads again', async () => {
    let finishS1: (u: string) => void = () => {};
    let loadsOfS2 = 0;
    mockUrl.mockImplementation((id: string) => {
      if (id === 's1') return new Promise<string>((r) => { finishS1 = r; });
      loadsOfS2 += 1;
      return Promise.resolve(`file:///run/user/1000/daisy-media/s2-${loadsOfS2}.opus`);
    });
    mockRelease.mockImplementation((url: string) => Promise.resolve(url.includes('stale')));
    const { rerender } = render(<SummaryPane sessionId="s1" onStartSummarize={() => {}} />);
    await waitFor(() => expect(mockUrl).toHaveBeenCalledWith('s1'));
    rerender(<SummaryPane sessionId="s2" onStartSummarize={() => {}} />);
    await waitFor(() => expect(screen.getByTestId('session-playback').getAttribute('src')).toBe('file:///run/user/1000/daisy-media/s2-1.opus'));
    await act(async () => { finishS1('file:///run/user/1000/daisy-media/stale.opus'); });
    expect(mockRelease).toHaveBeenCalledWith('file:///run/user/1000/daisy-media/stale.opus');
    await waitFor(() => expect(screen.getByTestId('session-playback').getAttribute('src')).toBe('file:///run/user/1000/daisy-media/s2-2.opus'));
  });

  it('a load still running when the pane closes releases its file once it lands', async () => {
    let finish: (u: string) => void = () => {};
    mockUrl.mockImplementation(() => new Promise<string>((r) => { finish = r; }));
    mockRelease.mockResolvedValue(true);
    const { unmount } = render(<SummaryPane sessionId="s1" onStartSummarize={() => {}} />);
    await waitFor(() => expect(mockUrl).toHaveBeenCalledWith('s1'));
    unmount();
    await act(async () => { finish('file:///run/user/1000/daisy-media/late.opus'); });
    await waitFor(() => expect(mockRelease).toHaveBeenCalledWith('file:///run/user/1000/daisy-media/late.opus'));
    await act(async () => { await Promise.resolve(); });
    expect(mockUrl).toHaveBeenCalledTimes(1);
  });

  it('shows the "can\'t play" state when the reload after a stale release fails', async () => {
    let finishS1: (u: string) => void = () => {};
    let loadsOfS2 = 0;
    mockUrl.mockImplementation((id: string) => {
      if (id === 's1') return new Promise<string>((r) => { finishS1 = r; });
      loadsOfS2 += 1;
      return loadsOfS2 === 1
        ? Promise.resolve('file:///run/user/1000/daisy-media/s2-1.opus')
        : Promise.reject('vault is locked');
    });
    mockRelease.mockImplementation((url: string) => Promise.resolve(url.includes('stale')));
    const { rerender } = render(<SummaryPane sessionId="s1" onStartSummarize={() => {}} />);
    await waitFor(() => expect(mockUrl).toHaveBeenCalledWith('s1'));
    rerender(<SummaryPane sessionId="s2" onStartSummarize={() => {}} />);
    await waitFor(() => expect(screen.getByTestId('session-playback').getAttribute('src')).toBe('file:///run/user/1000/daisy-media/s2-1.opus'));
    expect(screen.queryByText(/can't play/)).toBeNull();
    await act(async () => { finishS1('file:///run/user/1000/daisy-media/stale.opus'); });
    expect(await screen.findByText(/can't play — vault is locked/)).toBeTruthy();
    expect(loadsOfS2).toBe(2);
  });

  it('a daisy-media URL is never released', async () => {
    mockUrl.mockResolvedValue('daisy-media://localhost/session/s1/meeting');
    const { unmount } = render(<SummaryPane sessionId="s1" onStartSummarize={() => {}} />);
    await screen.findByTestId('session-playback');
    unmount();
    expect(mockRelease).not.toHaveBeenCalled();
  });
});

describe('untrimmedNote', () => {
  it('names the live logs a cut left whole', () => {
    expect(untrimmedNote([])).toBe('');
    expect(untrimmedNote(['live_transcript.jsonl'])).toBe(' The live transcript could not be trimmed and still holds the words after the cut.');
    expect(untrimmedNote(['telemetry.jsonl', 'live_transcript.jsonl'])).toBe(' The live caption timings and live transcript could not be trimmed and still hold the words after the cut.');
  });
});
