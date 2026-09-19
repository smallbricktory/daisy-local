// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
// Page objects for the app shell: nav rail, top banners, toast stack, the
// global confirm dialog, the find bar and the two shell-level modals.
import { browser, expect } from '@wdio/globals';
import { tid } from './_shared.ts';

/** WebDriver key code for the find shortcut's modifier: Command on macOS,
 *  Control elsewhere. FindBar accepts either. */
const FIND_MODIFIER = process.platform === 'darwin' ? '\uE03D' : '\uE009';

/** Nav-rail keys that route inside the app. `help` is a nav item too, but it
 *  opens an external URL in the system browser — it has no accessor here. */
export type NavKey =
  | 'record' | 'library' | 'calendar' | 'search' | 'tasks' | 'analyzer' | 'history'
  | 'settings' | 'mini';

export const nav = {
  item: (k: NavKey) => tid(`nav-${k}`),
  /** The routed page container. */
  body: () => tid('app-body'),
  async has(k: NavKey): Promise<boolean> {
    return this.item(k).isExisting();
  },
  /** Clicks a nav item and waits for it to become the active route. */
  async go(k: Exclude<NavKey, 'mini'>) {
    await this.item(k).click();
    await expect(this.item(k)).toHaveElementClass('active');
    await expect(this.body()).toBeDisplayed();
  },
  async expectRecordStyle() {
    await expect(this.item('record')).toHaveElementClass('nav-item--record');
  },
};

export const banner = {
  dismiss: () => tid('banner-dismiss'),
  keepDaisy: () => tid('banner-keep-daisy'),
  resubscribe: () => tid('banner-resubscribe'),
  openSettings: () => tid('banner-open-settings'),
  openChangelog: () => tid('app-open-changelog'),
  expired: () => tid('banner-expired'),
  activate: () => tid('banner-activate'),
};

export const toast = {
  /** A toast by its store id, e.g. `audio-mic-very-quiet`. */
  byId: (id: string) => tid(`toast-${id}`),
  /** An action button inside one toast, by its slugified label. */
  action: (id: string, slug: string) => tid(`toast-${id}`).$(`[data-testid="toast-action-${slug}"]`),
  /** Clicks that toast's X and waits for it to leave the stack. */
  async dismissById(id: string) {
    await this.byId(id).$('[data-testid="toast-dismiss"]').click();
    await expect(this.byId(id)).not.toBeExisting();
  },
};

export const confirmDialog = {
  backdrop: () => tid('confirm-backdrop'),
  modal: () => tid('confirm-modal'),
  text: () => tid('confirm-text'),
  accept: () => tid('confirm-accept'),
  cancel: () => tid('confirm-cancel'),
  /** Types the confirmation word when the dialog demands one, then accepts. */
  async acceptTyping(word?: string) {
    if (word) await this.text().setValue(word);
    await expect(this.accept()).toBeEnabled();
    await this.accept().click();
    await expect(this.modal()).not.toBeExisting();
  },
  async cancelOut() {
    await this.cancel().click();
    await expect(this.modal()).not.toBeExisting();
  },
};

export const findBar = {
  input: () => tid('findbar-input'),
  prev: () => tid('findbar-prev'),
  next: () => tid('findbar-next'),
  close: () => tid('findbar-close'),
  /** Ctrl/Cmd+F on the document. The bar listens on window, so any route works. */
  async open() {
    await browser.keys([FIND_MODIFIER, 'f']);
    await expect(this.input()).toBeDisplayed();
  },
  /** Types a query and steps to the next match. `window.find()` reports
   *  nothing back to the page, so the query itself is the visible outcome. */
  async typeQuery(term: string) {
    await this.input().setValue(term);
    await this.next().click();
    await expect(this.input()).toHaveValue(term);
  },
  async closeIt() {
    await this.close().click();
    await expect(this.input()).not.toBeExisting();
  },
};

export const aiRequired = {
  scrim: () => tid('ai-required-scrim'),
  modal: () => tid('ai-required-modal'),
  close: () => tid('ai-required-close'),
  openProviders: () => tid('ai-required-open-providers'),
  nerdHint: () => tid('ai-required-nerd-hint'),
  getKey: () => tid('ai-required-get-key'),
  builtin: () => tid('ai-required-builtin'),
  error: () => tid('ai-required-error'),
  async dismiss() {
    await this.close().click();
    await expect(this.modal()).not.toBeExisting();
  },
};

export const gatewayNotice = {
  scrim: () => tid('gateway-notice-scrim'),
  modal: () => tid('gateway-notice-modal'),
  faq: () => tid('gateway-notice-faq'),
  close: () => tid('gateway-notice-close'),
  async dismiss() {
    await this.close().click();
    await expect(this.modal()).not.toBeExisting();
  },
};

export const markdownEditor = {
  root: () => tid('md-editor'),
};
