import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, fireEvent, act } from '@testing-library/react';

// --- Mock all heavy/backend dependencies before importing the component ---

vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn(() => Promise.resolve(() => {})),
}));

vi.mock('@tauri-apps/plugin-dialog', () => ({
  save: vi.fn(),
}));

vi.mock('../components/tags/TagChip', () => ({
  TagChip: () => null,
}));

vi.mock('../components/tags/TagCombobox', () => ({
  TagCombobox: () => null,
}));

vi.mock('../components/tags/TagPromptModal', () => ({
  TagPromptModal: () => null,
}));

vi.mock('../components/ConfirmDialog', () => ({
  ConfirmDialog: () => null,
}));

vi.mock('../components/MarkdownView', () => ({
  MarkdownView: ({ markdown }: { markdown: string }) => <pre>{markdown}</pre>,
}));

// copyToClipboard is what copyWithPrompt calls. We'll make it controllable per test.
const mockCopyToClipboard = vi.fn();

vi.mock('../tauri', () => ({
  tauri: {},
  errStr: vi.fn((e: unknown) => String(e)),
  formatDurationHm: vi.fn(() => '—'),
  LANGUAGES: [],
  summaryProviderStatus: vi.fn(() => Promise.resolve(null)),
  copyToClipboard: (...args: unknown[]) => mockCopyToClipboard(...args),
}));

// Import the component AFTER mocks are registered. The "Copy all" affordance
// lives in CopyButton (hoisted onto the detail-tabs bar); the transcript just
// hands it the markdown. This suite exercises CopyButton's state machine.
import { CopyButton } from '../routes/SummaryPane';

beforeEach(() => {
  vi.clearAllMocks();
  vi.useFakeTimers();
});

afterEach(() => {
  vi.useRealTimers();
});

const SMALL_MD = '[00:00:01] **Me**: Hello world';

describe('<CopyButton /> copy-all state machine', () => {
  it('renders "Copy all" in the idle state', () => {
    render(<CopyButton kind="transcript" text={SMALL_MD} label="Copy all" />);
    expect(screen.getByRole('button', { name: 'Copy all' })).toBeInTheDocument();
  });

  it('shows "Copied ✓" after a successful small copy and reverts to "Copy all" after 2500 ms', async () => {
    mockCopyToClipboard.mockResolvedValue(undefined);

    render(<CopyButton kind="transcript" text={SMALL_MD} label="Copy all" />);
    const btn = screen.getByRole('button', { name: 'Copy all' });

    // Click fires the async handler.
    await act(async () => {
      fireEvent.click(btn);
      // Let the microtask queue (the resolved promise) flush.
      await Promise.resolve();
    });

    expect(screen.getByRole('button', { name: 'Copied ✓' })).toBeInTheDocument();

    // Advance time past the 2500 ms reset timeout.
    await act(async () => {
      vi.advanceTimersByTime(2500);
    });

    expect(screen.getByRole('button', { name: 'Copy all' })).toBeInTheDocument();
  });

  it('shows truncation label for content over 5 MB and reverts after 2500 ms', async () => {
    mockCopyToClipboard.mockResolvedValue(undefined);

    // Build a string just over 5 MB (5 * 1024 * 1024 + 1 chars).
    const LARGE_MD = 'x'.repeat(5 * 1024 * 1024 + 1);
    render(<CopyButton kind="transcript" text={LARGE_MD} label="Copy all" />);

    const btn = screen.getByRole('button', { name: 'Copy all' });

    await act(async () => {
      fireEvent.click(btn);
      await Promise.resolve();
    });

    expect(screen.getByRole('button', { name: /may truncate/i })).toBeInTheDocument();

    await act(async () => {
      vi.advanceTimersByTime(2500);
    });

    expect(screen.getByRole('button', { name: 'Copy all' })).toBeInTheDocument();
  });

  it('shows "Copy failed" when clipboard throws and reverts after 4000 ms', async () => {
    mockCopyToClipboard.mockRejectedValue(new Error('clipboard denied'));

    render(<CopyButton kind="transcript" text={SMALL_MD} label="Copy all" />);
    const btn = screen.getByRole('button', { name: 'Copy all' });

    await act(async () => {
      fireEvent.click(btn);
      // Let the rejection propagate through the catch.
      await Promise.resolve();
      await Promise.resolve();
    });

    expect(screen.getByRole('button', { name: 'Copy failed' })).toBeInTheDocument();

    await act(async () => {
      vi.advanceTimersByTime(4000);
    });

    expect(screen.getByRole('button', { name: 'Copy all' })).toBeInTheDocument();
  });
});
