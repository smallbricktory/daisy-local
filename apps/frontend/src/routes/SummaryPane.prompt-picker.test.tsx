/**
 * Regen Summary style flyout: Action menu > Regen. Summary lists every
 * summary style; picking one regenerates with that style's prompt id.
 */
import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent, act } from '@testing-library/react';

vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn(() => Promise.resolve(() => {})),
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
    listPrompts: vi.fn(() => Promise.resolve([
      { id: 'builtin:daisy', name: 'Daisy Summarizer', output: 'classic', directive_md: '', builtin: true },
      { id: 'builtin:zoom', name: 'Zoom-style Meeting Notes', output: 'sectioned', directive_md: 'x', builtin: true },
    ])),
  },
  errStr: vi.fn((e: unknown) => String(e)),
  formatDurationHm: vi.fn(() => '—'),
  LANGUAGES: [],
  summaryProviderStatus: vi.fn(() =>
    Promise.resolve({ state: 'Configured', provider: 'groq', hint: null }),
  ),
  copyToClipboard: vi.fn(),
}));

import { SummaryPane } from './SummaryPane';
import { tauri as mockTauri } from '../tauri';

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
  has_summary: true,
};

const META = {
  session_id: 'sess-1',
  title: 'Test meeting',
  tag_ids: [],
  attendees: [],
  has_notes: false,
  has_summary: true,
  recording_segments: 1,
};

const SUMMARY = {
  schema_version: 1,
  session_id: 'sess-1',
  provider: 'groq',
  model: 'm',
  generated_at_unix_seconds: 1_700_001_000,
  source_inputs_hash: 'h',
  structured: { tldr: 'Fine.', action_items: [], decisions: [], open_questions: [], key_topics: [] },
  markdown: '**TL;DR.** Fine.',
  user_edited: false,
};

beforeEach(() => {
  vi.clearAllMocks();
  (mockTauri.readSession as ReturnType<typeof vi.fn>).mockResolvedValue(VIEW);
  (mockTauri.sessionMetaGet as ReturnType<typeof vi.fn>).mockResolvedValue(META);
  (mockTauri.summaryLoad as ReturnType<typeof vi.fn>).mockResolvedValue(SUMMARY);
  (mockTauri.listTags as ReturnType<typeof vi.fn>).mockResolvedValue([]);
  (mockTauri.loadSessionChapters as ReturnType<typeof vi.fn>).mockResolvedValue(null);
  (mockTauri.sessionNotesLoad as ReturnType<typeof vi.fn>).mockResolvedValue('');
  (mockTauri.sessionHasPlaybackAudio as ReturnType<typeof vi.fn>).mockResolvedValue(false);
  (mockTauri.listIntegrations as ReturnType<typeof vi.fn>).mockResolvedValue([]);
  (mockTauri.listSessionSpeakers as ReturnType<typeof vi.fn>).mockResolvedValue([]);
  (mockTauri.sessionPlaybackAudioBytes as ReturnType<typeof vi.fn>).mockResolvedValue(new ArrayBuffer(0));
  (mockTauri.listPrompts as ReturnType<typeof vi.fn>).mockResolvedValue([
    { id: 'builtin:daisy', name: 'Daisy Summarizer', output: 'classic', directive_md: '', builtin: true },
    { id: 'builtin:zoom', name: 'Zoom-style Meeting Notes', output: 'sectioned', directive_md: 'x', builtin: true },
  ]);
});

async function openActionMenu() {
  await act(async () => {
    fireEvent.click(screen.getByRole('button', { name: /action/i }));
  });
}

describe('SummaryPane regen-summary style flyout', () => {
  it('lists every summary style in the Regen. Summary flyout', async () => {
    await act(async () => {
      render(<SummaryPane sessionId="sess-1" onStartSummarize={() => {}} />);
    });
    await openActionMenu();
    await act(async () => {
      fireEvent.mouseEnter(screen.getByRole('menuitem', { name: /regen\. summary/i }).parentElement!);
    });
    expect(screen.getByRole('menuitem', { name: 'Daisy Summarizer' })).toBeInTheDocument();
    expect(screen.getByRole('menuitem', { name: 'Zoom-style Meeting Notes' })).toBeInTheDocument();
  });

  it('picking a style regenerates with that prompt id', async () => {
    const onStartSummarize = vi.fn();
    await act(async () => {
      render(<SummaryPane sessionId="sess-1" onStartSummarize={onStartSummarize} />);
    });
    await openActionMenu();
    await act(async () => {
      fireEvent.mouseEnter(screen.getByRole('menuitem', { name: /regen\. summary/i }).parentElement!);
    });
    await act(async () => {
      fireEvent.click(screen.getByRole('menuitem', { name: 'Zoom-style Meeting Notes' }));
    });
    expect(onStartSummarize).toHaveBeenCalledWith('sess-1', 'Test meeting', 'regen-summary', 'builtin:zoom');
  });

  it('no title-row style dropdown; flyout disabled without a summary', async () => {
    (mockTauri.summaryLoad as ReturnType<typeof vi.fn>).mockResolvedValue(null);
    await act(async () => {
      render(<SummaryPane sessionId="sess-1" onStartSummarize={() => {}} />);
    });
    expect(screen.queryByTitle(/style used when you regenerate/i)).not.toBeInTheDocument();
    await openActionMenu();
    expect(screen.getByRole('menuitem', { name: /regen\. summary/i })).toBeDisabled();
  });
});
