// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { readFileSync } from 'node:fs';
import { describe, it, expect } from 'vitest';
import { TAG_SWATCHES, DEFAULT_TAG_COLOR } from './TagColorPicker';

// The phone offers the same twelve colours in the same order
// (smbr-daisy-ios ios/DaisyKit/Sources/DaisyKit/Tags.swift, TagCatalog
// .swatches), and it asserts against a byte-identical copy of this fixture.
describe('tag swatches', () => {
  const fixture = JSON.parse(readFileSync('test/fixtures/tag-swatches-v1.json', 'utf8'));

  it('match the shared fixture, in order', () => {
    expect(TAG_SWATCHES).toEqual(fixture.swatches);
  });

  it('start on the first swatch, so the picker opens with a ring on one', () => {
    expect(DEFAULT_TAG_COLOR).toBe(fixture.swatches[0]);
  });
});
