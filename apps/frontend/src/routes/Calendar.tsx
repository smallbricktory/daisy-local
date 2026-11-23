// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
// Calendar route — lists upcoming events from every enabled ICS
// subscription. Click an event to start a new recording pre-filled
// with its title, attendees, and auto-tag. Each event is dismissable
// (the dismissal is persisted on the owning subscription; the same
// event does not come back on the next sync). If a subscription is
// deleted in Settings → Calendars, its events are evicted from the
// cache immediately; existing recordings that linked to those events
// keep their `manifest.calendar` back-reference intact.

import { useCallback, useEffect, useState } from 'react';
import { fmtTime, fmtWeekdayDateTime } from '../lib/format';
import { tauri, errStr, type CalendarEvent, type EventSeed } from '../tauri';
import { PageHeader } from '../components/PageHeader';
import { EmptyState } from '../components/EmptyState';
import { Select } from '../components/Select';

interface Props {
  /** Called when the user clicks an event. The parent navigates to the
   *  Recording route and passes the seed to ActiveSession. */
  onStartRecording: (seed: EventSeed) => void;
}


function formatWhen(unixSec: number): string {
  const now = Date.now() / 1000;
  const delta = unixSec - now;
  const d = new Date(unixSec * 1000);
  if (delta < 0 && delta > -3600) return 'now';
  if (delta >= 0 && delta < 3600) return `in ${Math.max(1, Math.round(delta / 60))}m`;
  const today = new Date();
  today.setHours(0, 0, 0, 0);
  const sameDay = d.getDate() === today.getDate()
                && d.getMonth() === today.getMonth()
                && d.getFullYear() === today.getFullYear();
  if (sameDay) return fmtTime(unixSec);
  return fmtWeekdayDateTime(unixSec);
}

export function eventToSeed(e: CalendarEvent): EventSeed {
  return {
    title: e.title,
    tag_id: e.subscription_tag_id,
    calendar_link: {
      provider: e.subscription_id,
      event_id: e.uid,
      recurrence_id: null,
      planned_start_unix_seconds: e.start_unix_seconds,
      planned_end_unix_seconds: e.end_unix_seconds,
    },
    attendees: e.attendees,
  };
}

export function Calendar({ onStartRecording }: Props) {
  const [events, setEvents] = useState<CalendarEvent[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [refreshing, setRefreshing] = useState(false);
  const [windowDays, setWindowDays] = useState(7);
  // Ticks every 60s; the "happening now" highlight appears/clears without a
  // manual refresh.
  const [nowSec, setNowSec] = useState(() => Date.now() / 1000);
  useEffect(() => {
    const id = setInterval(() => setNowSec(Date.now() / 1000), 60_000);
    return () => clearInterval(id);
  }, []);

  const reload = useCallback(() => {
    setError(null);
    tauri.listUpcomingEvents(windowDays).then(setEvents).catch((e) => {
      setError(errStr(e));
      setEvents([]);
    });
  }, [windowDays]);

  useEffect(() => { reload(); }, [reload]);

  // Trims to the start of the local day: the full agenda for today shows
  // (including meetings that already ended), nothing from yesterday.
  const todayStart = new Date();
  todayStart.setHours(0, 0, 0, 0);
  const todayStartSec = todayStart.getTime() / 1000;
  const shown = events?.filter((e) => e.start_unix_seconds >= todayStartSec) ?? null;

  const [note, setNote] = useState<string | null>(null);

  async function refresh() {
    setRefreshing(true); setError(null); setNote(null);
    try {
      const r = await tauri.refreshCalendars();
      const errPart = r.errors.length > 0 ? ` · ${r.errors.length} issue${r.errors.length === 1 ? '' : 's'}` : '';
      setNote(`Synced ${r.subscriptions_scanned} calendar${r.subscriptions_scanned === 1 ? '' : 's'} · ${r.events_loaded} event${r.events_loaded === 1 ? '' : 's'}${errPart}.`);
      if (r.errors.length > 0) setError(r.errors.join('\n'));
      reload();
    } catch (e) {
      setError(errStr(e));
    } finally {
      setRefreshing(false);
    }
  }

  async function dismiss(e: CalendarEvent) {
    try {
      await tauri.dismissCalendarEvent(e.subscription_id, e.uid);
      reload();
    } catch (err) {
      setError(errStr(err));
    }
  }

  return (
    <div className="page">
      <PageHeader
        title="Calendar"
        actions={
          <>
          <span className="meta field-label--small header-field" title="How far ahead to list events">
            <label htmlFor="calendar-window">Window</label>
            <Select
              id="calendar-window"
              testId="calendar-window"
              value={String(windowDays)}
              onChange={(v) => setWindowDays(Number(v))}
              options={[
                { value: '1', label: 'Today' },
                { value: '3', label: '3 days' },
                { value: '7', label: '7 days' },
                { value: '14', label: '14 days' },
                { value: '30', label: '30 days' },
              ]}
            />
          </span>
          <button data-testid="calendar-sync" onClick={() => void refresh()} disabled={refreshing} className="btn">
            {refreshing ? 'Syncing…' : '↻ Sync'}
          </button>
          </>
        }
      />

      <div className="page-notices">
        {note && (
          <p data-testid="calendar-note" className="meta meta--success page-notice">{note}</p>
        )}
        {error && (
          <p className="meta--danger page-notice page-notice--pre">{error}</p>
        )}
      </div>

      <div className="page__body page__measure">
        {shown == null && <p className="meta">Loading…</p>}
        {shown && shown.length === 0 && (
          <EmptyState
            testid="calendar-empty"
            title={`No events in the next ${windowDays} day${windowDays === 1 ? '' : 's'}`}
            action={(
              <button data-testid="calendar-empty-sync" className="btn" disabled={refreshing} onClick={() => void refresh()}>
                {refreshing ? 'Syncing…' : '↻ Sync calendars'}
              </button>
            )}
          >
            Click a meeting here to start a recording already filled in with its title,
            attendees and tag. Widen the window above to look further ahead.
          </EmptyState>
        )}
        {shown && shown.map((e) => {
          const live = nowSec >= e.start_unix_seconds && nowSec < e.end_unix_seconds;
          return (
          <button
            key={e.id}
            data-testid={`calendar-event-${e.id}`}
            className={live ? 'event-card event-card--live' : 'event-card'}
            onClick={() => onStartRecording(eventToSeed(e))}
            title="Click to start a recording pre-filled with this event's title, attendees, and auto-tag."
          >
            <span aria-hidden="true" className="event-card__stripe" />
            <code className="event-card__when">{formatWhen(e.start_unix_seconds)}</code>
            <span className="event-card__main">
              <div className="event-card__title">
                {live && <span className="event-card__now">● Now</span>}
                <span className="event-card__name">{e.title}</span>
              </div>
              <div className="meta event-card__from">
                {e.subscription_name}
                {e.attendees.length > 0 && ` · ${e.attendees.length} attendee${e.attendees.length === 1 ? '' : 's'}`}
                {e.location && ` · ${e.location}`}
              </div>
            </span>
            <span
              data-testid={`calendar-dismiss-${e.id}`}
              onClick={(ev) => { ev.stopPropagation(); void dismiss(e); }}
              role="button"
              aria-label="Dismiss event"
              title="Dismiss — hide from this list (persists across syncs)"
              className="event-card__dismiss"
            >
              ×
            </span>
          </button>
          );
        })}
      </div>
    </div>
  );
}
