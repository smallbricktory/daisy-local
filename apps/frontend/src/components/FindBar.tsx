import { useEffect, useRef, useState } from 'react';

/**
 * Ctrl-F / Cmd-F find-in-page bar. Embedded webviews (WebView2 / WebKitGTK /
 * WKWebView) have no browser chrome and the native find shortcut does
 * nothing on its own; the engines do implement `window.find()`, which
 * highlights and scrolls to a match. This bar is the UI in front of that
 * engine call. No DOM search of its own.
 *
 * Mounted once at the top of the main window (not the mini window).
 */
export function FindBar() {
  const [open, setOpen] = useState(false);
  const [q, setQ] = useState('');
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.ctrlKey || e.metaKey) && !e.shiftKey && e.key.toLowerCase() === 'f') {
        e.preventDefault();
        setOpen(true);
        // Focus + select: a second Ctrl-F retypes over the old query.
        requestAnimationFrame(() => inputRef.current?.select());
      }
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, []);

  function runFind(backwards: boolean) {
    if (!q) return;
    // window.find(str, caseSensitive, backwards, wrapAround). Non-standard but
    // present in Chromium (WebView2) and WebKit (GTK / WKWebView).
    const w = window as unknown as {
      find?: (s: string, c?: boolean, b?: boolean, w?: boolean) => boolean;
    };
    w.find?.(q, false, backwards, true);
  }

  function close() {
    setOpen(false);
    window.getSelection?.()?.removeAllRanges();
  }

  if (!open) return null;
  return (
    <div className="findbar" role="search">
      <input
        ref={inputRef}
        className="findbar__input"
        placeholder="Find in page…"
        value={q}
        onChange={(e) => setQ(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === 'Enter') { e.preventDefault(); runFind(e.shiftKey); }
          else if (e.key === 'Escape') { e.preventDefault(); close(); }
        }}
        // eslint-disable-next-line jsx-a11y/no-autofocus
        autoFocus
      />
      <button className="findbar__btn" title="Previous (Shift+Enter)" aria-label="Previous match" onClick={() => runFind(true)}>▲</button>
      <button className="findbar__btn" title="Next (Enter)" aria-label="Next match" onClick={() => runFind(false)}>▼</button>
      <button className="findbar__btn" title="Close (Esc)" aria-label="Close find" onClick={close}>✕</button>
    </div>
  );
}
