import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, waitFor, cleanup } from '@testing-library/react';
import { MicLevel } from './MicLevel';

// Lifecycle spies for the cleanup assertions.
const closed = vi.fn();
const stopped = vi.fn();

class FakeAudioContext {
  state = 'running';
  createMediaStreamSource() {
    return { connect: vi.fn() };
  }
  createAnalyser() {
    return {
      fftSize: 0,
      frequencyBinCount: 1024,
      // Component uses getFloatTimeDomainData (Float32Array).
      getFloatTimeDomainData: (buf: Float32Array) => { buf.fill(0); },
    };
  }
  close = vi.fn(() => { closed(); return Promise.resolve(); });
}

const fakeTrack = { stop: stopped, kind: 'audio' };
const fakeStream = { getTracks: () => [fakeTrack] };

beforeEach(() => {
  closed.mockClear();
  stopped.mockClear();
  // @ts-expect-error — test stub
  globalThis.AudioContext = FakeAudioContext;
  globalThis.requestAnimationFrame = (cb: FrameRequestCallback) => {
    setTimeout(() => cb(0), 0);
    return 0 as any;
  };
  Object.defineProperty(navigator, 'mediaDevices', {
    configurable: true,
    value: {
      getUserMedia: vi.fn().mockResolvedValue(fakeStream),
      enumerateDevices: vi.fn().mockResolvedValue([]),
    },
  });
});

afterEach(() => { cleanup(); });

describe('<MicLevel />', () => {
  it('renders a placeholder when permission denied', async () => {
    (navigator.mediaDevices.getUserMedia as ReturnType<typeof vi.fn>).mockRejectedValueOnce(
      new Error('NotAllowedError'),
    );
    // A concrete deviceId is required for the Web Audio path to open a
    // capture; without one the component no-ops (never grabs the default
    // mic).
    const { findByText } = render(<MicLevel deviceId="mic-1" />);
    expect(await findByText('—')).toBeInTheDocument();
  });

  it('mounts a level bar when getUserMedia succeeds', async () => {
    const { container } = render(<MicLevel deviceId="mic-1" />);
    await waitFor(() => {
      const fill = container.querySelector('[data-testid="mic-level-fill"]');
      expect(fill).toBeTruthy();
    });
  });

  it('closes the AudioContext and stops tracks on unmount', async () => {
    const { unmount } = render(<MicLevel deviceId="mic-1" />);
    await waitFor(() =>
      expect(navigator.mediaDevices.getUserMedia).toHaveBeenCalled(),
    );
    // Give the async open() a chance to finish wiring up refs.
    await waitFor(() => {
      const fill = document.querySelector('[data-testid="mic-level-fill"]');
      expect(fill).toBeTruthy();
    });
    unmount();
    expect(stopped).toHaveBeenCalled();
    expect(closed).toHaveBeenCalled();
  });

  it('re-opens the stream when deviceId prop changes', async () => {
    const { rerender } = render(<MicLevel deviceId="device-a" />);
    await waitFor(() =>
      expect(navigator.mediaDevices.getUserMedia).toHaveBeenCalledWith(
        expect.objectContaining({ audio: { deviceId: { exact: 'device-a' } } }),
      ),
    );
    rerender(<MicLevel deviceId="device-b" />);
    await waitFor(() =>
      expect(navigator.mediaDevices.getUserMedia).toHaveBeenCalledWith(
        expect.objectContaining({ audio: { deviceId: { exact: 'device-b' } } }),
      ),
    );
    expect(navigator.mediaDevices.getUserMedia).toHaveBeenCalledTimes(2);
  });
});
