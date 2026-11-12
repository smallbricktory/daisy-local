// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
// Two things the build never complains about: a package.json entry the code
// stopped importing, and a file under public/ nothing points at. Both keep
// being installed or shipped.
import { describe, it, expect } from 'vitest';
import { readFileSync } from 'node:fs';
import { walk } from './test/walk';

/** Everything that could name an asset or import a package. */
function corpus(): string {
  const parts: string[] = [];
  for (const f of walk('src', (n) => /\.(tsx?|css)$/.test(n))) parts.push(readFileSync(f, 'utf8'));
  // public/ carries its own CSS (the @font-face declarations live there).
  for (const f of walk('public', (n) => /\.(css|html)$/.test(n))) parts.push(readFileSync(f, 'utf8'));
  for (const f of walk('test', () => true)) parts.push(readFileSync(f, 'utf8'));
  for (const f of walk('e2e', (n) => /\.(tsx?|json)$/.test(n))) parts.push(readFileSync(f, 'utf8'));
  for (const f of ['index.html', 'vite.config.ts', 'vitest.config.ts', 'package.json']) {
    try {
      parts.push(readFileSync(f, 'utf8'));
    } catch { /* optional */ }
  }
  for (const f of ['../../README.md']) {
    try {
      parts.push(readFileSync(f, 'utf8'));
    } catch { /* optional */ }
  }
  return parts.join('\n');
}

/**
 * Packages the compiler or a tool consumes without any source importing them
 * by name. A type package is the usual case: tsc resolves `node:fs` through
 * @types/node, and nothing ever writes the package name.
 */
const TOOL_PACKAGES = new Set<string>([
  '@types/node',
  // WebdriverIO loads these by convention: the `wdio` binary,
  // `runner: 'local'`, `reporters: ['spec']`, and the TS loader for configs.
  '@wdio/cli',
  '@wdio/local-runner',
  '@wdio/spec-reporter',
  'tsx',
  // Type declarations TypeScript resolves by package name.
  '@types/qrcode',
]);

describe('dead assets', () => {
  it('every package.json dependency is referenced', () => {
    const pkg = JSON.parse(readFileSync('package.json', 'utf8'));
    const text = corpus();
    const dead: string[] = [];
    for (const kind of ['dependencies', 'devDependencies'] as const) {
      for (const name of Object.keys(pkg[kind] ?? {})) {
        // package.json itself is in the corpus, so match uses elsewhere.
        const uses = text.split(name).length - 1;
        if (uses <= 1 && !TOOL_PACKAGES.has(name)) dead.push(`  ${kind}: ${name}`);
      }
    }
    expect(
      dead.sort(),
      `packages nothing references — uninstall, or list in TOOL_PACKAGES:\n${dead.join('\n')}`,
    ).toEqual([]);
  });

  it('every file under public/ is referenced', () => {
    const text = corpus();
    const dead = walk('public', () => true)
      .filter((p) => {
        const base = p.split('/').pop() ?? p;
        return !text.includes(base);
      })
      .map((p) => `  ${p}`);
    expect(
      dead.sort(),
      `files under public/ nothing points at — they still ship:\n${dead.join('\n')}`,
    ).toEqual([]);
  });
});
