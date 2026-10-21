// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { configDefaults, defineConfig } from 'vitest/config';
import react from '@vitejs/plugin-react';

// Node ≥ 22 defines its own `localStorage` global, which is unusable without
// `--localstorage-file`; the jsdom environment keeps a global that already
// exists, so the tests would see Node's. Turning Node's off hands them jsdom's.
// Older Node has no such flag.
const nodeMajor = Number(process.versions.node.split('.')[0]);
const execArgv = nodeMajor >= 22 ? ['--no-experimental-webstorage'] : [];

export default defineConfig({
  plugins: [react()],
  test: {
    environment: 'jsdom',
    globals: true,
    setupFiles: ['./src/test/setup.ts'],
    css: true,
    poolOptions: { forks: { execArgv }, threads: { execArgv } },
    // e2e/ specs are WebdriverIO's; they need a running app, not jsdom. The
    // world-coverage guard is a plain unit test over the fixture source.
    include: [...configDefaults.include, 'e2e/world-coverage.test.ts'],
    exclude: [...configDefaults.exclude, 'e2e/specs/**'],
  },
});
