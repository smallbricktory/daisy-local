// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
// Page objects for Settings: the section tabs, and the Recordings, Display,
// Tags, Prompts, Providers, Integrations, Storage, Voiceprints, Calendars, MCP,
// About and Profile panes (including the vault card).
import { browser, expect } from '@wdio/globals';
import {
  clickRowMenuItem, expectPicked, expectRowText, idByText, optionValueByText, selectOption,
  setCheckbox, tid, tids, tryOption, typeInto, waitForIdByName,
} from './_shared.ts';
import { confirmDialog, nav } from './shell.ts';
import { ensureAbsent } from './teardown.ts';
import { mockInvoke, restoreInvoke } from '../fixtures/invoke.ts';
import { emitAppEvent } from '../fixtures/events.ts';

export type Section =
  | 'recordings' | 'display' | 'tags' | 'prompts' | 'providers' | 'integrations'
  | 'storage' | 'voiceprints' | 'calendars' | 'mcp' | 'profile' | 'about' | 'mobile sync';

/** Appearance modes the theme cards offer. */
export type ThemeMode = 'light' | 'dark' | 'system';

/** Detailed-logs levels. */
export type DebugLevel = 'off' | 'basic' | 'full';

export const settings = {
  section: (s: Section) => tid(`settings-section-${s}`),
  error: () => tid('settings-error'),
  /** Routes to Settings and opens one section; the tab marks itself active. */
  async open(s: Section) {
    await nav.go('settings');
    await this.section(s).click();
    await expect(this.section(s)).toHaveElementClass('is-on');
  },
};

export const display = {
  theme: (m: ThemeMode) => tid(`display-theme-${m}`),
  cssTheme: () => tid('display-css-theme'),
  zoomOut: () => tid('display-zoom-out'),
  zoomIn: () => tid('display-zoom-in'),
  zoomReset: () => tid('display-zoom-reset'),
  /** Picks an appearance. The cards report the selection with aria-pressed. */
  async chooseTheme(m: ThemeMode) {
    await this.theme(m).click();
    await expect(this.theme(m)).toHaveAttribute('aria-pressed', 'true');
  },
  /** One step up. Reset arms itself as soon as the factor leaves 100%. */
  async zoomInOnce() {
    await this.zoomIn().click();
    await expect(this.zoomReset()).toBeEnabled();
  },
  /** Back to 100%, which disables Reset again. */
  async resetZoom() {
    await this.zoomReset().click();
    await expect(this.zoomReset()).toBeDisabled();
  },
  /** Leaves the UI at 100% whether or not it is zoomed. */
  async ensureZoomReset() {
    if (await this.zoomReset().isEnabled()) await this.zoomReset().click();
    await expect(this.zoomReset()).toBeDisabled();
  },
};

const BUILTIN_STATUS = 'builtin_model_status';
/** The built-in model row a pinned status carries; the AI gates read only
 *  its `provider`. */
const PINNED_ROW = {
  id: 'gemma-4-e4b-it-q4', display: 'Gemma 4 E4B', installed: true, bytes: 5_130_000_000,
  license: 'Gemma Terms of Use', license_url: 'https://ai.google.dev/gemma/terms',
  capability: 'fast', capability_line: 'Fast on this machine.', partial_bytes: 0, warming: false, warm_failed: null,
};

/** The AI provider status part of `builtin_model_status`. */
export interface AiStatus {
  state: string;
  provider: string | null;
  hint: string | null;
}

/** Every AI gate (the session pane, Search Q&A, the Analyzer, the Copy
 *  buttons) reads one status, which follows `builtin-model:changed`. */
export const aiStatus = {
  /** Answers the status with `status` and has every gate read it now. */
  async pin(status: AiStatus) {
    const payload = { model: PINNED_ROW, provider: status };
    await mockInvoke(BUILTIN_STATUS, payload);
    await emitAppEvent('builtin-model:changed', payload);
  },
  /** Hands the status back to the backend and has every gate read it. */
  async release() {
    await restoreInvoke(BUILTIN_STATUS);
    const current = await browser.execute(async (cmd: string) => {
      const w = window as unknown as { __TAURI_INTERNALS__: { invoke: (c: string) => Promise<unknown> } };
      return w.__TAURI_INTERNALS__.invoke(cmd);
    }, BUILTIN_STATUS);
    await emitAppEvent('builtin-model:changed', current);
  },
};

export const nerdMode = {
  toggle: () => tid('toggle-nerd-mode'),
  badge: () => tid('settings-nerd-badge'),
  /** Opens Display and leaves the switch in the asked-for state. */
  async set(on: boolean) {
    await settings.open('display');
    if ((await this.toggle().getAttribute('aria-checked')) !== String(on)) {
      await this.toggle().click();
    }
    await expect(this.toggle()).toHaveAttribute('aria-checked', String(on));
  },
};

export const recordings = {
  mic: () => tid('rec-mic'),
  aec: () => tid('rec-aec'),
  aiProvider: () => tid('rec-ai-provider'),
  aiProviderError: () => tid('rec-ai-provider-error'),
  debugLevel: () => tid('rec-debug-level'),
  openLogs: () => tid('rec-open-logs'),
  meetingPromptHelp: () => tid('meeting-prompt-help'),
  meetingStopToggle: () => tid('toggle-meeting-stop-prompt'),
  meetingPromptAskAgain: () => tid('meeting-prompt-ask-again'),
  speechLevelDevice: () => tid('rec-speech-level-device'),
  calibrate: () => tid('rec-calibrate'),
  manual: () => tid('rec-speech-level-manual'),
  slider: () => tid('rec-speech-level-slider'),
  /** One switch by its label slug, e.g. `reduce-background-noise`. */
  toggle: (slug: string) => tid(`toggle-${slug}`),
  // Providers → Advanced → Transcription tuning.
  whisperThreads: () => tid('rec-whisper-threads'),
  whisperLanguage: () => tid('rec-whisper-language'),
  whisperReset: () => tid('rec-whisper-reset'),
  hotkey: () => tid('hotkey-field'),
  hotkeyClear: () => tid('hotkey-clear'),
  hotkeyError: () => tid('hotkey-error'),
  /** Opens capture mode and presses one combination; the field reports the
   *  accelerator it stored. */
  async captureHotkey(keys: string[], accel: string) {
    await this.hotkey().click();
    await expect(this.hotkey()).toHaveText('Press a shortcut, Esc to cancel');
    await browser.keys(keys);
    await expect(this.hotkey()).toHaveAttribute('data-accel', accel);
  },
  /** Picks the AI provider; the app keeps it. */
  async setAiProvider(id: string) {
    await selectOption('rec-ai-provider', id);
    await expectPicked(this.aiProvider(), id);
  },
  /** Offers the AI provider a choice the backend may refuse — an unentitled
   *  provider leaves the previous one in place and names the reason. */
  async tryAiProvider(id: string) {
    await tryOption('rec-ai-provider', id);
  },
  /** Picks a logging level; the select keeps it. */
  async setDebugLevel(level: DebugLevel) {
    await selectOption('rec-debug-level', level);
    await expectPicked(this.debugLevel(), level);
  },
  /** Flips one switch and waits for its new aria-checked state. */
  async flipToggle(slug: string) {
    const before = await this.toggle(slug).getAttribute('aria-checked');
    await this.toggle(slug).click();
    await expect(this.toggle(slug)).toHaveAttribute('aria-checked', before === 'true' ? 'false' : 'true');
  },
};

export const tags = {
  row: (id: string) => tid(`tags-row-${id}`),
  fresh: () => tid('tags-new'),
  name: () => tid('tag-name'),
  /** Swatch by hex digits without the `#`; a click fills the hex field. */
  swatch: (hex: string) => tid(`tag-color-${hex}`),
  colorHex: () => tid('tag-color-hex'),
  prompt: () => tid('tag-prompt'),
  vocab: () => tid('tag-vocab'),
  save: () => tid('tag-save'),
  cancel: () => tid('tag-cancel'),
  del: () => tid('tag-delete'),
  /** Id of the listed tag carrying this name, or null. */
  async idByName(name: string): Promise<string | null> {
    return idByText('tags-row-', name, 'self');
  },
  /** Fills the new-tag form and saves; the tag joins the list on the left and
   *  its id comes back. */
  async create(name: string, prompt?: string): Promise<string> {
    await this.fresh().click();
    await typeInto('tag-name', name);
    if (prompt !== undefined) await typeInto('tag-prompt', prompt);
    await this.save().click();
    return waitForIdByName((n) => this.idByName(n), name, 'tag');
  },
  /** Selects a listed tag; the form seeds from it and offers Delete. */
  async select(id: string) {
    await this.row(id).click();
    await expect(this.del()).toBeDisplayed();
  },
  /** Renames the selected tag; the list row follows the new name. */
  async rename(id: string, to: string) {
    await typeInto('tag-name', to);
    await this.save().click();
    await expect(this.row(id)).toHaveText(expect.stringContaining(to));
  },
  /** Removes the tag with this name when one is listed. Idempotent. */
  async ensureDeleted(name: string) {
    await ensureAbsent({
      what: 'tag',
      name,
      open: () => settings.open('tags'),
      idByName: (n) => this.idByName(n),
      deleteOne: async (id) => {
        await this.row(id).click();
        await this.del().click();
        await confirmDialog.acceptTyping();
        await expect(this.row(id)).not.toBeExisting();
      },
    });
  },
};

export const prompts = {
  pick: () => tid('prompts-pick'),
  name: () => tid('prompt-name'),
  directive: () => tid('prompt-directive'),
  save: () => tid('prompt-save'),
  reset: () => tid('prompt-reset'),
  saveAs: () => tid('prompt-save-as'),
  del: () => tid('prompt-delete'),
  makeDefault: () => tid('prompt-make-default'),
  newName: () => tid('prompt-new-name'),
  newSave: () => tid('prompt-new-save'),
  newCancel: () => tid('prompt-new-cancel'),
  /** Id of the option whose label carries this name, or null when the picker
   *  offers no such prompt. The picker labels the default one "… · default",
   *  so the match is on a substring. */
  async idByName(name: string): Promise<string | null> {
    return optionValueByText('prompts-pick', name);
  },
  /** Picks a prompt by id; the form seeds from it. */
  async choose(id: string) {
    await selectOption('prompts-pick', id);
    await expectPicked(this.pick(), id);
  },
  /** Copies the shown directive into a new prompt under `name`, which the
   *  form then holds. */
  async saveAsNew(name: string) {
    await this.saveAs().click();
    await typeInto('prompt-new-name', name);
    await this.newSave().click();
    await expect(this.name()).toHaveValue(name);
  },
  /** Rewrites the directive and saves; Save disarms once the draft matches
   *  what is stored. */
  async setDirective(text: string) {
    await typeInto('prompt-directive', text);
    await expect(this.save()).toBeEnabled();
    await this.save().click();
    await expect(this.save()).toBeDisabled();
  },
  /** Removes the prompt with this name when the picker offers one. Idempotent. */
  async ensureDeleted(name: string) {
    await ensureAbsent({
      what: 'prompt',
      name,
      open: async () => {
        await settings.open('display');
        await settings.open('prompts');
      },
      idByName: (n) => this.idByName(n),
      deleteOne: async (id) => {
        await this.choose(id);
        await this.del().click();
        await confirmDialog.acceptTyping();
        await expect(this.pick().$(`option[value="${id}"]`)).not.toBeExisting();
      },
    });
  },
};

export const providers = {
  configure: (name: string) => tid(`provider-configure-${name}`),
  row: (name: string) => tid(`provider-row-${name}`),
  enable: (name: string) => tid(`provider-enable-${name}`),
  enabled: (name: string) => tid(`provider-enabled-${name}`),
  enableError: () => tid('provider-enable-error'),
  rowMeta: (name: string) => tid(`provider-meta-${name}`),
  key: () => tid('provider-key'),
  baseUrl: () => tid('provider-base-url'),
  test: () => tid('provider-test'),
  clearKey: () => tid('provider-clear-key'),
  cancel: () => tid('provider-cancel'),
  model: () => tid('provider-model'),
  modelText: () => tid('provider-model-text'),
  modelCustom: () => tid('provider-model-custom'),
  contextTokens: () => tid('provider-context-tokens'),
  /** Empties the context-length box one keystroke at a time. */
  async clearContextTokens() {
    await this.contextTokens().click();
    const len = (await this.contextTokens().getValue()).length;
    if (len > 0) await browser.keys(new Array(len).fill('Backspace'));
    await expect(this.contextTokens()).toHaveValue('');
  },
  save: () => tid('provider-save'),
  back: () => tid('provider-back'),
  cancel2: () => tid('provider-cancel-2'),
  /** Opens step 1 (key + base URL) on one provider row. The key field is
   *  rendered only for providers that authenticate, so the base URL is the
   *  marker every provider shows. */
  async open(name: string) {
    await this.configure(name).click();
    await expect(this.baseUrl()).toBeDisplayed();
  },
  /** Step 1 → step 2, through the provider's endpoint. */
  async listModels() {
    await this.test().click();
    await expect(this.model()).toBeDisplayed();
  },
  /** Leaves the row collapsed from either step. */
  async ensureClosed(name: string) {
    if (await this.cancel2().isExisting()) await this.cancel2().click();
    if (await this.cancel().isExisting()) await this.cancel().click();
    await expect(this.configure(name)).toBeDisplayed();
  },
};

export const localModel = {
  row: (id: string) => tid(`llm-row-${id}`),
  meta: (id: string) => tid(`llm-meta-${id}`),
  license: (id: string) => tid(`llm-license-${id}`),
  download: (id: string) => tid(`llm-download-${id}`),
  cancel: (id: string) => tid(`llm-cancel-${id}`),
  progress: (id: string) => tid(`llm-progress-${id}`),
  partialMenu: (id: string) => tid(`llm-partial-menu-${id}`),
  removePartial: (id: string) => tid(`llm-remove-partial-${id}`),
  menu: (id: string) => tid(`llm-menu-${id}`),
  del: (id: string) => tid(`llm-delete-${id}`),
  enable: (id: string) => tid(`llm-enable-${id}`),
  enabled: (id: string) => tid(`llm-enabled-${id}`),
  message: () => tid('llm-message'),
};

export const asrModels = {
  license: (id: string) => tid(`asr-license-${id}`),
};

/** Auth choices the destination editor offers. `keep` shows only when editing
 *  a destination that already stores a credential. */
export type IntegrationAuth = 'keep' | 'none' | 'header' | 'bearer';

export const integrations = {
  add: () => tid('integration-add'),
  edit: (id: string) => tid(`integration-edit-${id}`),
  menu: (id: string) => tid(`integration-menu-${id}`),
  name: () => tid('integration-name'),
  url: () => tid('integration-url'),
  auth: (m: IntegrationAuth) => tid(`integration-auth-${m}`),
  headerName: () => tid('integration-header-name'),
  headerValue: () => tid('integration-header-value'),
  bearer: () => tid('integration-bearer'),
  includeSummary: () => tid('integration-include-summary'),
  includeNotes: () => tid('integration-include-notes'),
  includeTranscript: () => tid('integration-include-transcript'),
  enabled: () => tid('integration-enabled'),
  save: () => tid('integration-save'),
  cancel: () => tid('integration-cancel'),
  /** Id of the listed destination carrying this name, or null. */
  async idByName(name: string): Promise<string | null> {
    return idByText('integration-edit-', name, 'parent');
  },
  /** Adds a bearer-authenticated webhook destination and returns its id.
   *  Saving stores it in the vault; nothing is sent to the URL. */
  async createBearerWebhook(name: string, url: string, token: string): Promise<string> {
    await this.add().click();
    await typeInto('integration-name', name);
    await typeInto('integration-url', url);
    await this.auth('bearer').click();
    await expect(this.auth('bearer')).toBeSelected();
    await this.bearer().setValue(token);
    await this.save().click();
    await expect(this.add()).toBeDisplayed();
    return waitForIdByName((n) => this.idByName(n), name, 'destination');
  },
  /** Waits for the list row of one destination — the container its buttons sit
   *  in, which is where the name, URL, auth kind and payload summary are
   *  rendered — to carry every one of these strings. */
  async expectRowText(id: string, ...parts: string[]) {
    await expectRowText(`integration-edit-${id}`, 'parent', 'destination', parts);
  },
  /** Reopens one destination's editor. An existing credential is kept unless
   *  another auth choice replaces it, so the editor opens on "keep current". */
  async openEditor(id: string) {
    await this.edit(id).click();
    await expect(this.name()).toBeDisplayed();
  },
  /** Removes the destination with this name when one is listed. Idempotent. */
  async ensureAbsent(name: string) {
    await ensureAbsent({
      what: 'destination',
      name,
      open: async () => {
        await settings.open('integrations');
        if (await this.cancel().isExisting()) await this.cancel().click();
      },
      idByName: (n) => this.idByName(n),
      deleteOne: async (id) => {
        await clickRowMenuItem(`integration-menu-${id}`, `integration-delete-${id}`);
        await confirmDialog.acceptTyping();
        await expect(this.menu(id)).not.toBeExisting();
      },
    });
  },
};

export const voiceprints = {
  rematch: () => tid('vp-rematch'),
  rename: (id: string) => tid(`vp-rename-${id}`),
  menu: (id: string) => tid(`vp-menu-${id}`),
  advanced: () => tid('vp-advanced'),
  tolerance: () => tid('vp-tolerance'),
  modal: () => tid('vp-rename-modal'),
  name: () => tid('vp-name'),
  email: () => tid('vp-email'),
  save: () => tid('vp-save'),
  cancel: () => tid('vp-cancel'),
  /** Opens the "Speaker recognition tuning" details; skips the toggle when already open. */
  async openAdvanced() {
    if (!(await this.tolerance().isDisplayed())) await this.advanced().click();
    await expect(this.tolerance()).toBeDisplayed();
  },
  /** Id of the listed voiceprint carrying this name, or null. */
  async idByName(name: string): Promise<string | null> {
    return idByText('vp-rename-', name, 'tr');
  },
  /** Waits for one voiceprint's table row — where the name, email and the two
   *  counts are rendered — to carry every one of these strings. */
  async expectRowText(id: string, ...parts: string[]) {
    await expectRowText(`vp-rename-${id}`, 'tr', 'voiceprint', parts);
  },
  /** Renames one voiceprint through its modal; the table row follows. */
  async renameTo(id: string, to: string, email?: string) {
    await this.rename(id).click();
    await expect(this.modal()).toBeDisplayed();
    await typeInto('vp-name', to);
    if (email !== undefined) await typeInto('vp-email', email);
    await this.save().click();
    await expect(this.modal()).not.toBeExisting();
    await this.expectRowText(id, to);
  },
  /** Leaves the table without its modal whether or not one is open. */
  async ensureModalClosed() {
    if (await this.cancel().isExisting()) await this.cancel().click();
    await expect(this.modal()).not.toBeExisting();
  },
};

export const calendars = {
  name: () => tid('cal-name'),
  url: () => tid('cal-url'),
  add: () => tid('cal-add'),
  refresh: () => tid('cal-refresh'),
  enabled: (id: string) => tid(`cal-enabled-${id}`),
  color: (id: string) => tid(`cal-color-${id}`),
  tag: (id: string) => tid(`cal-tag-${id}`),
  menu: (id: string) => tid(`cal-menu-${id}`),
  /** Id of the listed subscription carrying this name, or null. */
  async idByName(name: string): Promise<string | null> {
    return idByText('cal-menu-', name, 'tr');
  },
  /** Adds an ICS subscription and returns its id. The URL is written to the
   *  vault; the fetch happens only on Refresh. */
  async addSubscription(name: string, url: string): Promise<string> {
    await typeInto('cal-name', name);
    await typeInto('cal-url', url);
    await expect(this.add()).toBeEnabled();
    await this.add().click();
    return waitForIdByName((n) => this.idByName(n), name, 'calendar');
  },
  /** Flips one subscription's Enabled box and waits for the reloaded row to
   *  report the new state. */
  async setEnabled(id: string, on: boolean) {
    await setCheckbox(this.enabled(id), on, 'the calendar row');
  },
  /** Opens the row's overflow and confirms Delete. */
  async deleteSubscription(id: string) {
    await clickRowMenuItem(`cal-menu-${id}`, `cal-delete-${id}`);
    await confirmDialog.acceptTyping();
  },
  /** Removes the subscription with this name when one is listed. Idempotent. */
  async ensureAbsent(name: string) {
    await ensureAbsent({
      what: 'calendar',
      name,
      open: () => settings.open('calendars'),
      idByName: (n) => this.idByName(n),
      deleteOne: async (id) => {
        await clickRowMenuItem(`cal-menu-${id}`, `cal-delete-${id}`);
        await confirmDialog.acceptTyping();
        await expect(this.menu(id)).not.toBeExisting();
      },
    });
  },
};

export const mcp = {
  enabled: () => tid('mcp-enabled'),
  port: () => tid('mcp-port'),
  copy: () => tid('mcp-copy'),
  regenerate: () => tid('mcp-regenerate'),
  allowWrite: () => tid('mcp-allow-write'),
  /** Flips the server switch and waits for the panel to follow: the connect
     *  command and its two buttons are rendered only while it is on. */
  async setEnabled(on: boolean) {
    await setCheckbox(this.enabled(), on, 'the MCP switch');
    if (on) await expect(this.regenerate()).toBeDisplayed();
    else await expect(this.regenerate()).not.toBeExisting();
  },
  /** Leaves the server switched off. Idempotent. */
  async ensureDisabled() {
    await settings.open('mcp');
    await this.setEnabled(false);
  },
  /** Flips the write-access box and waits for its new state. */
  async flipAllowWrite() {
    const before = await this.allowWrite().isSelected();
    await setCheckbox(this.allowWrite(), !before, 'the write-access box');
  },
};

/** The two tabs the legal viewer offers. */
export type LegalTab = 'tos' | 'privacy';

export const about = {
  version: () => tid('about-version'),
  checkUpdate: () => tid('about-check-update'),
  releaseNotes: () => tid('about-release-notes'),
  viewTerms: () => tid('about-view-terms'),
  legalModal: () => tid('about-legal-modal'),
  legalTab: (t: LegalTab) => tid(`about-legal-${t}`),
  legalClose: () => tid('about-legal-close'),
  oss: () => tid('about-oss-licenses'),
  ossMirror: () => tid('about-oss-mirror'),
  modelLicenses: () => tid('about-model-licenses'),
  modelLicenseLinks: () => tids('about-model-license-'),
  linkSite: () => tid('about-link-site'),
  linkGithub: () => tid('about-link-github'),
  legalCredits: () => tid('about-legal-credits'),
  /** Opens the footer disclosure holding the two links and the licence text. */
  async openLegalCredits() {
    await this.legalCredits().click();
    await expect(this.linkSite()).toBeDisplayed();
  },
  // License card. Which half is rendered follows the install's state:
  // Deactivate on a licensed one, the key box and its two buttons otherwise.
  deactivate: () => tid('about-deactivate'),
  licenseKey: () => tid('about-license-key'),
  activate: () => tid('about-activate'),
  getLicense: () => tid('about-get-license'),
  async openLegal() {
    await this.viewTerms().click();
    await expect(this.legalModal()).toBeDisplayed();
  },
  /** Picks a tab and waits for the document under it. */
  async chooseLegalTab(t: LegalTab, marker: string) {
    await this.legalTab(t).click();
    await browser.waitUntil(async () => {
      const text: string = await browser.execute(() => {
        return document.querySelector('[data-testid="about-legal-modal"]')?.textContent ?? '';
      });
      return text.includes(marker);
    }, { timeoutMsg: `the ${t} tab never showed: ${marker}` });
  },
  /** Leaves About without the legal modal whether or not it is open. */
  async ensureLegalClosed() {
    if (await this.legalClose().isExisting()) await this.legalClose().click();
    await expect(this.legalModal()).not.toBeExisting();
  },
  /** Expands the open-source licenses panel; the mirror link is its content. */
  async openLicenses() {
    await this.oss().click();
    await expect(this.ossMirror()).toBeDisplayed();
  },
  async closeLicenses() {
    await this.oss().click();
    await expect(this.ossMirror()).not.toBeExisting();
  },
};

export const storage = {
  clear: () => tid('storage-clear'),
  /** Opens the type-DELETE confirm — the app's one confirm dialog, so its
   *  fields are `confirmDialog`'s. The button is inert while nothing is
   *  deletable, so callers check `clear().isEnabled()` first. */
  async openClear() {
    await this.clear().click();
    await expect(confirmDialog.text()).toBeDisplayed();
  },
  /** Types the confirmation word; the delete button arms only on an exact
   *  match. */
  async typeConfirmWord(word: string) {
    await typeInto('confirm-text', word);
    await expect(confirmDialog.text()).toHaveValue(word);
  },
  async cancelClear() {
    await confirmDialog.cancelOut();
    await expect(this.clear()).toBeDisplayed();
  },
};

export const profile = {
  /** Read-only name on a licensed install; bound to the licensee. */
  name: () => tid('profile-name'),
  displayName: () => tid('profile-display-name'),
  save: () => tid('profile-save'),
  openDir: () => tid('profile-open-dir'),
  switchProfile: () => tid('profile-switch'),
  move: () => tid('profile-move'),
  openWizard: () => tid('profile-open-wizard'),
  // Vault, machine mode.
  vaultAddPassphrase: () => tid('vault-add-passphrase'),
  vaultNew: () => tid('vault-new'),
  vaultNewConfirm: () => tid('vault-new-confirm'),
  vaultAddSubmit: () => tid('vault-add-submit'),
  vaultAddCancel: () => tid('vault-add-cancel'),
  // Vault, passphrase mode.
  vaultChange: () => tid('vault-change'),
  vaultLock: () => tid('vault-lock'),
  vaultDrop: () => tid('vault-drop'),
  vaultCurrent: () => tid('vault-current'),
  vaultChangeNew: () => tid('vault-change-new'),
  vaultChangeConfirm: () => tid('vault-change-confirm'),
  vaultChangeSubmit: () => tid('vault-change-submit'),
  vaultChangeCancel: () => tid('vault-change-cancel'),
  /** One error line for the whole vault card: a rejected passphrase change, a
   *  rejected add, and a failed Drop passphrase all render here. */
  vaultChangeError: () => tid('vault-change-error'),

  /** Settings → Profile → Vault: turns a machine-key vault into a
   *  passphrase one. */
  async addPassphrase(p: string) {
    await settings.open('profile');
    await this.vaultAddPassphrase().click();
    await typeInto('vault-new', p);
    await typeInto('vault-new-confirm', p);
    await this.vaultAddSubmit().click();
    await expect(this.vaultChange()).toBeDisplayed();
    await nav.go('library');
  },

  /** Types a name and saves it. The button carries the state: "Save" while the
   *  draft differs from the stored name, "Saved" once it lands. */
  async setDisplayName(n: string) {
    await typeInto('profile-display-name', n);
    if (await this.save().isEnabled()) await this.save().click();
    await expect(this.save()).toHaveText('Saved');
    await expect(this.displayName()).toHaveValue(n);
  },
  /** Opens the change-passphrase form. */
  async beginPassphraseChange() {
    await this.vaultChange().click();
    await expect(this.vaultCurrent()).toBeDisplayed();
  },
  /** Fills the form with values the form must refuse and submits. Validation
   *  runs on submit, so the rejection shows as an inline error over a form
   *  still holding what was typed. */
  async attemptInvalidPassphraseChange(current: string, next: string, confirm: string, error: string) {
    await typeInto('vault-current', current);
    await typeInto('vault-change-new', next);
    await typeInto('vault-change-confirm', confirm);
    await expect(this.vaultChangeSubmit()).toBeEnabled();
    await this.vaultChangeSubmit().click();
    await expect(this.vaultChangeError()).toHaveText(expect.stringContaining(error));
    await expect(this.vaultChangeSubmit()).toBeDisplayed();
  },
  /** Leaves the vault card on its buttons whether or not the form is open. */
  async ensurePassphraseChangeClosed() {
    if (await this.vaultChangeCancel().isExisting()) await this.vaultChangeCancel().click();
    await expect(this.vaultChange()).toBeDisplayed();
    await expect(this.vaultCurrent()).not.toBeExisting();
  },
};

export const mobileSync = {
  showCode: () => tid('mobile-sync-show-code'),
  cancel: () => tid('mobile-sync-cancel'),
  qr: () => tid('mobile-sync-qr'),
  key: () => tid('mobile-sync-key'),
  warning: () => tid('mobile-sync-warning'),
  expired: () => tid('mobile-sync-expired'),
  showNew: () => tid('mobile-sync-show-new'),
  qrText: () => tid('mobile-sync-qr').getAttribute('data-qr-text'),
  empty: () => tid('mobile-sync-empty'),
  error: () => tid('mobile-sync-error'),
  reach: () => tid('mobile-sync-reach'),
  firewall: () => tid('mobile-sync-firewall'),
  linuxFirewall: () => tid('mobile-sync-linux-firewall'),
  linuxFirewallCommand: () => tid('mobile-sync-linux-firewall-command'),
  linuxFirewallCopy: () => tid('mobile-sync-linux-firewall-copy'),
  rows: () => tids('mobile-sync-phone-'),
  row: (deviceId: string) => tid(`mobile-sync-phone-${deviceId}`),
  menu: (deviceId: string) => tid(`mobile-sync-menu-${deviceId}`),
  pairedAs: (deviceId: string) => tid(`mobile-sync-paired-as-${deviceId}`),
  /** The version-gate line that replaces a device's last-seen line. */
  outOfDate: (deviceId: string) => tid(`mobile-sync-out-of-date-${deviceId}`),
  renameModal: () => tid('mobile-sync-rename-modal'),
  renameBackdrop: () => tid('mobile-sync-rename-backdrop'),
  renameName: () => tid('mobile-sync-rename-name'),
  renameSave: () => tid('mobile-sync-rename-save'),
  renameCancel: () => tid('mobile-sync-rename-cancel'),
  /** Rename sits behind the row's overflow; a blank name is erased key by key. */
  async renameDevice(deviceId: string, name: string) {
    await clickRowMenuItem(`mobile-sync-menu-${deviceId}`, `mobile-sync-rename-${deviceId}`);
    if (name) {
      await typeInto('mobile-sync-rename-name', name);
    } else {
      const field = this.renameName();
      await field.click();
      await browser.keys('End');
      for (let i = (await field.getValue()).length; i > 0; i--) await browser.keys('Backspace');
      await expect(field).toHaveValue('');
    }
    await this.renameSave().click();
    await expect(this.renameModal()).not.toBeExisting();
  },
  /** Unpair sits behind the row's overflow. */
  async unpairDevice(deviceId: string) {
    await clickRowMenuItem(`mobile-sync-menu-${deviceId}`, `mobile-sync-unpair-${deviceId}`);
  },
  refused: () => tid('mobile-sync-refused'),
  refusedLine: (deviceId: string, reason: 'sealed' | 'unsupported') => tid(`mobile-sync-refused-${deviceId}-${reason}`),
};
