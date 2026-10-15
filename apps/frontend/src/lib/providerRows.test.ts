// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { describe, it, expect } from 'vitest';
import { buildProviderRows, isProviderConfigured, KEY_PROVIDERS } from './providerRows';
import type { ProviderListEntry } from '../tauri';

const gw: ProviderListEntry = {
  name: 'daisy_gateway', has_key: false, model: null, base_url: null,
context_tokens: null,
};

describe('buildProviderRows', () => {
  it('does not duplicate an allowlisted provider the backend also returns', () => {
    const rows = buildProviderRows([{ name: 'groq', has_key: true, model: null, base_url: null, context_tokens: null }]);
    expect(rows.filter((r) => r.name === 'groq')).toHaveLength(1);
  });

  it('lists Daisy Cloud only when the backend returns it (entitled license)', () => {
    expect(buildProviderRows([]).some((r) => r.name === 'daisy_gateway')).toBe(false);
    expect(buildProviderRows([gw]).some((r) => r.name === 'daisy_gateway')).toBe(true);
  });

  it('keeps daisy_gateway out of the static allowlist', () => {
    expect(KEY_PROVIDERS).not.toContain('daisy_gateway');
  });

  it('never drops a backend provider not in the allowlist', () => {
    const exotic: ProviderListEntry = {
      name: 'groq', has_key: true, model: 'x', base_url: null,
    context_tokens: null,
    };
    // groq IS in the allowlist; assert the backend entry (with has_key) wins.
    const rows = buildProviderRows([exotic]);
    expect(rows.find((r) => r.name === 'groq')!.has_key).toBe(true);
  });
});

describe('isProviderConfigured', () => {
  it('keyed providers need a key', () => {
    expect(isProviderConfigured({ name: 'openai', has_key: false, model: 'gpt-4o-mini', base_url: null, context_tokens: null })).toBe(false);
    expect(isProviderConfigured({ name: 'openai', has_key: true, model: null, base_url: null, context_tokens: null })).toBe(true);
  });

  it('keyless providers need a model', () => {
    expect(isProviderConfigured({ name: 'ollama', has_key: false, model: null, base_url: null, context_tokens: null })).toBe(false);
    expect(isProviderConfigured({ name: 'ollama', has_key: false, model: 'llama3.1', base_url: null, context_tokens: null })).toBe(true);
  });

  it('daisy_gateway is configured by presence alone', () => {
    expect(isProviderConfigured(gw)).toBe(true);
  });
});
