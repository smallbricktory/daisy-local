// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import type { Options } from '@wdio/types';
import { mkdirSync } from 'node:fs';
import { ARTIFACTS } from './env.ts';
import { clearCalls } from './fixtures/invoke.ts';

mkdirSync(ARTIFACTS, { recursive: true });

export const shared: Options.Testrunner = {
  runner: 'local',
  // Workers must fork directly: the xvfb-run wrapper wdio inserts for
  // headless Chrome severs the worker IPC channel (write EINVAL on WSL).
  // Displays come from the launcher level (CI wraps e2e:native in xvfb-run).
  autoXvfb: false,
  specs: ['./specs/**/*.spec.ts'],
  maxInstances: 1,
  logLevel: 'info',
  outputDir: ARTIFACTS,
  waitforTimeout: 15_000,
  connectionRetryTimeout: 120_000,
  connectionRetryCount: 2,
  framework: 'mocha',
  mochaOpts: { ui: 'bdd', timeout: 180_000 },
  reporters: ['spec'],
  // Native runs every spec file against one long-lived document, so the call
  // log carries over between files. Each suite starts from an empty one.
  beforeSuite: async function () {
    await clearCalls();
  },
  afterTest: async function (test, _ctx, { passed }) {
    if (!passed) {
      const name = `${test.parent}--${test.title}`.replace(/[^a-z0-9]+/gi, '_');
      await browser.saveScreenshot(`${ARTIFACTS}/${name}.png`);
    }
  },
};
