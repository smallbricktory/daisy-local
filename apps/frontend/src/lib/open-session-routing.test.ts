// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
// The session being recorded opens on the recording screen, which owns the
// transport controls and the auto-scrolling live transcript.
import { describe, it, expect } from 'vitest';
import { readFileSync } from 'node:fs';

const APP = readFileSync('src/App.tsx', 'utf8');

describe('opening a session while one is recording', () => {
  it('routes every session entry point through openSession', () => {
    // Direct setRoute to a session detail page bypasses the recording check.
    // The two permitted uses are openSession's own body and the post-stop
    // hand-off, where the recording has already ended.
    const direct = [...APP.matchAll(/setRoute\(\{\s*name:\s*'library',\s*sessionId/g)];
    expect(direct.length).toBe(2);
  });

  it('sends the actively-recording session to the recording screen', () => {
    expect(APP).toMatch(/snap\?\.session_id === sessionId[\s\S]{0,120}name: 'recording'/);
  });

  it('does not drag a just-stopped session back to the recording screen', () => {
    // Stop routes to the draft transcript while the backend snapshot still
    // reports the session as recording. The hand-off releases the session in
    // the shared store (recordingState.test.ts) before it routes.
    expect(APP).toMatch(/releaseRecording\(sessionId\);[\s\S]{0,200}setRoute\(\{ name: 'library', sessionId \}\)/);
  });

  it('redirects if the snapshot arrives after the route', () => {
    expect(APP).toMatch(
      /route\.name === 'library' && route\.sessionId === snap\.session_id[\s\S]{0,80}name: 'recording'/,
    );
  });
});
