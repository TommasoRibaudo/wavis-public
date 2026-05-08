import React from 'react';
import ReactDOM from 'react-dom/client';
import App from './App';
import '@shared/ring-buffer';
import './styles/index.css';
import { loadShootoutEnv } from './features/voice/shootout-bridge';

void loadShootoutEnv();

// Initialize the LiveKit transceiver reuse patch WeakMap early
// This is created by the patched livekit-client bundle and needs to exist
// before any screen sharing attempts
if (!(globalThis as { __wavisSenderData?: unknown }).__wavisSenderData) {
  (globalThis as { __wavisSenderData?: WeakMap<unknown, unknown> }).__wavisSenderData = new WeakMap();
}

ReactDOM.createRoot(document.getElementById('root')!).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
