import '@testing-library/jest-dom/vitest';
import { afterEach } from 'vitest';

afterEach(() => {
  // Node-environment tests (the build test) have no window.
  if (typeof window !== 'undefined') window.location.hash = '';
});
