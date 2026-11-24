import { screen } from '@testing-library/react';
import { describe, expect, it } from 'vitest';
import { modelLabel } from './components/Shell';
import { copy } from './copy';
import { renderApp } from './test/util';

// Slice H12 (docs/openshell-any-host-spec.md §3.6, "The indicator"): where prompts go, on every
// page, by label only.
describe('the model indicator', () => {
  it('names the source by label, never a host', () => {
    expect(modelLabel({ source: 'hosted', name: 'owner-hosted' })).toBe('Model: hosted (owner-hosted)');
    expect(modelLabel({ source: 'local' })).toBe(copy.nav.modelLocal);
    expect(modelLabel({ source: 'hosted-test' })).toBe(copy.nav.modelTest);
    // The test-only list is fake-data mode whatever its source says.
    expect(modelLabel({ source: 'hosted', name: 'stack-test', fake_data: true })).toBe(copy.nav.modelTest);
    expect(modelLabel({ source: 'unknown' })).toBe(copy.nav.modelUnknown);
    expect(modelLabel(undefined)).toBeNull();
  });

  it('shows on the page, with the switch notice while one waits', async () => {
    renderApp({ model: { source: 'hosted', name: 'owner-hosted', switch_pending: true } });
    await screen.findByLabelText(copy.home.askLabel);
    const el = screen.getByTestId('model-source');
    expect(el).toHaveTextContent('Model: hosted (owner-hosted)');
    expect(el).toHaveTextContent(copy.nav.modelSwitchPending);
  });
});
