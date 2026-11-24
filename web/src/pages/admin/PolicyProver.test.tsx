import { screen, within } from '@testing-library/react';
import { describe, expect, it } from 'vitest';
import type { ProverRow } from '../../api';
import { copy } from '../../copy';
import { renderApp } from '../../test/util';
import { proverLine } from './Policy';

// OpenShell's prover in Policy Admin (docs/openshell-prover-and-sandbox-audit-spec.md §A.5,
// PRV-10): "Proved" only for a current proof; every other state says what it is; what a proof
// never covers is always listed; validate and proposals say the prover doesn't run here.

const B = copy.admin.policy.bots;
const P = B.prover;
const ALL = ['filesystem', 'landlock', 'network_l4', 'network_rest', 'process'];

const row = (state: ProverRow['state'], extra: Partial<ProverRow> = {}): ProverRow => ({ state, passed: state === 'proved', ...extra });

describe('the prover line', () => {
  it('reads as proved only for a current proof', () => {
    const proved = proverLine(row('proved', { coverage: ALL, prover_version: '0.1.9', at: '2026-11-24T09:00:00Z' }));
    expect(proved.text).toBe(P.proved(5, 5));
    expect(proved.tone).toBe('good');
    for (const state of ['stale', 'exceeds', 'unsupported', 'inconclusive', 'error', 'not_available', 'version_mismatch', 'missing', 'unknown'] as const) {
      const line = proverLine(row(state, { coverage: ALL, reason: 'why', counterexample: 'filesystem write /sandbox' }));
      expect(line.text, state).not.toMatch(/^Proved/);
      expect(line.tone, state).not.toBe('good');
    }
    // A row that never came (an older server) is not a pass either.
    expect(proverLine(undefined).text).toBe(P.unknown);
  });

  it('names each failure as what it is', () => {
    expect(proverLine(row('exceeds', { counterexample: 'filesystem write /sandbox' })).text).toBe(P.exceeds('filesystem write /sandbox'));
    expect(proverLine(row('exceeds')).tone).toBe('bad');
    expect(proverLine(row('unsupported', { reason: 'the policy uses MCP rules' })).text).toBe(P.unsupported('the policy uses MCP rules'));
    expect(proverLine(row('inconclusive')).text).toBe(P.inconclusive);
    expect(proverLine(row('error')).text).toBe(P.error);
    expect(proverLine(row('not_available')).text).toBe(P.notAvailable);
    expect(proverLine(row('version_mismatch', { reason: 'found 0.1.8 (too old), pinned 0.1.9' })).text).toBe(P.versionMismatch('found 0.1.8 (too old), pinned 0.1.9'));
    expect(proverLine(row('stale', { stale_reason: 'policy_changed' })).sub).toBe(P.staleChanged);
    expect(proverLine(row('stale', { stale_reason: 'policy_loaded_after' })).sub).toBe(P.staleLoaded);
    expect(proverLine(row('missing')).text).toBe(P.missing);
  });
});

describe('the Prover row on each bot card', () => {
  it('shows each bot its own state, never "Proved" for a stale proof, and what is never proved', async () => {
    renderApp({ role: 'auditor', hash: '/admin/policy' });
    await screen.findByRole('heading', { name: new RegExp(`^${B.heading}`) });
    const cards = [...document.querySelectorAll<HTMLElement>('.bot-card')];
    const card = (name: string) => cards.find((c) => within(c).queryByRole('heading', { name }))!;
    const prover = (name: string) => card(name).querySelector<HTMLElement>('.bot-prover')!;

    expect(prover('docs').dataset.state).toBe('proved');
    expect(within(prover('docs')).getByText(P.proved(5, 5))).toBeInTheDocument();
    expect(within(prover('docs')).getByText(P.viewNote)).toBeInTheDocument();

    expect(prover('pm-ana').dataset.state).toBe('stale');
    expect(within(prover('pm-ana')).getByText(P.stale)).toBeInTheDocument();
    expect(within(prover('pm-ana')).queryByText(/^Proved/)).not.toBeInTheDocument();
    expect(within(prover('pm-ana')).queryByText(P.viewNote)).not.toBeInTheDocument();

    expect(within(prover('pm-ben')).getByText(P.exceeds('network_l4 api.example.com:443'))).toBeInTheDocument();
    expect(prover('pm-ben').querySelector('.pill-bad')).not.toBeNull();
    expect(within(prover('triage')).getByText(P.notAvailable)).toBeInTheDocument();

    for (const name of ['docs', 'pm-ana', 'pm-ben', 'triage']) {
      const list = within(prover(name)).getByRole('list', { name: `${P.notProvedHeading}, ${name}` });
      expect(within(list).getAllByRole('listitem')).toHaveLength(6);
    }
  });

  it('says on proposals that the prover runs on the pull request, not here', async () => {
    renderApp({ role: 'admin', hash: '/admin/policy/proposals' });
    expect(await screen.findAllByText(P.notRunHere)).not.toHaveLength(0);
  });
});
