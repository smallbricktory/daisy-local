// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { $, $$, browser, expect } from '@wdio/globals';

export const tid = (id: string) => $(`[data-testid="${id}"]`);
export const tids = (prefix: string) => $$(`[data-testid^="${prefix}"]`);

/** One element chainable, as `tid` returns. */
export type El = ReturnType<typeof tid>;

/** The same lookups inside one subtree. A screen whose component is mounted
 *  twice at once — the same labeler behind a modal and inside it — puts every
 *  one of its ids in the document twice, and `tid` answers with the first. */
export const within = (root: El, id: string) => root.$(`[data-testid="${id}"]`);
export const withinAll = (root: El, prefix: string) => root.$$(`[data-testid^="${prefix}"]`);

export async function typeIntoEl(el: El, text: string) {
  await el.click();
  await el.clearValue();
  // A controlled input re-renders after the clear rather than during it: React
  // can write the stored value back into the node between the two calls below,
  // and the typed text then lands after it ("Alex" + "Alex Q"). Waiting for the
  // node to actually read empty is what makes the clear stick.
  await browser.waitUntil(async () => (await el.getValue()) === '', {
    timeoutMsg: 'the field still held a value after it was cleared',
  });
  await el.setValue(text);
}

export async function typeInto(id: string, text: string) {
  await typeIntoEl(tid(id), text);
}

/** Assigns an input's value the way the browser's own UI does. WebDriver types
 *  into segmented date/time inputs in the machine locale's keystroke order —
 *  assigning the value and firing the control's event is what both engines
 *  take. React reads the value off the node, so the assignment goes through
 *  the prototype setter it patches. */
export async function setNativeValue(id: string, value: string) {
  await browser.execute((testid: string, v: string) => {
    const el = document.querySelector(`[data-testid="${testid}"]`) as HTMLInputElement | null;
    if (!el) throw new Error(`no element carries data-testid="${testid}"`);
    const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')?.set;
    if (!setter) throw new Error('input has no value setter');
    setter.call(el, v);
    el.dispatchEvent(new Event('input', { bubbles: true }));
  }, id, value);
}

/** Opens a Select's list. The options exist only while it is open. */
async function openSelect(id: string) {
  const trigger = tid(id);
  await trigger.waitForClickable();
  if ((await trigger.getAttribute('aria-expanded')) !== 'true') await trigger.click();
  await browser.waitUntil(async () => (await trigger.getAttribute('aria-expanded')) === 'true', {
    timeoutMsg: `the picker "${id}" never opened its list`,
  });
}

async function closeSelect(id: string) {
  const trigger = tid(id);
  if ((await trigger.getAttribute('aria-expanded')) !== 'true') return;
  await trigger.click();
  await browser.waitUntil(async () => (await trigger.getAttribute('aria-expanded')) !== 'true', {
    timeoutMsg: `the picker "${id}" never closed its list`,
  });
}

/** Clicks an option in a Select's list. Waits for the option to be rendered: a
 *  list that has not yet loaded its values carries none of them. Use where the
 *  app may refuse the choice; `selectOption` is the one that waits for it. */
export async function tryOption(id: string, value: string) {
  await openSelect(id);
  const trigger = tid(id);
  const option = $(`[data-testid="${id}"] ~ [role="listbox"] [role="option"][data-value="${value}"]`);
  await option.waitForClickable({
    timeoutMsg: `the picker "${id}" never offered the option "${value}"`,
  });
  await option.click();
  await browser.waitUntil(async () => (await trigger.getAttribute('aria-expanded')) !== 'true', {
    timeoutMsg: `the picker "${id}" kept its list open after the option was taken`,
  });
}

/** Picks a Select option and waits for the control to carry it. */
export async function selectOption(id: string, value: string) {
  await tryOption(id, value);
  await browser.waitUntil(async () => (await tid(id).getAttribute('data-value')) === value, {
    timeoutMsg: `the picker "${id}" never took the option "${value}"`,
  });
}

/** What a picker is showing. Its value lives on the trigger, where the list
 *  the reader picked from put it. */
export async function pickedValue(el: El): Promise<string> {
  return (await el.getAttribute('data-value')) ?? '';
}

export async function expectPicked(el: El, value: string) {
  await expect(el).toHaveAttribute('data-value', value);
}

/** Hovers an element the way a pointer entering it does. WebKitGTK's
 *  WebDriver moves the pointer and raises mousemove alone: no mouseover, no
 *  :hover. When the move leaves the element unhovered, the events a pointer
 *  entering it raises are dispatched by hand. */
export async function hover(el: El): Promise<void> {
  await el.moveTo();
  const target = await el;
  const hovered = await browser.execute((e) => (e as unknown as HTMLElement).matches(':hover'), target);
  if (hovered) return;
  await browser.execute((e) => {
    const h = e as unknown as HTMLElement;
    h.dispatchEvent(new MouseEvent('mouseover', { bubbles: true }));
    h.dispatchEvent(new MouseEvent('mouseenter'));
  }, target);
}

export async function pressEnter() { await browser.keys('Enter'); }
export async function pressEscape() { await browser.keys('Escape'); }

export async function expectText(id: string, s: string) {
  await expect(tid(id)).toHaveText(expect.stringContaining(s));
}

/** Where a list element's row text lives relative to the element itself. */
export type RowContainer = 'parent' | 'tr' | 'self';

/** Id suffix of the element under `prefix` whose row text carries `label`, or
 *  null. Ids are minted by the backend, so the label is the only handle on a
 *  row a spec just created. Read in one pass in the page: walking the rows
 *  over the wire hands back references the next re-render invalidates. */
export async function idByText(
  prefix: string, label: string, container: RowContainer,
): Promise<string | null> {
  return browser.execute((pre: string, lab: string, cont: string) => {
    const els = Array.from(document.querySelectorAll(`[data-testid^="${pre}"]`));
    const textOf = (el: Element) => (
      cont === 'parent' ? el.parentElement?.textContent
        : cont === 'tr' ? el.closest('tr')?.textContent
          : el.textContent) ?? '';
    const hit = els.find((el) => textOf(el).includes(lab));
    return hit ? (hit.getAttribute('data-testid') ?? '').replace(pre, '') : null;
  }, prefix, label, container);
}

/** What a picker offers, in order. Opens the list to read it and leaves it as
 *  it was found. */
async function optionsOf(selectId: string): Promise<{ value: string; label: string }[]> {
  await openSelect(selectId);
  const options: { value: string; label: string }[] = await browser.execute((sel: string) => {
    const root = document.querySelector(`[data-testid="${sel}"]`)?.parentElement;
    return [...(root?.querySelectorAll('[role="option"]') ?? [])].map((o) => ({
      value: o.getAttribute('data-value') ?? '',
      label: o.textContent ?? '',
    }));
  }, selectId);
  await closeSelect(selectId);
  return options;
}

/** Value of the option under `selectId` whose label carries `label`, or null. */
export async function optionValueByText(selectId: string, label: string): Promise<string | null> {
  const hit = (await optionsOf(selectId)).find((o) => o.label.includes(label));
  return hit?.value ?? null;
}

/** Values a picker offers, in order. */
export async function optionValues(selectId: string): Promise<string[]> {
  return (await optionsOf(selectId)).map((o) => o.value);
}

/** Waits for `idByName` to answer for `name` and returns the id. */
export async function waitForIdByName(
  idByName: (name: string) => Promise<string | null>, name: string, what: string,
): Promise<string> {
  let id: string | null = null;
  await browser.waitUntil(async () => {
    id = await idByName(name);
    return id !== null;
  }, { timeoutMsg: `the ${what} "${name}" never joined the list` });
  if (id === null) throw new Error(`the ${what} "${name}" never joined the list`);
  return id;
}

/** Id suffixes of every element under `prefix`, in display order. */
export async function idsOf(prefix: string): Promise<string[]> {
  const ids: string[] = [];
  for (const el of await tids(prefix)) {
    const id = await el.getAttribute('data-testid');
    if (id !== null) ids.push(id.replace(prefix, ''));
  }
  return ids;
}

/** Waits for a row under `prefix` whose text carries `text`. All rows' text is
 *  read in one round trip per poll. */
export async function expectRowWithText(prefix: string, text: string, msg: string) {
  await browser.waitUntil(async () => {
    const texts: string[] = await browser.execute((pre: string) => (
      Array.from(document.querySelectorAll(`[data-testid^="${pre}"]`))
        .map((el) => el.textContent ?? '')
    ), prefix);
    return texts.some((t) => t.includes(text));
  }, { timeoutMsg: msg });
}

/** Waits for the row anchored on `anchorId` — its parent element or enclosing
 *  `<tr>` — to carry every one of `parts`. Saving closes an editor before the
 *  reloaded list lands, so the read retries. */
export async function expectRowText(
  anchorId: string, container: Exclude<RowContainer, 'self'>, what: string, parts: string[],
) {
  await browser.waitUntil(async () => {
    const text: string = await browser.execute((id: string, cont: string) => {
      const el = document.querySelector(`[data-testid="${id}"]`);
      const box = cont === 'tr' ? el?.closest('tr') : el?.parentElement;
      return box?.textContent ?? '';
    }, anchorId, container);
    return parts.every((p) => text.includes(p));
  }, { timeoutMsg: `the ${what} row never showed: ${parts.join(' + ')}` });
}

/** Puts a checkbox or a switch into state `on` and waits for it to report that
 *  state. A switch is a button with role="switch", and isSelected() answers
 *  false for one however it is set — aria-checked is where its state lives. */
async function isOn(el: El): Promise<boolean> {
  const aria = await el.getAttribute('aria-checked');
  return aria === null ? el.isSelected() : aria === 'true';
}

export async function setCheckbox(el: El, on: boolean, what: string) {
  if ((await isOn(el)) !== on) await el.click();
  await browser.waitUntil(async () => (await isOn(el)) === on, {
    timeoutMsg: `${what} never reported enabled=${on}`,
  });
}

/** A click on a scrim, off the modal it backs. The modal covers the scrim's
 *  centre, so the click is raised on the scrim node itself. */
export async function clickScrim(id: string) {
  await browser.execute((testid: string) => {
    (document.querySelector(`[data-testid="${testid}"]`) as HTMLElement).click();
  }, id);
}

/** A row's destructive and secondary actions live behind its overflow kebab
 *  (RowMenu). Opens the menu, then clicks the item — the item exists only
 *  while the menu is open, so the two are always one call. */
export async function clickRowMenuItem(menuId: string, itemId: string) {
  const kebab = tid(menuId);
  await kebab.waitForClickable();
  await kebab.click();
  const item = tid(itemId);
  await item.waitForClickable();
  await item.click();
}
