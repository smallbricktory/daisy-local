import { useState } from 'react';

const SWATCHES = ['#FF6A00', '#FFC13B', '#3B4B9B', '#A9B4FF', '#4A235A', '#B23A2A', '#2E3D8C', '#6c6960'];
const HEX_RE = /^#([0-9a-fA-F]{3}|[0-9a-fA-F]{6})$/;

export function TagColorPicker({ value, onChange }: { value: string; onChange: (hex: string) => void }) {
  const [hexText, setHexText] = useState(value);
  function commitHex(v: string) {
    setHexText(v);
    if (HEX_RE.test(v)) onChange(v);
  }
  return (
    <div className="color-picker">
      {SWATCHES.map((s) => (
        <button key={s} type="button" title={s}
          className={`color-picker__swatch ${value.toLowerCase() === s.toLowerCase() ? 'color-picker__swatch--sel' : ''}`}
          style={{ background: s }} onClick={() => { onChange(s); setHexText(s); }} />
      ))}
      <input className="color-picker__native" type="color" value={HEX_RE.test(value) ? value : '#FF6A00'}
        onChange={(e) => { onChange(e.target.value); setHexText(e.target.value); }} aria-label="Pick a color" />
      <input className="color-picker__hex" value={hexText} placeholder="#RRGGBB" onChange={(e) => commitHex(e.target.value)} />
      {!HEX_RE.test(hexText) && <span style={{ color: 'var(--danger)', fontSize: 11 }}>invalid hex</span>}
    </div>
  );
}
