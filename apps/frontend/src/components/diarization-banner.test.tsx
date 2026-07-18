/**
 * G2: diarization_unavailable banner in SummaryPane transcript tab.
 *
 * Tests:
 *   - banner shown when manifest.diarization_unavailable === true
 *   - banner hidden when manifest.diarization_unavailable === false / missing
 */
import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent, act } from '@testing-library/react';

// ── Mock all backend / Tauri dependencies BEFORE imports ────────────────────

vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn(() => Promise.resolve(() => {})),
}));

vi.mock('@tauri-apps/plugin-dialog', () => ({
  save: vi.fn(),
}));

vi.mock('../components/tags/TagChip', () => ({ TagChip: () => null }));
vi.mock('../components/tags/TagCombobox', () => ({ TagCombobox: () => null }));
vi.mock('../components/tags/TagPromptModal', () => ({ TagPromptModal: () => null }));
vi.mock('../components/ConfirmDialog', () => ({ ConfirmDialog: () => null }));
vi.mock('../components/MarkdownView', () => ({
  MarkdownView: ({ markdown }: { markdown: string }) => <pre>{markdown}</pre>,
}));

// Use a stable reference object — do NOT reference a `let` variable inside vi.mock
// factories as they are hoisted and the variable isn't initialized yet.
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
    listPrompts: vi.fn(() => Promise.resolve([])),
  },
  errStr: vi.fn((e: unknown) => String(e)),
  formatDurationHm: vi.fn(() => '—'),
  LANGUAGES: [],
  summaryProviderStatus: vi.fn(() =>
    Promise.resolve({ state: 'Configured', provider: 'groq', hint: null }),
  ),
  copyToClipboard: vi.fn(),
}));

// ── Import after mocks ───────────────────────────────────────────────────────
import { SummaryPane } from '../routes/SummaryPane';
import { tauri as mockTauri } from '../tauri';

// Minimal session view factory
function makeView(diarizationUnavailable: boolean) {
  return {
    session_id: 'sess-1',
    manifest_json: {
      schema_version: 2,
      created_at_unix_seconds: 1_700_000_000,
      finalized_at_unix_seconds: 1_700_001_000,
      chunks: [{ duration_seconds: 600 }],
      diarization_unavailable: diarizationUnavailable,
    },
    transcript_md: '[00:00:01] **Me**: Hello\n[00:00:02] **Person A**: World',
    has_transcript: true,
    has_dedup: true,
    has_summary: false,
  };
}

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
  (mockTauri.readSession as ReturnType<typeof vi.fn>).mockResolvedValue(makeView(false));
  (mockTauri.sessionMetaGet as ReturnType<typeof vi.fn>).mockResolvedValue(META);
  (mockTauri.summaryLoad as ReturnType<typeof vi.fn>).mockResolvedValue(null);
  (mockTauri.listTags as ReturnType<typeof vi.fn>).mockResolvedValue([]);
  (mockTauri.loadSessionChapters as ReturnType<typeof vi.fn>).mockResolvedValue(null);
  (mockTauri.sessionNotesLoad as ReturnType<typeof vi.fn>).mockResolvedValue('');
  (mockTauri.sessionHasPlaybackAudio as ReturnType<typeof vi.fn>).mockResolvedValue(false);
});

describe('diarization_unavailable banner in SummaryPane', () => {
  it('shows the banner when manifest.diarization_unavailable is true', async () => {
    (mockTauri.readSession as ReturnType<typeof vi.fn>).mockResolvedValue(makeView(true));

    await act(async () => {
      render(
        <SummaryPane
          sessionId="sess-1"
          onStartSummarize={() => {}}
        />,
      );
    });

    // Switch to the participants tab — the diarization-unavailable banner
    // lives there.
    const participantsTab = screen.getByText('participants');
    await act(async () => { fireEvent.click(participantsTab); });

    expect(screen.getByTestId('diarization-unavailable-banner')).toBeInTheDocument();
    expect(screen.getByText(/voice grouping unavailable/i)).toBeInTheDocument();
    expect(screen.getByText(/on-device voice model is missing/i)).toBeInTheDocument();
  });

  it('does NOT show the banner when manifest.diarization_unavailable is false', async () => {
    (mockTauri.readSession as ReturnType<typeof vi.fn>).mockResolvedValue(makeView(false));

    await act(async () => {
      render(
        <SummaryPane
          sessionId="sess-1"
          onStartSummarize={() => {}}
        />,
      );
    });

    const transcriptTab = screen.getByText('transcript');
    await act(async () => { fireEvent.click(transcriptTab); });

    expect(screen.queryByTestId('diarization-unavailable-banner')).not.toBeInTheDocument();
  });
});
