// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
// The model rows of model-notices.txt, for the in-app licenses list: each
// section title between two rules, with the name and URL of its License line.

import { readFileSync } from 'node:fs';

const RULE = /^-{20,}\s*$/;

export function modelLicenses(path) {
  const lines = readFileSync(path, 'utf8').split('\n');
  const out = [];
  for (let i = 1; i + 1 < lines.length; i++) {
    if (!RULE.test(lines[i - 1]) || !RULE.test(lines[i + 1]) || RULE.test(lines[i])) continue;
    const name = lines[i].trim().split(/\s{2,}/)[0];
    const next = lines.slice(i + 2).findIndex((l) => RULE.test(l));
    const section = next < 0 ? lines.slice(i + 2) : lines.slice(i + 2, i + 2 + next);
    const line = section.find((l) => l.startsWith('License:'));
    if (!line) continue;
    const [license, rest = ''] = line.slice('License:'.length).trim().split(' — ');
    const url = rest.startsWith('https://') ? rest.split(/\s/)[0] : null;
    out.push({ name, license, url });
  }
  return out;
}
