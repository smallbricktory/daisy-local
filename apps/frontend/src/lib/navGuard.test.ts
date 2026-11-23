// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { describe, it, expect } from 'vitest';
import { canNavigate, setNavGuard } from './navGuard';

describe('navGuard', () => {
  it('allows navigation when no guard is registered', async () => {
    setNavGuard(null);
    expect(await canNavigate()).toBe(true);
  });

  it('consults the registered guard and honors its verdict', async () => {
    setNavGuard(async () => false);
    expect(await canNavigate()).toBe(false);
    setNavGuard(async () => true);
    expect(await canNavigate()).toBe(true);
  });

  it('unregistering restores default-allow', async () => {
    setNavGuard(async () => false);
    setNavGuard(null);
    expect(await canNavigate()).toBe(true);
  });
});
