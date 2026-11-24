import { render, screen, within } from '@testing-library/react';
import { describe, expect, it } from 'vitest';
import type { McpServerView } from '../../api';
import { copy } from '../../copy';
import { renderApp } from '../../test/util';
import { builtInSystems, mcpCheck, McpServerCard, McpSection, shortDigest } from './McpServers';

// Policy Admin's MCP servers (docs/mcp-servers-spec.md §5): read-only, the same metadata for an
// Admin and an Auditor; every state says what it is in words, never by colour alone.

const M = copy.admin.policy.mcp;

const server = (extra: Partial<McpServerView> = {}): McpServerView => ({
  id: 'test-server', operator: 'system_vendor', host: 'mcp.example.test', hosts: ['mcp.example.test'], auth: 'oauth_per_user',
  scopes: ['issues:read'], data_class: 'internal', systems: ['jira'], serving: ['jira'], egress: [], approved_by: 'ada', pull: '#9',
  refused_requests: 0,
  tools: [{ registry: 'jira.search', upstream: 'search', class: 'read', digest: `sha256:${'a'.repeat(64)}`, state: 'approved', approved_by: 'ada' }],
  last_check: { checked_at: '2026-11-24T09:00:00Z', outcome: 'ok', server_name: null, server_version: null },
  ...extra,
});

async function section(role: 'admin' | 'auditor', mcp?: 'none' | 'unavailable') {
  renderApp({ role, hash: '/admin/policy', mcp });
  return (await screen.findByRole('heading', { name: new RegExp(`^${M.heading}`) })).closest('section')!;
}

const card = (root: HTMLElement, id: string) => within(root).getByRole('article', { name: id });

describe('the check state', () => {
  it('is passed only for a passing check with every tool still as approved', () => {
    expect(mcpCheck(server())).toBe('passed');
    expect(mcpCheck(server({ last_check: { checked_at: null, outcome: null, server_name: null, server_version: null } }))).toBe('never');
    expect(mcpCheck(server({ last_check: { checked_at: '2026-11-24T09:00:00Z', outcome: "couldn't connect", server_name: null, server_version: null } }))).toBe('failed');
    for (const state of ['changed', 'missing'] as const) {
      const s = server();
      s.tools[0]!.state = state;
      expect(mcpCheck(s), state).toBe('changed');
    }
  });

  it('shortens a digest and finds the systems left on the built-in connectors', () => {
    expect(shortDigest(`sha256:${'0123456789abcdef'.repeat(4)}`)).toBe('sha256:0123456789ab');
    expect(shortDigest('abcdef0123456789')).toBe('abcdef012345');
    expect(builtInSystems([server({ systems: ['jira', 'confluence'], serving: ['jira'] })])).toEqual(['confluence', 'slack', 'docs']);
    // An older tool server without `serving`: its record's list counts.
    expect(builtInSystems([server({ systems: ['jira', 'slack'], serving: null })])).toEqual(['confluence', 'docs']);
    expect(builtInSystems([])).toEqual(['jira', 'confluence', 'slack', 'docs']);
  });
});

describe('the MCP servers section', () => {
  it('shows each server with who runs it, its host, sign-in, data class, check and tools', async () => {
    const root = await section('admin');
    expect(within(root).getByText(M.builtIn('Confluence'))).toBeInTheDocument();
    expect(within(root).getAllByRole('article')).toHaveLength(3);

    const tracker = card(root, 'acme-tracker');
    expect(within(tracker).getByText(M.operators.system_vendor!)).toBeInTheDocument();
    expect(within(tracker).getByText('mcp.tracker.example.test')).toBeInTheDocument();
    expect(within(tracker).getByText(M.auth.oauth_per_user!)).toBeInTheDocument();
    expect(within(tracker).getByText(M.dataClasses.internal!)).toBeInTheDocument();
    expect(within(tracker).getByRole('list', { name: `${M.facts.serves}, acme-tracker` })).toHaveTextContent('Jira');
    expect(within(tracker).getByText(M.notServing('Confluence'))).toBeInTheDocument();
    // A tool changed at the server: flagged on the card and on its row, in words.
    const flag = within(tracker).getByText(M.changedBody(1)).closest('[role="note"]')!;
    expect(flag).toHaveTextContent('jira.update_issue');
    const tools = within(tracker).getByRole('list', { name: `${M.facts.tools}, acme-tracker` });
    const changed = within(tools).getAllByRole('listitem').find((li) => li.dataset.state === 'changed')!;
    expect(changed).toHaveTextContent(M.toolStates.changed!);
    expect(changed).toHaveTextContent(M.classes.write!);
    expect(within(tracker).getByText(M.check.changed)).toBeInTheDocument();
    // Writes still need the PM.
    expect(within(tracker).getByText(M.writeNote)).toBeInTheDocument();
    const search = within(tools).getAllByRole('listitem').find((li) => li.textContent?.startsWith('jira.search'))!;
    expect(search).toHaveTextContent(M.classes.read!);
    const digest = within(search).getByText(/^definition sha256:[0-9a-f]{12}$/);
    expect(digest.getAttribute('title')).toMatch(/^sha256:[0-9a-f]{64}$/);
    expect(search).toHaveTextContent(M.approvedBy('ada'));

    const docs = card(root, 'pmos-docs');
    expect(within(docs).getByText(M.check.pillPassed)).toBeInTheDocument();
    expect(within(docs).getByText(M.operators.pmos!)).toBeInTheDocument();
    expect(within(docs).queryByText(M.writeNote)).not.toBeInTheDocument();
    expect(within(docs).queryByRole('note')).not.toBeInTheDocument();

    // A failed check: said on the card, with its reason, and the tool gone from the server.
    const chat = card(root, 'chat-selfhosted');
    expect(chat.dataset.check).toBe('failed');
    expect(within(chat).getByText(M.failedBody)).toBeInTheDocument();
    expect(within(chat).getByText(/connection refused/)).toBeInTheDocument();
    expect(within(chat).getByText(M.toolStates.missing!)).toBeInTheDocument();
    expect(within(chat).getByText(M.check.refused(2))).toBeInTheDocument();

    expect(within(root).getByText(M.cannotShow)).toBeInTheDocument();
    expect(within(root).getByRole('link', { name: M.proposeLink })).toHaveAttribute('href', '#/admin/content');
  });

  it('shows an Auditor the same metadata, with no way to propose', async () => {
    const root = await section('auditor');
    expect(within(root).getAllByRole('article')).toHaveLength(3);
    expect(within(card(root, 'acme-tracker')).getByText('mcp.tracker.example.test')).toBeInTheDocument();
    expect(within(root).getByText(M.howToChange)).toBeInTheDocument();
    expect(within(root).queryByRole('link', { name: M.proposeLink })).not.toBeInTheDocument();
  });

  it('says plainly when no server is configured, and when the tool server did not answer', async () => {
    const none = await section('admin', 'none');
    expect(within(none).getByText(M.none)).toBeInTheDocument();
    expect(within(none).queryByRole('article')).not.toBeInTheDocument();
    expect(within(none).getByText('0')).toBeInTheDocument();
  });

  it('never reads an unanswered view as "none configured"', async () => {
    const root = await section('admin', 'unavailable');
    expect(within(root).getByText(M.unknown)).toBeInTheDocument();
    expect(within(root).queryByText(M.none)).not.toBeInTheDocument();
    // An older server that sends no `mcp` at all reads the same way.
    const { container } = render(<McpSection mcp={undefined} editable={false} />);
    expect(within(container).getByText(M.unknown)).toBeInTheDocument();
  });

  it('says when a per-PM server has not been checked yet, and when the tool server is too old to say what it routes', () => {
    render(<McpServerCard server={server({ serving: null, last_check: { checked_at: null, outcome: null, server_name: null, server_version: null } })} />);
    expect(screen.getByText(M.check.neverPerUser)).toBeInTheDocument();
    expect(screen.getByText(M.servingUnknown)).toBeInTheDocument();
    expect(screen.queryByText(M.check.pillPassed)).not.toBeInTheDocument();
  });

  it('marks a tool not yet checked, and a server serving nothing', () => {
    const s = server({ serving: [], auth: 'service_token' });
    s.tools[0]!.state = 'unchecked';
    s.last_check = { checked_at: null, outcome: null, server_name: null, server_version: null };
    render(<McpServerCard server={s} />);
    expect(screen.getAllByText(M.nothingServed).length).toBeGreaterThan(0);
    expect(screen.getAllByText(M.toolStates.unchecked!).length).toBeGreaterThan(0);
    expect(screen.getAllByText(M.check.never).length).toBeGreaterThan(0);
  });
});
