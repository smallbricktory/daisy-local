// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { describe, expect, it } from 'vitest';
import { audioSrc, isLinuxWebview } from './audioSrc';

const LINUX = 'Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/605.1.15 (KHTML, like Gecko)';
const MAC = 'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko)';
const WIN = 'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.0.0 Safari/537.36 Edg/140.0.0.0';

describe('audioSrc', () => {
  it('recognises the Linux webview only', () => {
    expect(isLinuxWebview(LINUX)).toBe(true);
    expect(isLinuxWebview(MAC)).toBe(false);
    expect(isLinuxWebview(WIN)).toBe(false);
    expect(isLinuxWebview('Mozilla/5.0 (Linux; Android 14)')).toBe(false);
  });

  it('gives the Linux webview a data: URL of the bytes', async () => {
    const bytes = new Uint8Array([0x52, 0x49, 0x46, 0x46]).buffer;
    expect(await audioSrc(bytes, 'audio/wav', LINUX)).toBe('data:audio/wav;base64,UklGRg==');
  });
});
