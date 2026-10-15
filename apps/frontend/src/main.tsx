// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import React from 'react';
import ReactDOM from 'react-dom/client';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { App } from './App';
import { MiniWindow } from './routes/MiniWindow';
import { ReminderWindow } from './routes/ReminderWindow';
import { hardenWebview } from './harden';
import { initAppearance } from './lib/appearance';
import { GlobalConfirm } from './components/GlobalConfirm';

hardenWebview();

// e2e bridge; the branch and its chunk are absent from shipping bundles.
if (import.meta.env.VITE_UI_TEST === '1') await import('@wdio/tauri-plugin');

const label = getCurrentWindow().label;
const isMini = label === 'mini';
const isReminder = label === 'reminder';

// Marker classes scope each popup's CSS resets (body sizing etc.) to that
// webview only; unscoped resets leak into the main window.
if (isMini) document.documentElement.classList.add('is-mini');
if (isReminder) document.documentElement.classList.add('is-reminder');

// Every webview paints itself: the mini and reminder popups are separate
// windows and never mount <App>, so theming from there would leave them
// light on a dark desktop.
void initAppearance();

ReactDOM.createRoot(document.getElementById('root')!).render(
  <React.StrictMode>
    {isMini ? <><MiniWindow /><GlobalConfirm /></>
      : isReminder ? <ReminderWindow />
      : <App />}
  </React.StrictMode>,
);
