// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { describe, it, expect } from 'vitest';
import { releaseNotesUrl } from './releaseNotes';

describe('releaseNotesUrl', () => {
  it('anchors at the version', () => {
    expect(releaseNotesUrl('1.3.3')).toBe('https://www.daisylocal.app/changelog#1.3.3');
  });

  it('strips a tester suffix', () => {
    expect(releaseNotesUrl('1.3.3-a')).toBe('https://www.daisylocal.app/changelog#1.3.3');
    expect(releaseNotesUrl('1.3.2-f')).toBe('https://www.daisylocal.app/changelog#1.3.2');
  });

  it('keeps multi-digit parts', () => {
    expect(releaseNotesUrl('12.30.104-rc1')).toBe('https://www.daisylocal.app/changelog#12.30.104');
  });

  it('opens the page unanchored when there is no semver', () => {
    expect(releaseNotesUrl('…')).toBe('https://www.daisylocal.app/changelog');
    expect(releaseNotesUrl('')).toBe('https://www.daisylocal.app/changelog');
  });
});
