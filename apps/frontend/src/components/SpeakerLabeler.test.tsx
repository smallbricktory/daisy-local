import { render, screen, waitFor } from '@testing-library/react';
import { describe, it, expect, vi, beforeEach } from 'vitest';

// SpeakerLabeler subscribes to library events + the diarize bus, both of
// which call Tauri APIs absent in jsdom. Stubbed; the component mounts
// cleanly.
vi.mock('../lib/libraryEvents', () => ({ subscribeToLibrary: () => () => {} }));
vi.mock('../lib/diarizeBus', () => ({
  runDiarize: vi.fn().mockResolvedValue({ speakers: 0, segments_labeled: 0 }),
  isDiarizing: () => false,
}));
vi.mock('../lib/confirm', () => ({ confirm: vi.fn().mockResolvedValue(true) }));

const listSessionSpeakers = vi.fn();
const listVoiceprints = vi.fn().mockResolvedValue([]);
vi.mock('../tauri', async (orig) => {
  const mod = await (orig as () => Promise<typeof import('../tauri')>)();
  return {
    ...mod,
    tauri: { ...mod.tauri, listSessionSpeakers: () => listSessionSpeakers(), listVoiceprints: () => listVoiceprints() },
  };
});

import { SpeakerLabeler } from './SpeakerLabeler';

function speaker(id: number, name: string, side: 'room' | 'remote') {
  return {
    cluster_id: id, display_name: name, email: null, voiceprint_id: null,
    match_confidence: null, is_user_labeled: true, sample_text: null, speech_ms: 5000, side,
  };
}

describe('SpeakerLabeler side grouping', () => {
  beforeEach(() => {
    listSessionSpeakers.mockReset();
  });

  it('shows Your room and Remote sections when both sides present', async () => {
    listSessionSpeakers.mockResolvedValue([
      speaker(0, 'Alice', 'remote'),
      speaker(2, 'Bob', 'room'),
    ]);
    render(<SpeakerLabeler sessionId="s1" diarizationUnavailable={false} inviteAttendees={[]} onChanged={() => {}} />);
    expect(await screen.findByText('Your room')).toBeInTheDocument();
    expect(screen.getByText('Remote')).toBeInTheDocument();
    expect(screen.getByText('Alice')).toBeInTheDocument();
    expect(screen.getByText('Bob')).toBeInTheDocument();
  });

  it('shows no side headings when everyone is remote', async () => {
    listSessionSpeakers.mockResolvedValue([speaker(0, 'Alice', 'remote')]);
    render(<SpeakerLabeler sessionId="s2" diarizationUnavailable={false} inviteAttendees={[]} onChanged={() => {}} />);
    await waitFor(() => expect(screen.getByText('Alice')).toBeInTheDocument());
    expect(screen.queryByText('Your room')).not.toBeInTheDocument();
  });
});
