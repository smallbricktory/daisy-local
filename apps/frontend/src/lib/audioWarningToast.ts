// Maps a serialized `recording::supervisor::Notice` (the `audio-warning`
// event payload) onto the toast it shows.

import type { AudioWarning } from '../tauri';
import type { ToastSpec } from './toastStore';

export function audioWarningToast(w: AudioWarning): ToastSpec {
  switch (w.kind) {
    case 'mic-failed-over':
      return {
        id: 'audio-mic-failover',
        severity: 'warning',
        title: 'Microphone switched',
        body: `Your mic disappeared; recording continues on “${w.to}”.`,
      };
    case 'mic-muted-externally':
      return {
        id: 'audio-mic-muted',
        severity: 'warning',
        title: 'Mic muted outside Daisy',
        body: 'Your microphone was muted in the system mixer. Unmute it to keep recording your voice.',
      };
    case 'mic-very-quiet':
      // The mic is alive and recorded — its level is just low. Calm,
      // self-clearing; must never read as "not recording".
      return {
        id: 'audio-mic-very-quiet',
        severity: 'info',
        title: 'Mic level is very low',
        body: 'Check your system input volume — another app may have lowered it. Daisy is still recording.',
        autoDismissMs: 12000,
      };
    case 'mic-retrying':
      // The mic produces no audio and no other physical mic exists to
      // switch to. The backend keeps rebuilding it on a slow cadence and
      // recovers on its own once the device is released — the message must
      // say so, not send the user hunting for a fix.
      return {
        id: 'audio-mic-retrying',
        severity: 'warning',
        title: 'Your mic isn’t producing audio',
        body: 'Another app may be holding the microphone. Daisy will keep retrying and resumes recording your voice automatically when the mic is released.',
      };
    case 'mic-dead-air':
      // The stream looks alive but has produced nothing above the device's
      // learned floor. The backend is already rebuilding; recovery replaces
      // this toast via the shared mic-health toast id.
      return {
        id: 'audio-mic-retrying',
        severity: 'warning',
        title: 'Microphone appears silent',
        body: 'Your mic hasn’t produced audible signal for two minutes. Daisy is rebuilding it — check your input device if this persists.',
      };
    case 'mic-held-by-call':
      // Not a fault and not retryable: the mic comes back on its own.
      return {
        id: 'audio-mic-retrying',
        severity: 'info',
        title: 'Another app is using your microphone',
        body: 'Your mic is delivering silence while another app has it. Recording resumes automatically once it is released.',
      };
    case 'mic-recovered':
      return {
        id: 'audio-mic-retrying',
        severity: 'done',
        title: 'Mic recovered',
        body: 'Your microphone is recording again.',
        autoDismissMs: 5000,
      };
    case 'system-audio-lost':
      // Mic capture is unaffected; only the far-end track stopped.
      // Recovered replaces via the shared id.
      return {
        id: 'audio-system-lost',
        severity: 'warning',
        title: 'System audio capture stopped',
        body: 'The other side’s audio isn’t being captured. Your mic keeps recording.',
      };
    case 'system-audio-recovered':
      return {
        id: 'audio-system-lost',
        severity: 'done',
        title: 'System audio is back',
        body: 'System audio capture and echo cancellation resumed.',
        autoDismissMs: 5000,
      };
    case 'reconnect-failed':
      return {
        id: `audio-reconnect-${w.track}`,
        severity: 'error',
        title: w.track === 'System' ? 'System audio capture stopped' : 'Mic capture stopped',
        body: 'Reconnecting failed repeatedly. Pause and resume the recording, or pick a device in the mic selector.',
      };
  }
}
