// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { render, screen, waitFor } from '@testing-library/react';
import { describe, it, expect, vi, beforeEach } from 'vitest';

// SpeakerLabeler subscribes to library events + the diarize bus, both of
// which call Tauri APIs absent in jsdom. Stubbed; the component mounts
// cleanly.
vi.mock('../lib/libraryEvents', () => ({ subscribeToLibrary: () => () => {} }));
vi.mock('../lib/diarizeBus', () => ({
  runDiarize: vi.fn().mockResolvedValue({ speakers: 0, segments_labeled: 0, segments_seen: 12 }),
  isDiarizing: () => false,
}));
vi.mock('../lib/confirm', () => ({ confirm: vi.fn().mockResolvedValue(true) }));

const listSessionSpeakers = vi.fn();
const listVoiceprints = vi.fn().mockResolvedValue([]);
const diarizeAttendeeCount = vi.fn().mockResolvedValue(null);
vi.mock('../tauri', async (orig) => {
  const mod = await (orig as () => Promise<typeof import('../tauri')>)();
  return {
    ...mod,
    tauri: {
      ...mod.tauri,
      listSessionSpeakers: () => listSessionSpeakers(),
      listVoiceprints: () => listVoiceprints(),
      diarizeAttendeeCount: (...a: unknown[]) => diarizeAttendeeCount(...a),
      diarizeCount: () => Promise.resolve(null),
    },
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

describe('SpeakerLabeler count field', () => {
  it('shows the invitee count the backend would rest the run on', async () => {
    listSessionSpeakers.mockResolvedValue([]);
    diarizeAttendeeCount.mockReset();
    diarizeAttendeeCount.mockResolvedValue(3);
    const attendees = [{ display_name: 'Avery', role: 'other' as const }];
    render(<SpeakerLabeler sessionId="s3" diarizationUnavailable={false} inviteAttendees={attendees} onChanged={() => {}} />);
    const field = await screen.findByTestId('speakers-count');
    await waitFor(() => expect(field).toHaveAttribute('placeholder', '3'));
    expect(diarizeAttendeeCount).toHaveBeenCalledWith('s3', null);
  });

  it('falls back to auto when no invitee count applies', async () => {
    listSessionSpeakers.mockResolvedValue([]);
    diarizeAttendeeCount.mockReset();
    diarizeAttendeeCount.mockResolvedValue(null);
    render(<SpeakerLabeler sessionId="s4" diarizationUnavailable={false} inviteAttendees={[]} onChanged={() => {}} />);
    const field = await screen.findByTestId('speakers-count');
    await waitFor(() => expect(diarizeAttendeeCount).toHaveBeenCalled());
    expect(field).toHaveAttribute('placeholder', 'auto');
  });
});
