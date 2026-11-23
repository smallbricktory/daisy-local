// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent, waitFor } from '@testing-library/react';
import { ReminderWindow } from './ReminderWindow';
import type { ReminderChoice, ReminderPayload } from '../tauri';

let payload: ReminderPayload | null = null;
const reminderAction = vi.fn<(c: ReminderChoice) => Promise<void>>(() => Promise.resolve());
vi.mock('../tauri', () => ({
  tauri: {
    reminderPayload: () => Promise.resolve(payload),
    reminderAction: (c: ReminderChoice) => reminderAction(c),
  },
}));
vi.mock('@tauri-apps/api/event', () => ({ listen: () => Promise.resolve(() => {}) }));

describe('ReminderWindow', () => {
  beforeEach(() => {
    payload = null;
    reminderAction.mockClear();
  });

  it('renders the calendar face with the meeting title', async () => {
    payload = { kind: 'calendar', title: 'Design review with Jordan' };
    render(<ReminderWindow />);
    expect(await screen.findByText('Design review with Jordan')).toBeTruthy();
    expect(screen.getByTestId('reminder-open')).toBeTruthy();
    expect(screen.queryByTestId('reminder-start')).toBeNull();
  });

  it('open and dismiss send their own choices', async () => {
    payload = { kind: 'calendar', title: 'Standup' };
    render(<ReminderWindow />);
    fireEvent.click(await screen.findByTestId('reminder-open'));
    await waitFor(() => expect(reminderAction).toHaveBeenCalledWith('open'));
    fireEvent.click(screen.getByTestId('reminder-dismiss'));
    await waitFor(() => expect(reminderAction).toHaveBeenCalledWith('dismiss'));
  });

  it('renders the meeting face with its five snooze choices', async () => {
    payload = { kind: 'meeting' };
    render(<ReminderWindow />);
    expect(await screen.findByText('It seems a meeting has started.')).toBeTruthy();
    expect(screen.getByText('Start recording?')).toBeTruthy();
    expect(screen.getByTestId('reminder-start').textContent).toBe('Start recording');
    for (const id of [
      'reminder-not-now', 'reminder-snooze-30m', 'reminder-snooze-1h',
      'reminder-snooze-today', 'reminder-never',
    ]) {
      expect(screen.getByTestId(id)).toBeTruthy();
    }
    expect(screen.queryByTestId('reminder-open')).toBeNull();
  });

  it('renders the meeting-ended face naming the app', async () => {
    payload = { kind: 'meeting_ended', app: 'Fictional Call' };
    render(<ReminderWindow />);
    expect(await screen.findByText('Fictional Call left the call')).toBeTruthy();
    expect(screen.getByText('Stop recording?')).toBeTruthy();
    expect(screen.getByTestId('reminder-stop').textContent).toBe('Stop');
    expect(screen.getByTestId('reminder-keep').textContent).toBe('Keep recording');
    expect(screen.queryByTestId('reminder-start')).toBeNull();
    expect(screen.queryByTestId('reminder-never')).toBeNull();
    expect(screen.queryByRole('group', { name: "Don't remind me for:" })).toBeNull();
  });

  it('the meeting-ended buttons send stop and dismiss', async () => {
    payload = { kind: 'meeting_ended', app: 'Fictional Call' };
    render(<ReminderWindow />);
    fireEvent.click(await screen.findByTestId('reminder-stop'));
    await waitFor(() => expect(reminderAction).toHaveBeenCalledWith('stop'));
    fireEvent.click(screen.getByTestId('reminder-keep'));
    await waitFor(() => expect(reminderAction).toHaveBeenCalledWith('dismiss'));
  });

  it('labels the snooze durations and only them', async () => {
    payload = { kind: 'meeting' };
    render(<ReminderWindow />);
    const group = await screen.findByRole('group', { name: "Don't remind me for:" });
    const inGroup = [...group.querySelectorAll('button')].map((b) => b.dataset.testid);
    expect(inGroup).toEqual(['reminder-snooze-30m', 'reminder-snooze-1h', 'reminder-snooze-today']);
  });

  it('every meeting button sends its choice', async () => {
    payload = { kind: 'meeting' };
    render(<ReminderWindow />);
    const pairs: [string, ReminderChoice][] = [
      ['reminder-start', 'start'],
      ['reminder-not-now', 'dismiss'],
      ['reminder-snooze-30m', 'snooze_30m'],
      ['reminder-snooze-1h', 'snooze_1h'],
      ['reminder-snooze-today', 'snooze_today'],
      ['reminder-never', 'never'],
    ];
    for (const [id, choice] of pairs) {
      fireEvent.click(await screen.findByTestId(id));
      await waitFor(() => expect(reminderAction).toHaveBeenCalledWith(choice));
    }
  });
});
