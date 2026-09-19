// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import '@testing-library/jest-dom';
// Vitest globals (describe, it, expect) are enabled by the config.

// jsdom implements no layout, so it ships no scrollIntoView; a component that
// keeps its active row in view calls one.
if (typeof Element !== 'undefined' && !Element.prototype.scrollIntoView) {
  Element.prototype.scrollIntoView = () => {};
}

// Same reason: no layout means no ResizeObserver, and a component that
// publishes its own measured height observes itself.
if (typeof globalThis.ResizeObserver === 'undefined') {
  globalThis.ResizeObserver = class {
    observe() {}
    unobserve() {}
    disconnect() {}
  } as unknown as typeof ResizeObserver;
}
