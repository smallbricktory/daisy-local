/**
 * Host webview detection.
 *
 * Only Chromium-based webviews (Windows WebView2) have a reliable Web Audio
 * stack. WebKitGTK (Linux) returns silent getUserMedia streams, and WKWebView
 * (macOS) NATIVELY CRASHES inside its AVFoundation capture path when the page
 * calls `getUserMedia` or even `enumerateDevices` — a SIGSEGV the JS
 * try/catch can't stop. Callers gate every `navigator.mediaDevices.*` use
 * behind this and fall back to the Rust-side backend meter on non-Chromium
 * hosts.
 *
 * macOS WKWebView's UA on recent releases carries a "Chrome/" token;
 * Macintosh is excluded explicitly before the Chrome/Edge check.
 */
export function isChromiumWebview(): boolean {
  if (typeof navigator === 'undefined') return false;
  if (/Macintosh|Mac OS X/.test(navigator.userAgent)) return false;
  return /Chrome\/|Edg\//.test(navigator.userAgent);
}
