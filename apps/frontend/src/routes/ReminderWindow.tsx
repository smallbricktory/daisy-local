// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { useEffect, useState } from 'react';
import './../reminder-window.css';
import { listen } from '@tauri-apps/api/event';
import { tauri } from '../tauri';
import type { ReminderChoice, ReminderPayload } from '../tauri';

/**
 * The reminder popup: a small frameless always-on-top webview (label
 * "reminder"), pre-rendered hidden at startup and shown by the backend with
 * a calendar payload (a meeting about to start), a meeting payload (another
 * app has opened the microphone), or a meeting-ended payload (that app has
 * released it during a recording). Every button sends its choice to the
 * backend.
 */
export function ReminderWindow() {
  const [payload, setPayload] = useState<ReminderPayload | null>(null);

  useEffect(() => {
    // Reads the payload from the backend on mount and on every reminder:show.
    const refresh = () => tauri.reminderPayload().then(setPayload).catch(() => {});
    void refresh();
    let unlisten: (() => void) | undefined;
    let cancelled = false;
    listen('reminder:show', () => { void refresh(); })
      .then((fn) => { if (cancelled) fn(); else unlisten = fn; })
      .catch(() => {});
    return () => { cancelled = true; unlisten?.(); };
  }, []);

  const act = (choice: ReminderChoice) => () => { tauri.reminderAction(choice).catch(() => {}); };

  if (payload === null) return null;
  if (payload.kind === 'meeting') return <MeetingFace act={act} />;
  if (payload.kind === 'meeting_ended') return <MeetingEndedFace app={payload.app} act={act} />;
  return <CalendarFace title={payload.kind === 'calendar' ? payload.title : ''} act={act} />;
}

function CalendarFace({ title, act }: {
  title: string;
  act: (choice: ReminderChoice) => () => void;
}) {
  return (
    <div className="reminder">
      <div className="reminder__bar" data-tauri-drag-region>
        <span className="reminder__dot" aria-hidden="true" />
        <span className="reminder__eyebrow">Meeting starting</span>
      </div>
      <p className="reminder__title" title={title} data-tauri-drag-region>
        {title || 'A meeting'}
      </p>
      <div className="reminder__actions">
        <button data-testid="reminder-open" className="reminder__btn reminder__btn--primary" onClick={act('open')}>Open</button>
        <button data-testid="reminder-dismiss" className="reminder__btn" onClick={act('dismiss')}>Dismiss</button>
      </div>
    </div>
  );
}

function MeetingEndedFace({ app, act }: {
  app: string;
  act: (choice: ReminderChoice) => () => void;
}) {
  return (
    <div className="reminder">
      <div className="reminder__bar" data-tauri-drag-region>
        <span className="reminder__dot" aria-hidden="true" />
        <span className="reminder__eyebrow" title={`${app} left the call`}>{app} left the call</span>
      </div>
      <p className="reminder__body" data-tauri-drag-region>
        Stop recording?
      </p>
      <div className="reminder__actions">
        <button data-testid="reminder-stop" className="reminder__btn reminder__btn--primary" onClick={act('stop')}>Stop</button>
        <button data-testid="reminder-keep" className="reminder__btn" onClick={act('dismiss')}>Keep recording</button>
      </div>
    </div>
  );
}

function MeetingFace({ act }: { act: (choice: ReminderChoice) => () => void }) {
  return (
    <div className="reminder">
      <div className="reminder__bar" data-tauri-drag-region>
        <span className="reminder__dot" aria-hidden="true" />
        <span className="reminder__eyebrow">It seems a meeting has started.</span>
      </div>
      <p className="reminder__body" data-tauri-drag-region>
        Start recording?
      </p>
      <div className="reminder__actions reminder__actions--stack">
        <div className="reminder__row">
          <button data-testid="reminder-start" className="reminder__btn reminder__btn--primary" onClick={act('start')}>Start recording</button>
          <button data-testid="reminder-not-now" className="reminder__btn" onClick={act('dismiss')}>Not now</button>
          <button data-testid="reminder-never" className="reminder__btn" onClick={act('never')}>Never ask</button>
        </div>
        <div className="reminder__snooze" role="group" aria-labelledby="reminder-snooze-label">
          <p id="reminder-snooze-label" className="reminder__label">Don&apos;t remind me for:</p>
          <div className="reminder__row">
            <button data-testid="reminder-snooze-30m" className="reminder__btn" onClick={act('snooze_30m')}>30 min</button>
            <button data-testid="reminder-snooze-1h" className="reminder__btn" onClick={act('snooze_1h')}>1 hour</button>
            <button data-testid="reminder-snooze-today" className="reminder__btn" onClick={act('snooze_today')}>Today</button>
          </div>
        </div>
      </div>
    </div>
  );
}
