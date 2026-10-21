// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
// Overrides Tauri commands in the page and records the calls, in either mode.
//
// The service's `browser.tauri.mock()` patches `window.__TAURI__.core.invoke`.
// The app never calls that — it imports `@tauri-apps/api` as a module, which
// reads `window.__TAURI_INTERNALS__.invoke` — and in the native webview
// `__TAURI_INTERNALS__` and its `invoke`, `ipc` and `postMessage` are all
// non-writable and non-configurable, so nothing there can be wrapped.
//
// Two seams remain, one per mode:
//  - `invoke`: the browser-mode injection leaves `__TAURI_INTERNALS__.invoke`
//    a plain writable property.
//  - `fetch`: natively, the custom-protocol IPC posts each command through
//    `window.fetch` (tauri `scripts/ipc-protocol.js`), addressed by
//    `convertFileSrc(cmd, 'ipc')`, with the invoke args as the JSON request
//    body. The reply routes to the caller's resolve or reject by its
//    `Tauri-Response` header (`ok` resolves, anything else rejects) and is
//    decoded by its content type.
import { browser } from '@wdio/globals';

/** Reply builder for a mocked command. Receives the decoded invoke args
 *  (`null` when the request body was not JSON). Returning a value resolves the
 *  caller; throwing rejects it. It is evaluated in the page with no closure, so
 *  it must be self-contained.
 *
 *  The reply must be produced synchronously. On the fetch seam the return value
 *  is JSON-encoded into the IPC response body, and a Promise serializes to
 *  `{}` — the caller would resolve with an empty object instead of waiting.
 *
 *  `mockInvokeWith` calls it once with `null` at install to check it is
 *  closure-free, so it must tolerate that call and leave nothing behind. */
export type InvokeImpl = (args: Record<string, unknown> | null) => unknown;

/** One intercepted call, in invocation order. */
export interface InvokeCall {
  cmd: string;
  args: Record<string, unknown> | null;
}

type Override =
  | { kind: 'value'; value: unknown }
  | { kind: 'error'; value: unknown }
  | { kind: 'fn'; fn: InvokeImpl };

type InvokeFn = (cmd: string, args?: unknown, opts?: unknown) => Promise<unknown>;
type Internals = {
  __TAURI_INTERNALS__?: {
    invoke: InvokeFn;
    convertFileSrc: (path: string, protocol: string) => string;
    __e2eOverrides?: Record<string, Override>;
    /** IPC URL → command, filled at override install so the fetch seam's hot
     *  path is one lookup instead of a convertFileSrc call per override. */
    __e2eUrls?: Record<string, string>;
    __e2eCalls?: InvokeCall[];
    __e2eSeam?: 'fetch' | 'invoke';
  };
};

/** Which interception the page is using. */
export type Seam = 'fetch' | 'invoke';

/**
 * Installs the interception once and reports which seam it took. Throws when
 * neither seam is available, so a mock can never silently no-op.
 */
export async function installInvokeInterceptor(): Promise<Seam> {
  return browser.execute(() => {
    const internals = (window as unknown as Internals).__TAURI_INTERNALS__;
    if (!internals) throw new Error('window.__TAURI_INTERNALS__ missing');
    if (internals.__e2eSeam) return internals.__e2eSeam;

    const overrides: Record<string, Override> = (internals.__e2eOverrides ??= {});
    const calls: InvokeCall[] = (internals.__e2eCalls ??= []);

    // Undefined bodies are the hazard: `JSON.stringify(undefined)` is
    // `undefined`, `new Response(undefined)` has a null body, and tauri's
    // `response.json()` then rejects — which it reads as "custom protocol
    // failed", re-sending the command to the real backend and disabling the
    // fetch seam for the rest of the run.
    const encode = (value: unknown): string => {
      const body = JSON.stringify(value === undefined ? null : value);
      return body === undefined ? 'null' : body;
    };

    /** Applies an override: `[ok, value]`. */
    const apply = (o: Override, args: Record<string, unknown> | null): [boolean, unknown] => {
      if (o.kind === 'error') return [false, o.value];
      if (o.kind === 'value') return [true, o.value];
      try {
        return [true, o.fn(args)];
      } catch (e) {
        return [false, e instanceof Error ? e.message : e];
      }
    };

    const desc = Object.getOwnPropertyDescriptor(internals, 'invoke');
    if (!desc || desc.writable || desc.configurable) {
      const real = internals.invoke.bind(internals);
      internals.invoke = (cmd: string, args?: unknown, opts?: unknown) => {
        const o = overrides[cmd];
        if (!o) return real(cmd, args, opts);
        const decoded = (args ?? null) as Record<string, unknown> | null;
        calls.push({ cmd, args: decoded });
        const [ok, value] = apply(o, decoded);
        return ok
          ? Promise.resolve(value === undefined ? null : value)
          : Promise.reject(value);
      };
      internals.__e2eSeam = 'invoke';
      return internals.__e2eSeam;
    }

    if (typeof internals.convertFileSrc !== 'function' || typeof window.fetch !== 'function') {
      throw new Error('__TAURI_INTERNALS__.invoke is sealed and the fetch seam is unavailable');
    }
    const urls: Record<string, string> = (internals.__e2eUrls ??= {});
    const realFetch = window.fetch.bind(window);
    window.fetch = (input: RequestInfo | URL, init?: RequestInit) => {
      const url = typeof input === 'string' ? input
        : input instanceof URL ? input.href
          : (input as Request).url;
      const cmd = urls[url];
      if (cmd === undefined || !overrides[cmd]) return realFetch(input, init);
      // `scripts/process-ipc-message-fn.js` sends a plain-object payload as
      // a JSON string body; binary payloads go out as octet-stream.
      let args: Record<string, unknown> | null = null;
      const body = init?.body;
      if (typeof body === 'string') {
        try { args = JSON.parse(body); } catch { args = null; }
      }
      calls.push({ cmd, args });
      const [ok, value] = apply(overrides[cmd], args);
      return Promise.resolve(new Response(encode(value), {
        headers: { 'content-type': 'application/json', 'Tauri-Response': ok ? 'ok' : 'error' },
      }));
    };
    internals.__e2eSeam = 'fetch';
    return internals.__e2eSeam;
  });
}

/** Page-side assignment of one override. Serializable kinds only — `fn` goes
 *  through `mockInvokeWith`. */
async function setOverride(cmd: string, o: Exclude<Override, { kind: 'fn' }>): Promise<void> {
  await installInvokeInterceptor();
  await browser.execute((c: string, spec: Override) => {
    const internals = (window as unknown as Internals).__TAURI_INTERNALS__!;
    internals.__e2eOverrides![c] = spec;
    if (typeof internals.convertFileSrc === 'function') {
      (internals.__e2eUrls ??= {})[internals.convertFileSrc(c, 'ipc')] = c;
    }
  }, cmd, o);
}

/** Resolves `cmd` with `value` until `restoreInvoke(cmd)`. A cancelled file
 *  picker is `null`; `undefined` is coerced to it. */
export async function mockInvoke(cmd: string, value: unknown): Promise<void> {
  await setOverride(cmd, { kind: 'value', value: value === undefined ? null : value });
}

/** Resolves `cmd` from its args. `impl` is evaluated in the page, so it must
 *  reference nothing from the spec's scope. Throwing from it rejects the
 *  caller. */
export async function mockInvokeWith(cmd: string, impl: InvokeImpl): Promise<void> {
  await installInvokeInterceptor();
  // The page's CSP has no 'unsafe-eval', so the function cannot be rebuilt
  // from a string in-page. The driver compiles this script itself, which CSP
  // does not police.
  await browser.execute(
    `window.__TAURI_INTERNALS__.__e2eOverrides[${JSON.stringify(cmd)}]`
    + ` = { kind: 'fn', fn: (${String(impl)}) };`
    + ` if (typeof window.__TAURI_INTERNALS__.convertFileSrc === 'function') {`
    + ` (window.__TAURI_INTERNALS__.__e2eUrls ??= {})`
    + `[window.__TAURI_INTERNALS__.convertFileSrc(${JSON.stringify(cmd)}, 'ipc')]`
    + ` = ${JSON.stringify(cmd)}; }`,
  );
  // A leaked closure reference only shows up when the command fires, where the
  // ReferenceError is swallowed into a rejected invoke. Probing here turns it
  // into a failure at the mock site. An impl that forwards to a real world
  // handler (to keep the world owning the state while still counting calls)
  // mutates `window.__e2e_world.data` as a side effect of that forwarding —
  // the probe snapshots and restores it so the check stays invisible.
  const name = JSON.stringify(cmd);
  await browser.execute(
    `(() => {`
    + ` const w = window.__e2e_world;`
    + ` const snapshot = w ? JSON.parse(JSON.stringify(w.data)) : undefined;`
    + ` try { window.__TAURI_INTERNALS__.__e2eOverrides[${name}].fn(null); }`
    + ` catch (e) {`
    + `  if (e instanceof ReferenceError) throw new Error('mockInvokeWith(' + ${name} + ') impl is not closure-free: ' + e.message);`
    + ` }`
    + ` finally { if (snapshot !== undefined) Object.assign(w.data, snapshot); }`
    + `})();`,
  );
}

/** Rejects `cmd` with `error`. The real backend rejects with its serialized
 *  AppError (`{ kind, code, message, friendly }`), so mirror that shape when
 *  the UI reads fields off it. */
export async function mockInvokeReject(cmd: string, error: unknown): Promise<void> {
  await setOverride(cmd, { kind: 'error', value: error === undefined ? null : error });
}

/** Hands `cmd` back to the real backend. Recorded calls are kept. */
export async function restoreInvoke(cmd: string): Promise<void> {
  await browser.execute((c: string) => {
    const internals = (window as unknown as Internals).__TAURI_INTERNALS__;
    if (internals?.__e2eOverrides) delete internals.__e2eOverrides[c];
  }, cmd);
}

/** The args of every intercepted `cmd` call, in order. Its length is the call
 *  count. Only mocked commands are recorded. */
export async function callsOf(cmd: string): Promise<(Record<string, unknown> | null)[]> {
  return browser.execute((c: string) => {
    const internals = (window as unknown as Internals).__TAURI_INTERNALS__;
    return (internals?.__e2eCalls ?? []).filter((k) => k.cmd === c).map((k) => k.args);
  }, cmd);
}

/** Drops the call log, keeping the overrides in place. */
export async function clearCalls(): Promise<void> {
  await browser.execute(() => {
    const internals = (window as unknown as Internals).__TAURI_INTERNALS__;
    if (internals?.__e2eCalls) internals.__e2eCalls.length = 0;
  });
}
