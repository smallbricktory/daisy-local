// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
// Driving the app's own <Select> from a component test: its options exist only
// while the list is open, so opening and picking are two flushes, and the
// effects a pick starts are awaited before the call returns.
import { act, fireEvent } from '@testing-library/react';

async function openList(trigger: HTMLElement): Promise<HTMLElement> {
  if (trigger.getAttribute('aria-expanded') !== 'true') {
    await act(async () => { fireEvent.click(trigger); });
  }
  const list = trigger.parentElement?.querySelector('[role="listbox"]');
  if (!list) throw new Error('the picker opened no list');
  return list as HTMLElement;
}

/** Picks the option carrying `value`. */
export async function pickOption(trigger: HTMLElement, value: string) {
  const list = await openList(trigger);
  const option = list.querySelector(`[data-value="${value}"]`);
  if (!option) throw new Error(`the picker offers no option "${value}"`);
  await act(async () => { fireEvent.click(option); });
}

/** The labels a picker offers, in order. Leaves the list closed. */
export async function optionLabels(trigger: HTMLElement): Promise<string[]> {
  const list = await openList(trigger);
  const labels = [...list.querySelectorAll('[role="option"]')].map((o) => o.textContent ?? '');
  await act(async () => { fireEvent.click(trigger); });
  return labels;
}
