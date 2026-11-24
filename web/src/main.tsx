import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import { resolveApiFactory } from './api';
import { App } from './App';
// Geist and Geist Mono, served from this origin (the content security policy allows no font host).
import '@fontsource-variable/geist';
import '@fontsource-variable/geist-mono';
import './styles.css';
import { applyTheme, readTheme } from './theme';

// Apply the saved theme before the first paint, so a dark choice never flashes light.
applyTheme(readTheme());

void resolveApiFactory().then((createApi) => {
  createRoot(document.getElementById('root')!).render(
    <StrictMode>
      <App createApi={createApi} />
    </StrictMode>,
  );
});
