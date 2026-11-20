import '@testing-library/jest-dom/vitest';
import { configure } from '@testing-library/react';
import { afterEach } from 'vitest';

// The Admin, Reviews, setup and security pages are lazy-loaded, so the first test in a file that
// opens one waits for a real dynamic import. On a busy CI runner that import has taken longer
// than Testing Library's default 1 second, failing `findBy…` while the page was still loading
// (CI run 44780459497). Tests that pass are no slower; this only lengthens how long a query
// keeps looking before it gives up.
configure({ asyncUtilTimeout: 5000 });

afterEach(() => {
  // Node-environment tests (the build test) have no window.
  if (typeof window !== 'undefined') window.location.hash = '';
});
