import React from 'react';
import ReactDOM from 'react-dom/client';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { App } from './App';
import { MiniWindow } from './routes/MiniWindow';
import { ReminderWindow } from './routes/ReminderWindow';
import { hardenWebview } from './harden';
import { GlobalConfirm } from './components/GlobalConfirm';

hardenWebview();

const label = getCurrentWindow().label;
const isMini = label === 'mini';
const isReminder = label === 'reminder';

// Marker classes scope each popup's CSS resets (body sizing etc.) to that
// webview only; unscoped resets leak into the main window (e.g. flipping
// native <select> dropdowns to the OS dark theme).
if (isMini) document.documentElement.classList.add('is-mini');
if (isReminder) document.documentElement.classList.add('is-reminder');

ReactDOM.createRoot(document.getElementById('root')!).render(
  <React.StrictMode>
    {isMini ? <><MiniWindow /><GlobalConfirm /></>
      : isReminder ? <ReminderWindow />
      : <App />}
  </React.StrictMode>,
);
