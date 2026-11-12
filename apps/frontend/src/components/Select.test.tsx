// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
// The keyboard and focus contract the app's own list of values owes.
import { describe, it, expect, vi } from 'vitest';
import { act, render, fireEvent } from '@testing-library/react';
import { Select, type SelectOption } from './Select';

const OPTS: SelectOption[] = [
  { value: 'a', label: 'Alpha' },
  { value: 'b', label: 'Bravo' },
  { value: 'c', label: 'Charlie' },
];

function mount(props: Partial<Parameters<typeof Select>[0]> = {}) {
  const onChange = vi.fn();
  const r = render(
    <Select testId="pick" label="Pick" value="a" options={OPTS} onChange={onChange} {...props} />,
  );
  const trigger = r.getByTestId('pick');
  const list = () => trigger.parentElement?.querySelector('[role="listbox"]');
  const active = () => trigger.parentElement?.querySelector('[data-active="true"]')?.getAttribute('data-value');
  return { ...r, onChange, trigger, list, active };
}

describe('Select', () => {
  it('opens on a click with the focus the keyboard contract assumes', async () => {
    // WebKit does not focus a button on mousedown, so a mouse-opened list has
    // to take the focus itself or every key below reaches nothing.
    const { trigger, list } = mount();
    await act(async () => { fireEvent.click(trigger); });
    expect(list()).not.toBeNull();
    expect(document.activeElement).toBe(trigger);
  });

  it('Escape closes the list and leaves the value as it was', async () => {
    const { trigger, list, onChange } = mount();
    await act(async () => { fireEvent.click(trigger); });
    await act(async () => { fireEvent.keyDown(trigger, { key: 'Escape' }); });
    expect(list()).toBeNull();
    expect(trigger.getAttribute('aria-expanded')).toBe('false');
    expect(onChange).not.toHaveBeenCalled();
  });

  it('hands the focus back after a pick, the way the native control does', async () => {
    const { trigger, list, onChange } = mount();
    await act(async () => { fireEvent.click(trigger); });
    const bravo = list()!.querySelector('[data-value="b"]')!;
    await act(async () => { fireEvent.click(bravo); });
    expect(onChange).toHaveBeenCalledWith('b');
    expect(list()).toBeNull();
    expect(document.activeElement).toBe(trigger);
  });

  it('Home and End reach the first and last option that can be chosen', async () => {
    const opts: SelectOption[] = [
      { value: 'a', label: 'Alpha', disabled: true },
      { value: 'b', label: 'Bravo' },
      { value: 'c', label: 'Charlie' },
      { value: 'd', label: 'Delta', disabled: true },
    ];
    const { trigger, active } = mount({ value: 'c', options: opts });
    await act(async () => { fireEvent.click(trigger); });
    await act(async () => { fireEvent.keyDown(trigger, { key: 'Home' }); });
    expect(active()).toBe('b');
    await act(async () => { fireEvent.keyDown(trigger, { key: 'End' }); });
    expect(active()).toBe('c');
  });

  it('Enter takes the active option, and a disabled one takes nothing', async () => {
    const opts: SelectOption[] = [
      { value: 'a', label: 'Alpha' },
      { value: 'b', label: 'Bravo', disabled: true },
      { value: 'c', label: 'Charlie' },
    ];
    const { trigger, onChange, active } = mount({ options: opts });
    await act(async () => { fireEvent.click(trigger); });
    await act(async () => { fireEvent.keyDown(trigger, { key: 'ArrowDown' }); });
    expect(active()).toBe('c');
    await act(async () => { fireEvent.keyDown(trigger, { key: 'Enter' }); });
    expect(onChange).toHaveBeenCalledWith('c');
  });

  it('typing jumps to a label, and the buffer does not survive the list', async () => {
    const { trigger, active, onChange } = mount();
    await act(async () => { fireEvent.click(trigger); });
    await act(async () => { fireEvent.keyDown(trigger, { key: 'c' }); });
    expect(active()).toBe('c');
    await act(async () => { fireEvent.keyDown(trigger, { key: 'Escape' }); });
    // 'c' then 'b' would read as "cb" and match nothing if the buffer carried.
    await act(async () => { fireEvent.click(trigger); });
    await act(async () => { fireEvent.keyDown(trigger, { key: 'b' }); });
    expect(active()).toBe('b');
    expect(onChange).not.toHaveBeenCalled();
  });

  it('a list with nothing to offer is not a control', async () => {
    const { trigger, list } = mount({ options: [], value: '' });
    expect(trigger).toBeDisabled();
    await act(async () => { fireEvent.click(trigger); });
    expect(list()).toBeNull();
    expect(trigger.getAttribute('aria-expanded')).toBe('false');
    expect(trigger.getAttribute('aria-activedescendant')).toBeNull();
  });

  it('re-derives the active row when the list is re-read while open', async () => {
    // `onOpen` re-reads the microphones attached right now; the index the
    // arrows walk has to follow the values that arrived, not the ones that left.
    const { trigger, active, rerender } = mount({ value: 'c' });
    await act(async () => { fireEvent.click(trigger); });
    expect(active()).toBe('c');
    await act(async () => {
      rerender(
        <Select
          testId="pick"
          label="Pick"
          value="c"
          options={[{ value: 'z', label: 'Zulu' }, { value: 'c', label: 'Charlie' }]}
          onChange={() => {}}
        />,
      );
    });
    expect(active()).toBe('c');
  });

  it('points aria-activedescendant at a row that exists', async () => {
    const { trigger, list } = mount();
    await act(async () => { fireEvent.click(trigger); });
    const id = trigger.getAttribute('aria-activedescendant')!;
    expect(id).toBeTruthy();
    expect(list()!.querySelector(`#${CSS.escape(id)}`)).not.toBeNull();
  });

  it('marks one row selected even when two options carry one value', async () => {
    const opts: SelectOption[] = [
      { value: 'a', label: 'Alpha' },
      { value: 'a', label: 'Alpha (again)' },
    ];
    const { trigger, list } = mount({ options: opts });
    await act(async () => { fireEvent.click(trigger); });
    const on = [...list()!.querySelectorAll('[aria-selected="true"]')];
    expect(on).toHaveLength(1);
  });
});
