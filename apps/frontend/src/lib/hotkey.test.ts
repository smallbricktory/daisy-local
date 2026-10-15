// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { describe, it, expect } from 'vitest';
import { accelFromKeyEvent, labelParts } from './hotkey';

const ev = (over: Partial<Parameters<typeof accelFromKeyEvent>[0]>) => ({
  key: '', code: '', ctrlKey: false, altKey: false, shiftKey: false, metaKey: false, ...over,
});

describe('accelFromKeyEvent', () => {
  it('names the modifiers in Control, Alt, Shift, Super order', () => {
    expect(accelFromKeyEvent(ev({ key: 'r', code: 'KeyR', ctrlKey: true, altKey: true })))
      .toBe('Control+Alt+R');
    expect(accelFromKeyEvent(ev({
      key: 'F9', code: 'F9', shiftKey: true, altKey: true,
    }))).toBe('Alt+Shift+F9');
    expect(accelFromKeyEvent(ev({ key: '4', code: 'Digit4', metaKey: true })))
      .toBe('Super+4');
    expect(accelFromKeyEvent(ev({ key: 'ArrowUp', code: 'ArrowUp', ctrlKey: true, shiftKey: true })))
      .toBe('Control+Shift+ArrowUp');
  });

  it('refuses a keydown that carries no shortcut', () => {
    expect(accelFromKeyEvent(ev({ key: 'r', code: 'KeyR' }))).toBeNull();
    expect(accelFromKeyEvent(ev({ key: 'Control', code: 'ControlLeft', ctrlKey: true }))).toBeNull();
    expect(accelFromKeyEvent(ev({ key: 'Escape', code: 'Escape', ctrlKey: true }))).toBeNull();
  });

  it('falls back to the typed key when the layout reports no code', () => {
    expect(accelFromKeyEvent(ev({ key: 'p', code: '', ctrlKey: true }))).toBe('Control+P');
  });

  it('accepts the named keys the backend parser also holds', () => {
    const names = [
      'Space', 'Tab', 'Enter', 'Backspace', 'Delete', 'Insert',
      'Home', 'End', 'PageUp', 'PageDown',
    ];
    for (const name of names) {
      expect(accelFromKeyEvent(ev({ key: name, code: name, ctrlKey: true }))).toBe(`Control+${name}`);
    }
  });
});

describe('labelParts', () => {
  it('draws mac glyphs, Windows words, and Linux Super', () => {
    expect(labelParts('Control+Alt+R', 'mac')).toEqual(['⌃', '⌥', 'R']);
    expect(labelParts('Control+Alt+R', 'win')).toEqual(['Ctrl', 'Alt', 'R']);
    expect(labelParts('Control+Alt+R', 'linux')).toEqual(['Ctrl', 'Alt', 'R']);
    expect(labelParts('Super+Shift+4', 'mac')).toEqual(['⌘', '⇧', '4']);
    expect(labelParts('Super+Shift+4', 'win')).toEqual(['Win', 'Shift', '4']);
    expect(labelParts('Super+Shift+4', 'linux')).toEqual(['Super', 'Shift', '4']);
    expect(labelParts('Control+ArrowUp', 'win')).toEqual(['Ctrl', '↑']);
    expect(labelParts('Alt+F9', 'win')).toEqual(['Alt', 'F9']);
  });
});
