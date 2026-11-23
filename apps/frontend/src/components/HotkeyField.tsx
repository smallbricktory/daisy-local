// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
// The keyboard-shortcut control: shows the stored combination as key caps and
// captures a new one from the next keydown.
import { useEffect, useRef, useState } from 'react';
import { accelFromKeyEvent, labelParts, type HotkeyPlatform } from '../lib/hotkey';
import { isMacPlatform } from '../lib/uiZoom';

function platform(): HotkeyPlatform {
  if (isMacPlatform()) return 'mac';
  if (typeof navigator !== 'undefined' && /Win/i.test(navigator.userAgent)) return 'win';
  return 'linux';
}

export function HotkeyField({ value, onChange, error }: {
  value: string | null;
  onChange: (v: string | null) => Promise<void>;
  error?: string;
}) {
  const [capturing, setCapturing] = useState(false);
  const changeRef = useRef(onChange);
  useEffect(() => { changeRef.current = onChange; });
  const buttonRef = useRef<HTMLButtonElement>(null);

  useEffect(() => {
    if (!capturing) return;
    buttonRef.current?.focus();
    const onKey = (e: KeyboardEvent) => {
      e.preventDefault();
      e.stopPropagation();
      if (e.key === 'Escape') {
        setCapturing(false);
        return;
      }
      const accel = accelFromKeyEvent(e);
      if (accel === null) return;
      setCapturing(false);
      void changeRef.current(accel);
    };
    window.addEventListener('keydown', onKey, true);
    return () => { window.removeEventListener('keydown', onKey, true); };
  }, [capturing]);

  const parts = value === null ? [] : labelParts(value, platform());
  return (
    <div className="hotkey-field">
      <div className="hotkey-field__row">
        <button
          ref={buttonRef}
          type="button"
          className="btn hotkey-field__button"
          data-testid="hotkey-field"
          data-accel={value ?? 'off'}
          aria-label={capturing
            ? 'Keyboard shortcut, press keys to set'
            : value === null
              ? 'Keyboard shortcut: none'
              : `Keyboard shortcut ${parts.join(' ')}`}
          onClick={() => setCapturing(true)}
          onBlur={() => setCapturing(false)}
        >
          {capturing
            ? 'Press a shortcut, Esc to cancel'
            : value === null
              ? 'Off'
              : parts.map((p, i) => (
                <span className="keycap" key={`${p}-${i}`}>{p}</span>
              ))}
        </button>
        {value !== null && !capturing && (
          <button
            type="button"
            className="btn-link"
            data-testid="hotkey-clear"
            onClick={() => { void changeRef.current(null); }}
          >
            Clear
          </button>
        )}
      </div>
      {error && <p className="meta meta--danger" data-testid="hotkey-error" aria-live="polite">{error}</p>}
    </div>
  );
}
