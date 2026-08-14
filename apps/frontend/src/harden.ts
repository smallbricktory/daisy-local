// Production-only UI hardening: blocks the right-click menu and the common
// devtools / view-source keyboard shortcuts. A deterrent, not a security
// boundary — the WebView can still be inspected via env-var remote
// debugging. Release builds also compile without the `devtools` tauri
// feature; the inspector is not bundled.
export function hardenWebview(): void {
  if (!import.meta.env.PROD) return; // dev builds are not hardened

  document.addEventListener('contextmenu', (e) => e.preventDefault());

  document.addEventListener('keydown', (e) => {
    const k = e.key.toLowerCase();
    const block =
      e.key === 'F12' ||
      (e.ctrlKey && e.shiftKey && (k === 'i' || k === 'j' || k === 'c')) || // devtools
      (e.ctrlKey && k === 'u'); // view-source
    if (block) {
      e.preventDefault();
      e.stopPropagation();
    }
  });
}
