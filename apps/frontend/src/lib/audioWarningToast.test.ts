import { describe, it, expect } from 'vitest';
import { audioWarningToast } from './audioWarningToast';

describe('audioWarningToast', () => {
  it('mic-very-quiet is a calm, self-dismissing toast that never claims not-recording', () => {
    const t = audioWarningToast({ kind: 'mic-very-quiet' });
    expect(t.id).toBe('audio-mic-very-quiet');
    expect(t.severity).toBe('info');
    expect(t.autoDismissMs).toBeGreaterThan(0);
    expect(t.body).toContain('still recording');
    expect(`${t.title} ${t.body}`).not.toMatch(/isn.t being recorded|not record/i);
  });

  it('mic-retrying warns but promises automatic recovery', () => {
    const t = audioWarningToast({ kind: 'mic-retrying' });
    expect(t.id).toBe('audio-mic-retrying');
    expect(t.severity).toBe('warning');
    expect(t.body).toMatch(/keep retrying/i);
    expect(t.body).toMatch(/automatically/i);
    // Self-healing case: the user must not be told to switch devices.
    expect(t.body).not.toMatch(/pick a device|pause and resume/i);
  });

  it('reconnect-failed stays a sticky error per track', () => {
    const t = audioWarningToast({ kind: 'reconnect-failed', track: 'Mic' });
    expect(t.id).toBe('audio-reconnect-Mic');
    expect(t.severity).toBe('error');
    expect(t.autoDismissMs).toBeUndefined();
    const s = audioWarningToast({ kind: 'reconnect-failed', track: 'System' });
    expect(s.title).toContain('System audio');
  });

  it('failover and external mute keep their warning shape', () => {
    const f = audioWarningToast({ kind: 'mic-failed-over', to: 'USB Mic', id: 7 });
    expect(f.severity).toBe('warning');
    expect(f.body).toContain('USB Mic');
    const m = audioWarningToast({ kind: 'mic-muted-externally' });
    expect(m.id).toBe('audio-mic-muted');
    expect(m.severity).toBe('warning');
  });
});
