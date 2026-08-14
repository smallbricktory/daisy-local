// Nav-rail icons. Drawn on a 24-grid, rendered at 18, stroked in
// `currentColor` so each icon takes its item's colour.

type Props = { name: string; className?: string };

/** Path data per nav key. `null` = no icon (the Record item draws a dot). */
const PATHS: Record<string, React.ReactNode> = {
  library: (
    <>
      <path d="M4 5.5A1.5 1.5 0 0 1 5.5 4H10v16H5.5A1.5 1.5 0 0 1 4 18.5z" />
      <path d="M10 4h4.5A1.5 1.5 0 0 1 16 5.5v13a1.5 1.5 0 0 1-1.5 1.5H10z" />
      <path d="m17.5 5.2 2 .5a1.5 1.5 0 0 1 1.05 1.84l-3 11.6" />
    </>
  ),
  calendar: (
    <>
      <rect x="3.5" y="5" width="17" height="15.5" rx="2.5" />
      <path d="M3.5 10h17M8 3.5V6M16 3.5V6" />
    </>
  ),
  search: (
    <>
      <circle cx="11" cy="11" r="6.5" />
      <path d="m15.8 15.8 4.2 4.2" />
    </>
  ),
  tasks: (
    <>
      <path d="M4 8h11a4 4 0 0 1 0 8H9" />
      <path d="m11.5 12.5-2.5 3 2.5 3" />
      <path d="M7 5 4.5 8 7 11" />
    </>
  ),
  analyzer: (
    <>
      <circle cx="12" cy="12" r="8" />
      <circle cx="12" cy="12" r="3.6" />
      <path d="M12 1.8v3M12 19.2v3M1.8 12h3M19.2 12h3" />
    </>
  ),
  history: (
    <>
      <circle cx="12" cy="12" r="8.2" />
      <path d="M12 7v5.3l3.4 2" />
    </>
  ),
  help: (
    <>
      <circle cx="12" cy="12" r="8.2" />
      <path d="M9.6 9.4a2.5 2.5 0 1 1 3.3 2.4c-.6.2-.9.7-.9 1.3v.5" />
      <path d="M12 16.8h.01" />
    </>
  ),
  mini: (
    <>
      <rect x="3.5" y="4.5" width="17" height="15" rx="2.5" />
      <path d="M12.5 12h5.5v5.5" />
    </>
  ),
  settings: (
    <>
      <circle cx="12" cy="12" r="3.2" />
      <path d="M19.9 15a1.7 1.7 0 0 0 .34 1.88l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.7 1.7 0 0 0-2.9 1.21V21a2 2 0 1 1-4 0v-.11a1.7 1.7 0 0 0-2.9-1.2l-.07.06a2 2 0 1 1-2.83-2.83l.06-.07a1.7 1.7 0 0 0-1.2-2.9H3a2 2 0 1 1 0-4h.11a1.7 1.7 0 0 0 1.2-2.9l-.05-.06a2 2 0 1 1 2.83-2.83l.06.06A1.7 1.7 0 0 0 10 4.1V4a2 2 0 1 1 4 0v.11a1.7 1.7 0 0 0 2.9 1.2l.07-.05a2 2 0 1 1 2.83 2.83l-.06.06a1.7 1.7 0 0 0 1.2 2.9H21a2 2 0 1 1 0 4h-.11a1.7 1.7 0 0 0-1.56 1.03z" />
    </>
  ),
};

export function NavIcon({ name, className }: Props) {
  const d = PATHS[name];
  if (!d) return null;
  return (
    <svg
      className={className ?? 'nav-item__icon'}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth={1.7}
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
      focusable="false"
    >
      {d}
    </svg>
  );
}
