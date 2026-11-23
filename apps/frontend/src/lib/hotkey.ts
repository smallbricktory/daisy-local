// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
// Accelerator strings: what a keydown means, and how a stored accelerator
// reads on this platform.

/** The keydown fields an accelerator is built from. */
export interface HotkeyEventLike {
  key: string;
  code: string;
  ctrlKey: boolean;
  altKey: boolean;
  shiftKey: boolean;
  metaKey: boolean;
}

const ARROWS = ['ArrowUp', 'ArrowDown', 'ArrowLeft', 'ArrowRight'];
// KeyboardEvent.code values that are also names the backend's accelerator
// parser accepts verbatim (global-hotkey's `parse_key`).
const NAMED_KEYS = [
  'Space', 'Tab', 'Enter', 'Backspace', 'Delete', 'Insert',
  'Home', 'End', 'PageUp', 'PageDown',
];

/** The key half of an accelerator in Tauri's vocabulary, or null when this
 *  key cannot carry a shortcut. */
function keyName(e: HotkeyEventLike): string | null {
  const code = e.code || (/^[a-zA-Z]$/.test(e.key) ? `Key${e.key.toUpperCase()}` : '');
  const letter = /^Key([A-Z])$/.exec(code);
  if (letter) return letter[1];
  const digit = /^Digit([0-9])$/.exec(code);
  if (digit) return digit[1];
  if (/^F([1-9]|1[0-9]|2[0-4])$/.test(code)) return code;
  if (ARROWS.includes(code)) return code;
  if (NAMED_KEYS.includes(code)) return code;
  return null;
}

/** The accelerator a keydown means ("Control+Alt+R"), or null when it carries
 *  no modifier or no shortcut-capable key. */
export function accelFromKeyEvent(e: HotkeyEventLike): string | null {
  const mods: string[] = [];
  if (e.ctrlKey) mods.push('Control');
  if (e.altKey) mods.push('Alt');
  if (e.shiftKey) mods.push('Shift');
  if (e.metaKey) mods.push('Super');
  if (mods.length === 0) return null;
  const key = keyName(e);
  if (key === null) return null;
  return [...mods, key].join('+');
}

export type HotkeyPlatform = 'mac' | 'win' | 'linux';

const MAC_MODS: Record<string, string> = {
  Control: '⌃', Alt: '⌥', Shift: '⇧', Super: '⌘',
};
const WIN_MODS: Record<string, string> = {
  Control: 'Ctrl', Alt: 'Alt', Shift: 'Shift', Super: 'Win',
};
const LINUX_MODS: Record<string, string> = {
  Control: 'Ctrl', Alt: 'Alt', Shift: 'Shift', Super: 'Super',
};
const ARROW_GLYPHS: Record<string, string> = {
  ArrowUp: '↑', ArrowDown: '↓', ArrowLeft: '←', ArrowRight: '→',
};

/** One cap per accelerator part: ["⌃", "⌥", "R"] on macOS, ["Ctrl", "Alt",
 *  "R"] on Windows, ["Ctrl", "Alt", "R"] with "Super" (not "Win") on Linux. */
export function labelParts(accel: string, platform: HotkeyPlatform): string[] {
  const mods = platform === 'mac' ? MAC_MODS : platform === 'win' ? WIN_MODS : LINUX_MODS;
  return accel.split('+').map((p) => mods[p] ?? ARROW_GLYPHS[p] ?? p);
}
