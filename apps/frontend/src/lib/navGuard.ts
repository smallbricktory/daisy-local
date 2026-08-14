// Global navigation guard. A page with unsaved state (e.g. an edited prompt
// directive in Analyzer or Settings → Prompts) registers a guard; App.tsx asks
// it before honoring a nav-rail route change. The guard returns true to allow
// navigation (typically after showing the app confirm dialog) or false to stay.
// One guard at a time — last registration wins; unregister on save/clean/unmount.
let guard: (() => Promise<boolean>) | null = null;

export function setNavGuard(g: (() => Promise<boolean>) | null): void {
  guard = g;
}

export async function canNavigate(): Promise<boolean> {
  return guard ? guard() : true;
}
