// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { describe, it, expect, vi } from 'vitest';
import { finishActiveRecording, registerScreenFinisher } from './finishRequest';

describe('finishActiveRecording', () => {
  it('hands the session to the cascade when no record screen is mounted', () => {
    const cascade = vi.fn();
    finishActiveRecording('s-1', cascade);
    expect(cascade).toHaveBeenCalledWith('s-1', 's-1');
  });

  it('lets the mounted record screen finish itself', () => {
    const cascade = vi.fn();
    const screen = vi.fn();
    const unregister = registerScreenFinisher(screen);
    finishActiveRecording('s-1', cascade);
    expect(screen).toHaveBeenCalledOnce();
    expect(cascade).not.toHaveBeenCalled();
    unregister();
    finishActiveRecording('s-1', cascade);
    expect(cascade).toHaveBeenCalledOnce();
  });

  it('an older screen unregistering leaves the newer one in place', () => {
    const first = vi.fn();
    const second = vi.fn();
    const offFirst = registerScreenFinisher(first);
    const offSecond = registerScreenFinisher(second);
    offFirst();
    finishActiveRecording('s-1', vi.fn());
    expect(second).toHaveBeenCalledOnce();
    offSecond();
  });
});
