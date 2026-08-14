import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen } from '@testing-library/react';

vi.mock('../tauri', () => ({
  tauri: { listSessions: vi.fn(), listTags: vi.fn() },
  errStr: vi.fn((e: unknown) => String(e)),
  formatDurationSeconds: (s: number) => `${s}s`,
}));
vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn().mockResolvedValue(() => {}),
}));
vi.mock('@tauri-apps/plugin-dialog', () => ({ open: vi.fn() }));
// The detail pane is irrelevant here — the test only cares which session id
// the library hands it.
vi.mock('./SummaryPane', () => ({
  SummaryPane: ({ sessionId }: { sessionId?: string }) => (
    <div data-testid="detail">{sessionId ?? '(none)'}</div>
  ),
}));

import { tauri } from '../tauri';
import { Library } from './Library';

const mock = tauri as unknown as Record<string, ReturnType<typeof vi.fn>>;

const now = 1_786_600_000;
const sessions = [
  { session_id: 's-live', created_at_unix_seconds: now, finalized_at_unix_seconds: null, duration_seconds: 60, title: 'In progress', tag_ids: [], has_transcript: false, has_dedup: false, has_summary: false },
  { session_id: 's-old', created_at_unix_seconds: now - 3600, finalized_at_unix_seconds: now - 3000, duration_seconds: 900, title: 'Yesterday standup', tag_ids: [], has_transcript: true, has_dedup: true, has_summary: true },
];

beforeEach(() => {
  vi.clearAllMocks();
  mock.listSessions.mockResolvedValue(sessions);
  mock.listTags.mockResolvedValue([]);
});

const props = {
  onSelect: () => {},
  onOpenSearch: () => {},
  onStartSummarize: () => {},
  processingSessionIds: [] as string[],
  onNavigateToProviders: () => {},
};

describe('Library auto-selection vs the active recording', () => {
  it('skips the session being recorded when nothing is selected', async () => {
    render(<Library {...props} recordingSessionId="s-live" />);
    expect(await screen.findByTestId('detail')).toHaveTextContent('s-old');
  });

  it('falls back to the newest session when nothing is recording', async () => {
    render(<Library {...props} />);
    expect(await screen.findByTestId('detail')).toHaveTextContent('s-live');
  });
});
