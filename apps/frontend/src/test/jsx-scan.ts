// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE

/** Attribute text of the JSX tag whose name ends at `start`. Brace-aware, so
 *  `=>` inside handlers doesn't end the tag, and quote-aware, so a `>` inside
 *  a quoted attribute value doesn't either. */
export function tagAttrs(s: string, start: number): string {
  let depth = 0;
  let quote: string | null = null;
  for (let i = start; i < s.length; i++) {
    const c = s[i];
    if (quote) {
      if (c === quote) quote = null;
    } else if (c === '"' || c === "'" || c === '`') {
      quote = c;
    } else if (c === '{') {
      depth++;
    } else if (c === '}') {
      depth--;
    } else if (c === '>' && depth === 0) {
      return s.slice(start, i);
    }
  }
  return s.slice(start);
}
