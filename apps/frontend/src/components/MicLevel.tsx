/**
 * MicLevel — microphone level meter.
 *
 * Two data sources, picked by host:
 *
 *   * **Linux (WebKitGTK):** backend-driven. Opens a PipeWire capture on the
 *     Rust side (same path the recorder uses) and subscribes to
 *     `mic-level:<request-id>` events. WebKitGTK's `getUserMedia` silently
 *     returns zero-amplitude streams on many hosts.
 *
 *   * **Windows (WebView2 / Chromium):** Web Audio. `getUserMedia` works
 *     reliably here. When `tauriSourceId` is given but no `deviceId`, the
 *     Web Audio deviceId is resolved from the Tauri source's description.
 *
 * Color zones based on level:
 *   0–70%   green  (normal)
 *   70–90%  yellow (caution)
 *   >90%    red    (clip)
 *
 * On permission-denied or no-device the component renders a small "—" instead
 * of crashing.
 */

import { useEffect, useRef, useState } from 'react';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { tauri } from '../tauri';
import { isChromiumWebview } from '../lib/webview';

interface Props {
  /** Numeric Tauri AudioSourceInfo id. When set, the Rust-side meter is used
   *  and `deviceId` is ignored. */
  tauriSourceId?: number | null;
  /** Web Audio deviceId string — comes from MediaDeviceInfo.deviceId.
   *  Omit (or pass undefined) to let the browser choose the default mic.
   *  Only consulted when `tauriSourceId` is not set. */
  deviceId?: string;
  /** Width of the track in px. Defaults to 120. */
  width?: number;
  /** Height of the track in px. Defaults to 6. */
  height?: number;
  /** Externally-driven level (0..1). When set, the component renders this
   *  value and does not open any capture (no Tauri meter, no getUserMedia).
   *  Used during recording, fed by the `recording:mic-level` event; the
   *  in-call bar reflects the recording's own (mute-aware) mic. */
  controlledLevel?: number;
}

export function MicLevel({ tauriSourceId, deviceId, width = 120, height = 6, controlledLevel }: Props) {
  const [level, setLevel] = useState(0);          // 0..1 normalised RMS
  const [failed, setFailed] = useState(false);
  // Refs; the cleanup closure sees the current objects.
  const ctxRef = useRef<AudioContext | null>(null);
  const streamRef = useRef<MediaStream | null>(null);
  const rafRef = useRef<number | null>(null);
  const analyserRef = useRef<AnalyserNode | null>(null);

  const isChromium = isChromiumWebview();
  // Resolved Web Audio deviceId: the caller's value, or — on WebView2 with
  // only a tauriSourceId — derived from the source's description by matching
  // against MediaDeviceInfo.label.
  const [resolvedDeviceId, setResolvedDeviceId] = useState<string | undefined>(deviceId);

  // On WebView2 + numeric tauriSourceId without an explicit deviceId, looks
  // up the Tauri source by id, then matches its description against
  // navigator.mediaDevices labels. Falls back to the browser default mic
  // when there is no match.
  useEffect(() => {
    if (controlledLevel !== undefined) return; // externally driven; no capture
    if (!isChromium) return;
    if (deviceId !== undefined) { setResolvedDeviceId(deviceId); return; }
    if (tauriSourceId == null) { setResolvedDeviceId(undefined); return; }
    let cancelled = false;
    (async () => {
      try {
        const [sources, devices] = await Promise.all([
          tauri.listAudioSources(),
          navigator.mediaDevices.enumerateDevices(),
        ]);
        if (cancelled) return;
        const src = sources.find((s) => s.id === tauriSourceId);
        if (!src) { setResolvedDeviceId(undefined); return; }
        const audio = devices.filter((d) => d.kind === 'audioinput');
        const exact = audio.find((d) => d.label === src.description);
        const partial = exact ?? audio.find(
          (d) => d.label.includes(src.description) || src.description.includes(d.label),
        );
        setResolvedDeviceId(partial?.deviceId);
      } catch {
        if (!cancelled) setResolvedDeviceId(undefined);
      }
    })();
    return () => { cancelled = true; };
  }, [isChromium, tauriSourceId, deviceId]);

  // Backend-driven path: opens a Rust capture (PipeWire on Linux, AVAudioEngine
  // on macOS) and listens for `mic-level:<id>` events. Only used on
  // WebKitGTK/WKWebView where getUserMedia is unreliable. Chromium hosts skip
  // this entirely.
  //
  // `start_mic_meter` is fire-and-forget — it spawns the meter thread and
  // returns Ok immediately; a start failure (device contended by an active
  // recording → rc=-1, stale/absent device id → rc=-10851) never surfaces
  // via the invoke. The backend emits `mic-meter-error:<id>` on failure,
  // which triggers a retry here.
  useEffect(() => {
    if (controlledLevel !== undefined) return; // externally driven; no capture
    if (isChromium) return;
    if (tauriSourceId == null) return;
    // Captures the narrowed (non-null) id; TS can't carry the null-guard
    // narrowing into deferred closures (setTimeout retry, the async invoke).
    const sourceId = tauriSourceId;
    // Fresh attempt for this source — clear any latched failure from a prior id.
    setFailed(false);
    setLevel(0);

    const requestId = `mic-meter-${sourceId}-${Math.random().toString(36).slice(2, 10)}`;
    // After this many fast retries the component shows "—" and keeps
    // slow-probing; the meter recovers once the recording releases the mic
    // or the device (e.g. AirPods) reconnects.
    const FAST_RETRIES = 3;
    let cancelled = false;
    let unlistenLevel: UnlistenFn | null = null;
    let unlistenErr: UnlistenFn | null = null;
    let retryTimer: ReturnType<typeof setTimeout> | null = null;
    let attempts = 0;

    function scheduleRetry() {
      if (cancelled) return;
      attempts += 1;
      if (attempts >= FAST_RETRIES) setFailed(true);
      // 1s, 2s, 4s for the fast retries, then a steady 10s background re-probe.
      const delay = attempts < FAST_RETRIES ? 1000 * 2 ** (attempts - 1) : 10_000;
      retryTimer = setTimeout(() => {
        retryTimer = null;
        void tauri.startMicMeter(requestId, sourceId).catch(() => {
          if (!cancelled) scheduleRetry();
        });
      }, delay);
    }

    (async () => {
      try {
        unlistenLevel = await listen<{ request_id: string; rms: number }>(
          `mic-level:${requestId}`,
          (e) => {
            if (cancelled) return;
            // Data arriving = the meter is live: clears any failure state
            // and resets the retry counter.
            attempts = 0;
            setFailed(false);
            // Backend reports peak amplitude (0..1, full-scale int16 = 1.0)
            // over the most-recent ~10ms audio buffer. A sqrt curve maps
            // normal-voice peaks (raw ~0.05–0.2) to 22–45% on the bar. The
            // field is named `rms` but carries the peak value.
            const raw = e.payload.rms;
            setLevel(Math.min(1, Math.sqrt(raw) * 1.6));
          },
        );
        unlistenErr = await listen<string>(`mic-meter-error:${requestId}`, () => {
          if (cancelled) return;
          scheduleRetry();
        });
        if (cancelled) return;
        await tauri.startMicMeter(requestId, sourceId);
      } catch {
        // Transport-level failure setting up the listeners / invoke. Treated
        // like a meter error; the retry path kicks in.
        if (!cancelled) scheduleRetry();
      }
    })();

    return () => {
      cancelled = true;
      if (retryTimer) clearTimeout(retryTimer);
      if (unlistenLevel) unlistenLevel();
      if (unlistenErr) unlistenErr();
      // Best-effort: tell the backend to release the capture stream.
      tauri.stopMicMeter(requestId).catch(() => { /* ok */ });
    };
  }, [isChromium, tauriSourceId]);

  // Web Audio path: used on Chromium hosts (Windows WebView2 reliably hands
  // back real audio data), and as the fallback when no tauriSourceId is set.
  useEffect(() => {
    if (controlledLevel !== undefined) return; // externally driven; no capture
    if (!isChromium && tauriSourceId != null) return;
    let cancelled = false;

    const effectiveDeviceId = isChromium ? resolvedDeviceId : deviceId;

    // No concrete device → flat meter, no capture. `audio: true` opens the
    // system default mic and lights the OS mic indicator.
    if (effectiveDeviceId === undefined) { setLevel(0); return; }

    async function open() {
      try {
        const constraints: MediaStreamConstraints = {
          audio: effectiveDeviceId ? { deviceId: { exact: effectiveDeviceId } } : true,
          video: false,
        };
        const stream = await navigator.mediaDevices.getUserMedia(constraints);
        if (cancelled) {
          stream.getTracks().forEach((t) => t.stop());
          return;
        }
        streamRef.current = stream;

        const ctx = new AudioContext();
        ctxRef.current = ctx;

        const source = ctx.createMediaStreamSource(stream);
        const analyser = ctx.createAnalyser();
        analyser.fftSize = 256;
        analyserRef.current = analyser;
        source.connect(analyser);

        const buf = new Float32Array(analyser.fftSize);

        function tick() {
          if (cancelled) return;
          analyser.getFloatTimeDomainData(buf);
          // RMS over the frame
          let sumSq = 0;
          for (let i = 0; i < buf.length; i++) sumSq += buf[i] * buf[i];
          const rms = Math.sqrt(sumSq / buf.length);
          // Normalise: typical voice peaks around 0.1–0.3.
          // Scale: 0.15 → roughly 50% fill.
          const normalised = Math.min(1, rms * 6);
          setLevel(normalised);
          rafRef.current = requestAnimationFrame(tick);
        }
        rafRef.current = requestAnimationFrame(tick);
      } catch {
        if (!cancelled) setFailed(true);
      }
    }

    void open();

    return () => {
      cancelled = true;
      if (rafRef.current !== null) {
        cancelAnimationFrame(rafRef.current);
        rafRef.current = null;
      }
      // Stop all tracks — releases the mic indicator in the OS
      streamRef.current?.getTracks().forEach((t) => t.stop());
      streamRef.current = null;
      // Close the AudioContext (releases the audio thread)
      ctxRef.current?.close().catch(() => { /* best effort */ });
      ctxRef.current = null;
      analyserRef.current = null;
    };
  }, [isChromium, tauriSourceId, deviceId, resolvedDeviceId]);

  if (failed && controlledLevel === undefined) {
    return (
      <span
        title="Mic level unavailable"
        style={{ fontSize: 11, color: 'var(--muted)', lineHeight: 1 }}
      >
        —
      </span>
    );
  }

  // Color zones: green ≤70%, yellow ≤90%, red >90%
  const effective = controlledLevel !== undefined
    ? Math.min(1, Math.max(0, controlledLevel))
    : level;
  const pct = effective * 100;
  const fillColor = pct > 90 ? '#e53e3e' : pct > 70 ? '#d69e2e' : '#38a169';

  return (
    <span
      role="meter"
      aria-label="Microphone level"
      aria-valuenow={Math.round(pct)}
      aria-valuemin={0}
      aria-valuemax={100}
      style={{
        display: 'inline-block',
        width,
        height,
        background: 'var(--frost-deep)',
        borderRadius: height / 2,
        overflow: 'hidden',
        verticalAlign: 'middle',
        flexShrink: 0,
      }}
    >
      <span
        data-testid="mic-level-fill"
        style={{
          display: 'block',
          width: `${pct}%`,
          height: '100%',
          background: fillColor,
          borderRadius: height / 2,
          transition: 'width 60ms linear',
        }}
      />
    </span>
  );
}
