import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import { resolveApiFactory } from './api';
import { App } from './App';
import './styles.css';

void resolveApiFactory().then((createApi) => {
  createRoot(document.getElementById('root')!).render(
    <StrictMode>
      <App createApi={createApi} />
    </StrictMode>,
  );
});
