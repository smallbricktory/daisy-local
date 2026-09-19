// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { describe, it, expect, beforeEach } from 'vitest';
import { act, renderHook } from '@testing-library/react';
import { pushToast, dismissToast, useToasts, type ToastSpec } from './toastStore';

const warn = (body: string): ToastSpec => ({
  id: 'mic-silent', severity: 'warning', title: 'Microphone is silent', body,
});

function open(): { current: ToastSpec[] } {
  const { result } = renderHook(() => useToasts());
  return result;
}

// A programmatic dismiss also lifts any standing suppression.
beforeEach(() => { dismissToast('mic-silent'); });

describe('toast store', () => {
  it('an X-dismissed toast stays down while the emitter repeats it', () => {
    const t = open();
    act(() => { pushToast(warn('No audio for 30s')); });
    act(() => { dismissToast('mic-silent', true); });
    act(() => { pushToast(warn('No audio for 30s')); });
    expect(t.current.find((x) => x.id === 'mic-silent')).toBeUndefined();
  });

  it('a different warning under the same id comes back', () => {
    const t = open();
    act(() => { pushToast(warn('No audio for 30s')); });
    act(() => { dismissToast('mic-silent', true); });
    act(() => { pushToast(warn('No audio for 5 minutes')); });
    expect(t.current.find((x) => x.id === 'mic-silent')?.body).toBe('No audio for 5 minutes');
  });

  it('a programmatic dismiss lifts the suppression', () => {
    const t = open();
    act(() => { pushToast(warn('No audio for 30s')); });
    act(() => { dismissToast('mic-silent', true); });
    act(() => { dismissToast('mic-silent'); });
    act(() => { pushToast(warn('No audio for 30s')); });
    expect(t.current.find((x) => x.id === 'mic-silent')).toBeDefined();
  });

  it('the X runs the toast\'s onDismiss; a programmatic dismiss does not', () => {
    let ran = 0;
    act(() => { pushToast({ ...warn('No audio for 30s'), onDismiss: () => { ran += 1; } }); });
    act(() => { dismissToast('mic-silent'); });
    expect(ran).toBe(0);
    act(() => { pushToast({ ...warn('No audio for 30s'), onDismiss: () => { ran += 1; } }); });
    act(() => { dismissToast('mic-silent', true); });
    expect(ran).toBe(1);
  });
});
