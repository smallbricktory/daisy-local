import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent, act, waitFor } from '@testing-library/react';

vi.mock('../tauri', () => ({
  tauri: {
    listTags: vi.fn().mockResolvedValue([]),
    listContacts: vi.fn().mockResolvedValue([
      { id: 'c1', display_name: 'Mira', emails: ['mira@x.com'], created_at_unix_seconds: 1 },
      { id: 'c2', display_name: 'Dana', emails: [], created_at_unix_seconds: 1 },
    ]),
    searchSessions: vi.fn().mockResolvedValue([]),
  },
  errStr: vi.fn((e: unknown) => String(e)),
}));
vi.mock('../lib/aiProviderStatus', () => ({
  useAiProviderStatus: () => ({ configured: true, state: 'Configured', provider: 'groq', hint: null }),
}));
vi.mock('../lib/qaStore', () => ({
  useQa: () => ({ status: 'idle' }),
  askQuestion: vi.fn(),
  cancelQuestion: vi.fn(),
  resetQa: vi.fn(),
}));
vi.mock('../components/AiProviderRequiredModal', () => ({ AiProviderRequiredModal: () => null }));
vi.mock('../components/tags/TagChip', () => ({ TagChip: () => null }));

import { Search } from './Search';
import { tauri } from '../tauri';

beforeEach(() => {
  vi.clearAllMocks();
});

describe('Search — people filter', () => {
  it('renders a chip per contact and filters by contact_ids when toggled (AND across chips)', async () => {
    await act(async () => {
      render(<Search onOpenSession={() => {}} onNavigateToProviders={() => {}} />);
    });
    // Filters live behind the expander; open it if collapsed.
    const expander = screen.queryByText(/filters/i);
    if (expander) {
      await act(async () => { fireEvent.click(expander); });
    }
    expect(screen.getByText(/○ Mira/)).toBeInTheDocument();
    expect(screen.getByText(/○ Dana/)).toBeInTheDocument();

    await act(async () => {
      fireEvent.click(screen.getByText(/○ Mira/));
    });
    // Debounced (200ms) search fires with the selected contact.
    await waitFor(() => {
      const calls = (tauri.searchSessions as ReturnType<typeof vi.fn>).mock.calls;
      expect(calls.some(([req]) => JSON.stringify(req.contact_ids) === JSON.stringify(['c1']))).toBe(true);
    });

    // Second chip → both ids sent (AND semantics live on the backend).
    await act(async () => {
      fireEvent.click(screen.getByText(/○ Dana/));
    });
    await waitFor(() => {
      const calls = (tauri.searchSessions as ReturnType<typeof vi.fn>).mock.calls;
      expect(calls.some(([req]) => (req.contact_ids ?? []).slice().sort().join(',') === 'c1,c2')).toBe(true);
    });

    // Toggle off → back to a single id.
    await act(async () => {
      fireEvent.click(screen.getByText(/✕ Mira|Mira/));
    });
    await waitFor(() => {
      const calls = (tauri.searchSessions as ReturnType<typeof vi.fn>).mock.calls;
      expect(calls.some(([req]) => JSON.stringify(req.contact_ids) === JSON.stringify(['c2']))).toBe(true);
    });
  });

  it('shows the empty hint when there are no contacts', async () => {
    (tauri.listContacts as ReturnType<typeof vi.fn>).mockResolvedValue([]);
    await act(async () => {
      render(<Search onOpenSession={() => {}} onNavigateToProviders={() => {}} />);
    });
    const expander = screen.queryByText(/filters/i);
    if (expander) {
      await act(async () => { fireEvent.click(expander); });
    }
    expect(screen.getByText(/no people yet/i)).toBeInTheDocument();
  });
});
