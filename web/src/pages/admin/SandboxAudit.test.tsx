import { render, screen, within } from '@testing-library/react';
import { describe, expect, it } from 'vitest';
import { copy } from '../../copy';
import { renderApp } from '../../test/util';
import { SandboxDetails } from './AuditLog';
import { denialDest } from './Policy';

// OpenShell's sandbox decisions in the web app (docs/openshell-prover-and-sandbox-audit-spec.md
// §B.5, §B.7): the overview's 24-hour denials and the collector's state, each bot's latest
// denials on Policy Admin, and a sandbox event's own columns in the audit log, shown as the tool
// server projected them (an auditor's carry a destination class, never the host or path).

const O = copy.admin.overview.sandbox;
const P = copy.admin.policy.bots.prover;

describe('the overview', () => {
  it('shows the collector and the denials of the last 24 hours by bot', async () => {
    renderApp({ role: 'admin', hash: '/admin' });
    const card = (await screen.findByRole('heading', { name: O.title })).closest('section')!;
    expect(within(card).getByText(O.receiving)).toBeInTheDocument();
    expect(within(card).getByText(O.bot('pm-ben', 3))).toBeInTheDocument();
  });
});

describe('Policy Admin', () => {
  it("lists a bot's latest denials with rule, destination and time, and says when there are none", async () => {
    renderApp({ role: 'admin', hash: '/admin/policy' });
    const list = await screen.findByRole('list', { name: `${P.denialsLabel}, pm-ben` });
    expect(within(list).getByText(P.denialLine('-', 'api.example.com:443'))).toBeInTheDocument();
    expect(within(list).getByText(P.denialSummary)).toBeInTheDocument();
    const docs = [...document.querySelectorAll<HTMLElement>('.bot-card')].find((c) => within(c).queryByRole('heading', { name: 'docs' }))!;
    expect(within(docs).getByText(P.denialsNone)).toBeInTheDocument();
  });

  it('names the destination as the viewer may see it', () => {
    expect(denialDest({ at: '', action: 'sandbox.deny', detail: null, task_id: null, rule: '-', reason: null, dest_class: 'public', dest_host: 'api.example.com', dest_port: 443, http_method: 'GET', http_path: '/v1/x' })).toBe(
      'api.example.com:443 (GET /v1/x)',
    );
    // An auditor's row: no host, no path.
    expect(denialDest({ at: '', action: 'sandbox.deny', detail: null, task_id: null, rule: '-', reason: null, dest_class: 'private', dest_port: 5432 })).toBe('private:5432');
  });
});

describe('the audit log', () => {
  it("shows a sandbox event's columns, and for an auditor's projection only the class and port", () => {
    const { unmount } = render(<SandboxDetails s={{ sandbox: 'pmos-docs', decision: 'deny', rule: '-', reason: 'no_matching_policy', dest_class: 'public', dest_host: 'httpbin.org', dest_port: 443, http_method: 'GET', http_path: '/get', process: '/usr/bin/curl' }} />);
    expect(screen.getByText('httpbin.org:443')).toBeInTheDocument();
    expect(screen.getByText('GET /get')).toBeInTheDocument();
    expect(screen.getByText('/usr/bin/curl')).toBeInTheDocument();
    unmount();
    render(<SandboxDetails s={{ decision: 'deny', rule: '-', reason: 'no_matching_policy', dest_class: 'public', dest_port: 443, sandbox: 'pmos-docs' }} />);
    expect(screen.getByText('public')).toBeInTheDocument();
    expect(screen.getByText(':443')).toBeInTheDocument();
    expect(screen.queryByText(/httpbin/)).not.toBeInTheDocument();
  });
});
