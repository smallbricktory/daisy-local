// Calendar route — lists upcoming events from every enabled ICS
// subscription. Click an event to start a new recording pre-filled
// with its title, attendees, and auto-tag. Each event is dismissable
// (the dismissal is persisted on the owning subscription; the same
// event does not come back on the next sync). If a subscription is
// deleted in Settings → Calendars, its events are evicted from the
// cache immediately; existing recordings that linked to those events
// keep their `manifest.calendar` back-reference intact.

import { useCallback, useEffect, useState } from 'react';
import { tauri, errStr, type CalendarEvent, type EventSeed } from '../tauri';

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
  if (sameDay) return TIME_FMT.format(d);
  return DATE_FMT.format(d);
}

// Cached Intl formatters — toLocaleTimeString / toLocaleString rebuild
// the formatter on every call (huge CPU cost in big calendar lists).
const TIME_FMT = new Intl.DateTimeFormat(undefined, { hour: 'numeric', minute: '2-digit' });
const DATE_FMT = new Intl.DateTimeFormat(undefined, {
  weekday: 'short', month: 'short', day: 'numeric',
  hour: 'numeric', minute: '2-digit',
});

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
    <div style={{ display: 'flex', flexDirection: 'column', height: '100%' }}>
      <div style={{ display: 'flex', alignItems: 'center', padding: '8px 14px', borderBottom: '1px solid var(--frost-deep)', gap: 12 }}>
        <h1 className="h1" style={{ margin: 0 }}>Calendar</h1>
        <div style={{ marginLeft: 'auto', display: 'flex', gap: 10, alignItems: 'center' }}>
          <label style={{ fontSize: 12, color: 'var(--iron)' }}>
            Window:
            <select
              value={windowDays}
              onChange={(e) => setWindowDays(Number(e.target.value))}
              style={{ marginLeft: 6 }}
            >
              <option value={1}>Today</option>
              <option value={3}>3 days</option>
              <option value={7}>7 days</option>
              <option value={14}>14 days</option>
              <option value={30}>30 days</option>
            </select>
          </label>
          <button onClick={() => void refresh()} disabled={refreshing} className="btn">
            {refreshing ? 'Syncing…' : '↻ Sync'}
          </button>
        </div>
      </div>

      {note && (
        <p className="meta" style={{ padding: '8px 14px', color: 'var(--ok)', borderBottom: '1px solid var(--frost-deep)' }}>{note}</p>
      )}
      {error && (
        <p style={{ padding: 12, color: 'var(--danger)', whiteSpace: 'pre-wrap', borderBottom: '1px solid var(--frost-deep)' }}>{error}</p>
      )}

      <div style={{ flex: 1, minHeight: 0, overflowY: 'auto', padding: '8px 0' }}>
        {shown == null && <p style={{ padding: 16, color: 'var(--iron)' }}>Loading…</p>}
        {shown && shown.length === 0 && (
          <p style={{ padding: 16, color: 'var(--iron)' }}>
            No upcoming events in the next {windowDays} day{windowDays === 1 ? '' : 's'}.
            Try a wider window, or click <em>↻ Sync</em> to fetch the latest from your calendars.
          </p>
        )}
        {shown && shown.map((e) => {
          const live = nowSec >= e.start_unix_seconds && nowSec < e.end_unix_seconds;
          const liveBg = 'var(--amber, #FFF3CD)';
          return (
          <button
            key={e.id}
            className="event-card"
            onClick={() => onStartRecording(eventToSeed(e))}
            style={{
              display: 'grid',
              gridTemplateColumns: '6px 110px 1fr auto',
              gap: 12,
              alignItems: 'center',
              width: '100%',
              padding: '10px 14px',
              border: 'none',
              borderBottom: '1px solid var(--frost-soft)',
              background: live ? liveBg : 'transparent',
              textAlign: 'left',
              cursor: 'pointer',
              fontFamily: 'inherit',
              fontSize: 14,
            }}
            onMouseEnter={(ev) => { ev.currentTarget.style.background = 'var(--tint)'; }}
            onMouseLeave={(ev) => { ev.currentTarget.style.background = live ? liveBg : 'transparent'; }}
            title="Click to start a recording pre-filled with this event's title, attendees, and auto-tag."
          >
            <span
              aria-hidden="true"
              style={{
                width: 6, alignSelf: 'stretch', borderRadius: 3,
                background: e.subscription_color || 'var(--indigo)',
              }}
            />
            <code style={{ fontSize: 11, color: 'var(--iron)' }}>{formatWhen(e.start_unix_seconds)}</code>
            <span style={{ minWidth: 0, overflow: 'hidden' }}>
              <div style={{ fontWeight: 600, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap', display: 'flex', alignItems: 'center', gap: 8 }}>
                {live && (
                  <span style={{
                    flex: '0 0 auto', fontSize: 10, fontWeight: 700, letterSpacing: '0.06em',
                    color: 'var(--cream-pure)', background: 'var(--danger, #c0392b)',
                    borderRadius: 99, padding: '1px 7px', textTransform: 'uppercase',
                  }}>● Now</span>
                )}
                <span style={{ overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{e.title}</span>
              </div>
              <div className="meta" style={{ fontSize: 11, marginTop: 2 }}>
                {e.subscription_name}
                {e.attendees.length > 0 && ` · ${e.attendees.length} attendee${e.attendees.length === 1 ? '' : 's'}`}
                {e.location && ` · ${e.location}`}
              </div>
            </span>
            <span
              onClick={(ev) => { ev.stopPropagation(); void dismiss(e); }}
              role="button"
              aria-label="Dismiss event"
              title="Dismiss — hide from this list (persists across syncs)"
              style={{
                padding: '2px 8px', fontSize: 11, color: 'var(--iron)',
                border: '1px solid var(--frost-deep)', borderRadius: 4,
                background: 'var(--cream-pure)',
              }}
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
