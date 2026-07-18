import { useEffect, useState } from 'react';
import './../reminder-window.css';
import { listen } from '@tauri-apps/api/event';
import { tauri } from '../tauri';

/**
 * Meeting-start reminder popup. A small frameless always-on-top webview
 * (label "reminder"), pre-rendered hidden at startup and shown by the main
 * window's scheduler at `start − reminder_lead_seconds`. It only shows the
 * title and emits Open / hides on Dismiss. The main window owns the calendar
 * event + does the actual "open recording" routing.
 */
export function ReminderWindow() {
  const [title, setTitle] = useState<string>('');

  useEffect(() => {
    // The title is stored backend-side by show_reminder_window and read from
    // there, not from the event payload (the pre-rendered window can miss an
    // emit). Re-read on every reminder:show; a second meeting refreshes it.
    // Falls back to "A meeting" when the store is empty.
    const refresh = () => tauri.reminderPayload().then((t) => setTitle(t ?? '')).catch(() => {});
    void refresh();
    let unlisten: (() => void) | undefined;
    let cancelled = false;
    listen('reminder:show', () => { void refresh(); })
      .then((fn) => { if (cancelled) fn(); else unlisten = fn; })
      .catch(() => {});
    return () => { cancelled = true; unlisten?.(); };
  }, []);

  // Both go through the backend (the reminder webview isn't granted window-hide,
  // and Open must reach the main window).
  const onOpen = () => { tauri.reminderAction(true).catch(() => {}); };
  const onDismiss = () => { tauri.reminderAction(false).catch(() => {}); };

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
        <button className="reminder__btn reminder__btn--open" onClick={onOpen}>Open</button>
        <button className="reminder__btn" onClick={onDismiss}>Dismiss</button>
      </div>
    </div>
  );
}
