// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { useState } from 'react';

/** Twelve hues around the wheel at one lightness, the same list and order the
 *  phone shows (`ios/DaisyKit/Sources/DaisyKit/Tags.swift`, `TagCatalog
 *  .swatches`), so a tag made on either device is a colour the other's picker
 *  shows as chosen. Each clears 2:1 against white and against black, which is
 *  what a 7px chip dot needs to be seen on either ground. */
export const TAG_SWATCHES = [
  '#DD3131', '#EB760F', '#E0B406', '#73A923', '#2BA13B', '#1FA382',
  '#1C9AAB', '#1E81C8', '#3664E2', '#7B5AD8', '#B450CE', '#DD4099',
];
/** What a tag being made starts on: the first swatch, so the picker opens
 *  with a ring already on it. */
export const DEFAULT_TAG_COLOR = TAG_SWATCHES[0];
const HEX_RE = /^#([0-9a-fA-F]{3}|[0-9a-fA-F]{6})$/;

export function TagColorPicker({ value, onChange }: { value: string; onChange: (hex: string) => void }) {
  const [hexText, setHexText] = useState(value);
  function commitHex(v: string) {
    setHexText(v);
    if (HEX_RE.test(v)) onChange(v);
  }
  return (
    <div className="color-picker">
      <div className="color-picker__grid">
        {TAG_SWATCHES.map((s) => {
          const sel = value.toLowerCase() === s.toLowerCase();
          return (
            <button key={s} type="button" title={s} data-testid={`tag-color-${s.replace('#', '')}`}
              aria-label={s} aria-pressed={sel}
              className={`color-picker__swatch ${sel ? 'color-picker__swatch--sel' : ''}`}
              style={{ '--swatch': s } as React.CSSProperties} onClick={() => { onChange(s); setHexText(s); }} />
          );
        })}
      </div>
      <input data-testid="tag-color-native" className="color-picker__native" type="color" value={HEX_RE.test(value) ? value : DEFAULT_TAG_COLOR}
        onChange={(e) => { onChange(e.target.value); setHexText(e.target.value); }} aria-label="Pick a color" />
      <input data-testid="tag-color-hex" className="color-picker__hex" value={hexText} placeholder="#RRGGBB" onChange={(e) => commitHex(e.target.value)} />
      {!HEX_RE.test(hexText) && <span className="meta--danger" style={{ fontSize: 'var(--t-meta)' }}>invalid hex</span>}
    </div>
  );
}
