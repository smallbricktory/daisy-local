// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
// The src an <audio> element gets for a clip held in memory.

/** True in the Linux webview (WebKitGTK). */
export function isLinuxWebview(ua: string = navigator.userAgent): boolean {
  return /Linux/.test(ua) && !/Android/.test(ua);
}

/** A src that plays `bytes` as `type`: a data: URL in the Linux webview, a
 *  Blob URL elsewhere. */
export async function audioSrc(bytes: ArrayBuffer, type: string, ua?: string): Promise<string> {
  const blob = new Blob([bytes], { type });
  if (!isLinuxWebview(ua)) return URL.createObjectURL(blob);
  return new Promise<string>((resolve, reject) => {
    const r = new FileReader();
    r.onload = () => resolve(String(r.result));
    r.onerror = () => reject(r.error ?? new Error('could not read the clip'));
    r.readAsDataURL(blob);
  });
}
