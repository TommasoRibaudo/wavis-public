import React from 'react';
import ReactDOM from 'react-dom/client';
import App from './App';
import '@shared/ring-buffer';
import './styles/index.css';
import { loadShootoutEnv } from './features/voice/shootout-bridge';

void loadShootoutEnv();

ReactDOM.createRoot(document.getElementById('root')!).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
