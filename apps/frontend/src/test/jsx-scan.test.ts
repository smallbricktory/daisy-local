// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { describe, it, expect } from 'vitest';
import { tagAttrs } from './jsx-scan';

describe('tagAttrs', () => {
  it('ends the tag at a bare >', () => {
    const s = '<button data-testid="x">go</button>';
    expect(tagAttrs(s, '<button'.length)).toBe(' data-testid="x"');
  });

  it('a > inside a quoted attribute value stays in the tag', () => {
    const s = '<button title="a > b" onClick={go} data-testid="x">go</button>';
    const attrs = tagAttrs(s, '<button'.length);
    expect(attrs).toContain('data-testid="x"');
    expect(attrs).toContain('onClick={go}');
  });

  it('a > inside a braced handler stays in the tag', () => {
    const s = '<input onChange={(e) => set(e)} data-testid="y">';
    const attrs = tagAttrs(s, '<input'.length);
    expect(attrs).toContain('data-testid="y"');
  });
});
