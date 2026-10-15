// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
// What a tag looks like once it is attached: the squares a library row draws,
// and the two named halves of the picker.
import { expect } from '@wdio/globals';
import { isNative } from '../env.ts';
import { bootWorld } from '../fixtures/install.ts';
import { nav } from '../pages/shell.ts';
import { library } from '../pages/library.ts';
import { tagging } from '../pages/session.ts';
import type { SessionListEntry } from '../../src/tauri.ts';

/** A session carries this alongside real ids: a tag another device applied
 *  that never reached this catalog. Nothing here can name it. */
const UNKNOWN = 'tag-from-another-device';

const MANY = 's-001';
const ONE = 's-003';

/** `s-001` holds one unnameable id and three real ones; `s-003` holds one.
 *  The ids are the world's own, so summaries and transcripts still resolve. */
const SESSIONS: Partial<SessionListEntry>[] = [
  { session_id: MANY, tag_ids: [UNKNOWN, 't-1', 't-2', 't-3'] },
  { session_id: ONE, tag_ids: ['t-1'] },
];

describe('tags on screen', () => {
  before(async function () {
    // Needs a seeded catalog and seeded sessions; the native sandbox has both empty.
    if (isNative()) this.skip();
  });

  describe('a library row', () => {
    before(async () => {
      const now = Math.floor(Date.now() / 1000);
      await bootWorld({
        sessions: SESSIONS.map((s, i) => ({
          created_at_unix_seconds: now - i * 3600,
          finalized_at_unix_seconds: now - i * 3600 + 600,
          duration_seconds: 600,
          title: `Meeting ${i + 1}`,
          has_transcript: true,
          has_dedup: true,
          has_summary: true,
          ...s,
        })) as SessionListEntry[],
      });
      await nav.go('library');
    });

    it('draws a square per tag and collapses the rest into a count', async () => {
      await expect(library.tagDots(MANY)).toBeElementsArrayOfSize(2);
      await expect(library.tagMore(MANY)).toHaveText('+1');
    });

    it('spends no square and no count on a tag it cannot name', async () => {
      // Four ids, three of them nameable: two squares and "+1", never "+2".
      await expect(library.tagMore(MANY)).not.toHaveText('+2');
    });

    it('leaves the count off a row whose tags all fit', async () => {
      await expect(library.tagDots(ONE)).toBeElementsArrayOfSize(1);
      await expect(library.tagMore(ONE)).not.toBeExisting();
    });

    it('paints each square in its own tag colour', async () => {
      const fills = await browser.execute(() =>
        [...document.querySelectorAll('.session-row__tagdot')].map(
          (el) => getComputedStyle(el).backgroundColor,
        ));
      expect(fills.length).toBeGreaterThan(0);
      for (const fill of fills) {
        expect(fill).not.toBe('rgba(0, 0, 0, 0)');
      }
    });
  });

  describe('the tag picker', () => {
    before(async () => {
      await bootWorld();
      await nav.go('library');
      // `s-004` is the world's untagged session with tags to suggest.
      await library.openRow('s-004');
      await tagging.openPicker();
    });

    it('names both halves and rules a line between them', async () => {
      const structure = await tagging.popStructure();
      expect(structure.filter((s) => s.startsWith('section:') || s === 'sep')).toEqual([
        'section:Suggested',
        'sep',
        'section:All tags',
      ]);
    });

    it('puts every suggestion above the rule and the catalog below it', async () => {
      const structure = await tagging.popStructure();
      const sep = structure.indexOf('sep');
      expect(sep).toBeGreaterThan(1);
      expect(structure.slice(sep + 1).filter((s) => s === 'item').length).toBeGreaterThan(0);
    });
  });
});
