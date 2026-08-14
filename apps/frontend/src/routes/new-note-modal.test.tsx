import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent, waitFor } from '@testing-library/react';

vi.mock('@tauri-apps/plugin-dialog', () => ({ open: vi.fn().mockResolvedValue('/tmp/panel.m4a') }));
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn().mockResolvedValue(() => {}) }));

const mockTauri = vi.hoisted(() => ({
  importAudioMeeting: vi.fn(),
  createNoteSession: vi.fn(),
  listSessions: vi.fn().mockResolvedValue([]),
  listTags: vi.fn().mockResolvedValue([]),
  searchTags: vi.fn().mockResolvedValue([]),
}));
vi.mock('../tauri', async (orig) => ({
  ...(await orig() as object),
  tauri: mockTauri,
}));
// One-click picker standing in for the combobox (its own behavior is covered elsewhere).
vi.mock('../components/tags/TagCombobox', () => ({
  TagCombobox: ({ onPick }: { onPick: (t: { id: string; name: string; color_hex: string }) => void }) => (
    <button onClick={() => onPick({ id: 't-new', name: 'Client A', color_hex: '#aabbcc' })}>pick-tag</button>
  ),
}));

import { Library } from './Library';

beforeEach(() => {
  vi.clearAllMocks();
  mockTauri.listSessions.mockResolvedValue([]);
  mockTauri.importAudioMeeting.mockResolvedValue({
    session_id: 's-new', quality_ok: true, quality_note: '', duration_secs: 60,
  });
});

async function openModal() {
  render(
    <Library
      selectedSessionId=""
      onSelect={() => {}}
      onOpenSearch={() => {}}
      onStartSummarize={() => {}}
      processingSessionIds={[]}
    />,
  );
  fireEvent.click(await screen.findByText('+ Meeting'));
  await screen.findByText('New meeting');
}

describe('NewNoteModal import options', () => {
  it('hides Speakers until an audio file is chosen, then defaults to auto-detect', async () => {
    await openModal();
    expect(screen.queryByText('Speakers')).not.toBeInTheDocument();
    fireEvent.click(screen.getByText('Choose audio…'));
    await screen.findByText('Speakers');
    expect(screen.getByPlaceholderText('Auto-detect')).toHaveValue(null);
  });

  it('passes expected_speakers when set', async () => {
    await openModal();
    fireEvent.click(screen.getByText('Choose audio…'));
    await screen.findByText('Speakers');
    fireEvent.change(screen.getByPlaceholderText('Auto-detect'), { target: { value: '4' } });
    fireEvent.click(screen.getByText('Import & transcribe'));
    await waitFor(() => expect(mockTauri.importAudioMeeting).toHaveBeenCalled());
    expect(mockTauri.importAudioMeeting.mock.calls[0][0].expected_speakers).toBe(4);
  });

  it('passes null expected_speakers when left on auto-detect', async () => {
    await openModal();
    fireEvent.click(screen.getByText('Choose audio…'));
    await screen.findByText('Speakers');
    fireEvent.click(screen.getByText('Import & transcribe'));
    await waitFor(() => expect(mockTauri.importAudioMeeting).toHaveBeenCalled());
    expect(mockTauri.importAudioMeeting.mock.calls[0][0].expected_speakers).toBeNull();
  });

  it('creating a tag via the combobox selects it and sends it with the import', async () => {
    await openModal();
    fireEvent.click(screen.getByText('pick-tag'));
    expect(await screen.findByText('Client A')).toBeInTheDocument();
    fireEvent.click(screen.getByText('Choose audio…'));
    await screen.findByText('Speakers');
    fireEvent.click(screen.getByText('Import & transcribe'));
    await waitFor(() => expect(mockTauri.importAudioMeeting).toHaveBeenCalled());
    expect(mockTauri.importAudioMeeting.mock.calls[0][0].tag_ids).toEqual(['t-new']);
  });
});
