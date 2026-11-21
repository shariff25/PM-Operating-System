import { screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { ApiError } from '../../api';
import type { Api } from '../../api';
import { copy } from '../../copy';
import { renderApp } from '../../test/util';

const saveBlob = vi.hoisted(() => vi.fn());
vi.mock('../../admin/download', () => ({ saveBlob }));

const A = copy.admin;
const nav = () => screen.getByRole('navigation', { name: copy.nav.label });
const rows = () => within(screen.getByRole('table', { name: A.audit.tableLabel })).getAllByRole('row').slice(1);
const hash = () => window.location.hash;

beforeEach(() => saveBlob.mockClear());

describe('admin navigation and access', () => {
  it('shows an admin the Admin entry with all four parts below it', async () => {
    renderApp({ role: 'admin', hash: '/admin' });
    await screen.findByRole('heading', { level: 1, name: A.sections.overview });
    const sub = within(nav()).getByRole('list', { name: A.subnavLabel });
    expect(within(sub).getAllByRole('link').map((a) => a.textContent)).toEqual(['Overview', 'Audit log', 'Team', 'Bots', 'Connectors', 'Policy', 'Content and harness']);
    expect(within(sub).getByRole('link', { name: 'Overview' })).toHaveAttribute('aria-current', 'page');
  });

  it('shows an auditor three parts: no Connectors entry, and no dead link on the overview', async () => {
    renderApp({ role: 'auditor', hash: '/admin' });
    await screen.findByRole('heading', { level: 1, name: A.sections.overview });
    expect(within(nav()).queryByRole('link', { name: 'Connectors' })).not.toBeInTheDocument();
    expect(screen.queryByRole('link', { name: /Connectors/ })).not.toBeInTheDocument();
    expect(screen.getByRole('link', { name: /Audit log/ })).toBeInTheDocument();
  });

  it('shows a PM no Admin entry, and a calm page on a direct visit', async () => {
    renderApp({ role: 'pm', hash: '/admin/audit' });
    await screen.findByRole('heading', { level: 1, name: A.noAccess.title });
    expect(within(nav()).queryByRole('link', { name: copy.nav.admin })).not.toBeInTheDocument();
    expect(screen.getByRole('link', { name: A.noAccess.back })).toHaveAttribute('href', '#/');
  });

  it('keeps an auditor out of the connectors page without asking the server', async () => {
    const calls: string[] = [];
    renderApp({
      role: 'auditor',
      hash: '/admin/connectors',
      wrap: (api) => {
        const f = api.adminConnectors.bind(api);
        api.adminConnectors = async () => { calls.push('connectors'); return f(); };
        return api;
      },
    });
    await screen.findByRole('heading', { level: 1, name: A.noAccess.title });
    expect(calls).toEqual([]);
  });

  it('treats a 403 from the server like no access, not like a broken page', async () => {
    renderApp({
      role: 'admin',
      hash: '/admin/team',
      wrap: (api) => {
        api.adminUsers = async () => { throw new ApiError(403, 'forbidden', 'Your role does not allow this.'); };
        return api;
      },
    });
    await screen.findByRole('heading', { level: 1, name: A.noAccess.title });
    expect(screen.queryByRole('alert')).not.toBeInTheDocument();
  });
});

describe('overview', () => {
  it('shows the four counts and the window', async () => {
    renderApp({ role: 'admin', hash: '/admin' });
    const group = await screen.findByRole('group', { name: A.overview.statsLabel });
    for (const label of Object.values(A.overview.stats)) expect(within(group).getByText(label)).toBeInTheDocument();
    expect(screen.getByRole('heading', { level: 2, name: /^Last 7 days, since/ })).toBeInTheDocument();
    // The denied tile opens the audit log on that outcome.
    expect(within(group).getByRole('link', { name: new RegExp(A.overview.stats.denied) }).getAttribute('href')).toMatch(/^#\/admin\/audit\?outcome=denied&from=\d{4}-\d{2}-\d{2}$/);
  });

  it('shows a dash and a plain note when the audit counts are unavailable', async () => {
    renderApp({ role: 'admin', hash: '/admin', toolServerDown: true });
    await screen.findByText(A.overview.partial);
    expect(screen.getAllByText(A.overview.subs.unavailable)).toHaveLength(3);
  });
});

describe('overview: people who need an admin', () => {
  const P = A.overview.people;
  it('lists each flag with its people, each a link to them on the Team page', async () => {
    renderApp({ role: 'admin', hash: '/admin' });
    const card = await screen.findByRole('region', { name: new RegExp(P.title) });
    expect(within(card).getByText(P.unconfirmed)).toBeInTheDocument();
    expect(within(card).getByRole('link', { name: 'sam' })).toHaveAttribute('href', '#/admin/team?person=sam');
    expect(within(card).getByText(P.conflicts(1))).toBeInTheDocument();
    expect(within(card).getByRole('link', { name: 'ana' })).toHaveAttribute('href', '#/admin/team?person=ana');
    expect(within(card).getByText(/^Admin and Auditor \(pilot exception, ends \d+ \w+ \d{4}\)$/)).toBeInTheDocument();
    expect(within(card).getByRole('link', { name: 'raj' })).toBeInTheDocument();
    expect(within(card).getByText(P.needsHost)).toBeInTheDocument();
    expect(within(card).getByRole('link', { name: P.openTeam })).toHaveAttribute('href', '#/admin/team?show=attention');
    // Bots are not people: their names are not links.
    expect(within(card).queryByRole('link', { name: 'pm-tom' })).not.toBeInTheDocument();
    expect(within(card).getByText('pm-tom')).toBeInTheDocument();
    expect(screen.getByText(/^The pilot exception ends on /)).toBeInTheDocument();
    expect(screen.getByText(A.overview.held(1))).toBeInTheDocument();
  });

  it('shows nothing when no list has anyone, and nothing from an older server', async () => {
    const first = renderApp({
      role: 'admin',
      hash: '/admin',
      wrap: (api) => {
        const f = api.adminOverview.bind(api);
        api.adminOverview = async () => {
          const o = await f();
          return {
            ...o,
            users: {
              ...o.users!, sign_in_unconfirmed: [], dual_role: [], pilot_exception: null, role_conflicts: [], entra_needs_host: [],
              no_slack: [], identity_conflicts: 0, identity_conflict_users: [], resolve_miss_warning: [], bots_without_active_owner: [],
              deactivation_limit_reached: false,
            },
          };
        };
        return api;
      },
    });
    await screen.findByRole('group', { name: A.overview.statsLabel });
    expect(screen.queryByRole('region', { name: new RegExp(P.title) })).not.toBeInTheDocument();
    expect(document.querySelector('.overview-notices')).toBeNull();
    first.unmount();
    renderApp({
      role: 'admin',
      hash: '/admin',
      wrap: (api) => {
        const f = api.adminOverview.bind(api);
        api.adminOverview = async () => {
          const o = await f();
          delete o.users;
          return o;
        };
        return api;
      },
    });
    await screen.findByRole('group', { name: A.overview.statsLabel });
    expect(screen.queryByRole('region', { name: new RegExp(P.title) })).not.toBeInTheDocument();
  });

  it('shows the red notice naming who still holds both once the exception has lapsed, and the guessing warning', async () => {
    renderApp({ role: 'admin', hash: '/admin', pilotLapsed: true });
    const alert = await screen.findByText(/^The pilot exception ended on /);
    const note = alert.closest('.note')!;
    expect(note).toHaveClass('note-error');
    expect(within(note as HTMLElement).getByRole('link', { name: 'raj' })).toBeInTheDocument();
    expect(screen.getByText(/^Lookups from the operator console found nobody more than 100 times/)).toBeInTheDocument();
  });

  it('lists who signed in with Microsoft for the first time, by handle', async () => {
    renderApp({
      role: 'admin',
      hash: '/admin',
      wrap: (api) => {
        const f = api.adminOverview.bind(api);
        api.adminOverview = async () => {
          const o = await f();
          return {
            ...o,
            users: { ...o.users!, first_links: [{ handle: 'nia-first', at: '2026-11-19T09:00:00Z', provider: 'entra' }] },
          };
        };
        return api;
      },
    });
    const card = await screen.findByRole('region', { name: new RegExp(P.title) });
    expect(within(card).getByText(P.firstLinks(1))).toBeInTheDocument();
    expect(within(card).getByText(P.firstLinksNext)).toBeInTheDocument();
    expect(within(card).getByRole('link', { name: 'nia-first' })).toBeInTheDocument();
  });

  it('shows an auditor the same lists, linked to the Team page they may read', async () => {
    renderApp({ role: 'auditor', hash: '/admin' });
    const card = await screen.findByRole('region', { name: new RegExp(P.title) });
    expect(within(card).getByRole('link', { name: 'sam' })).toBeInTheDocument();
  });
});

describe('overview: recent denials and approvals', () => {
  const recent = (title: string) => screen.getByRole('region', { name: title });

  it('lists the five newest denied actions and approvals from the audit log, each with a way into the filtered log', async () => {
    const seen: unknown[] = [];
    renderApp({
      role: 'admin',
      hash: '/admin',
      wrap: (api) => {
        const f = api.adminAudit.bind(api);
        api.adminAudit = async (q) => { seen.push(q); return f(q); };
        return api;
      },
    });
    const denied = await screen.findByRole('region', { name: A.overview.recent.denied });
    await waitFor(() => expect(within(denied).getAllByRole('listitem').length).toBeGreaterThan(0));
    expect(within(denied).getAllByRole('listitem').length).toBeLessThanOrEqual(5);
    expect(seen).toEqual(expect.arrayContaining([{ outcome: 'denied', limit: 5 }, { outcome: 'approved', limit: 5 }]));
    expect(within(denied).getByRole('link', { name: /See all/ })).toHaveAttribute('href', '#/admin/audit?outcome=denied');
    const approved = recent(A.overview.recent.approved);
    await waitFor(() => expect(within(approved).getAllByRole('listitem').length).toBeGreaterThan(0));
    expect(within(approved).getByRole('link', { name: /See all/ })).toHaveAttribute('href', '#/admin/audit?outcome=approved');
    // Metadata only: who, which client, when. No draft text.
    const first = within(denied).getAllByRole('listitem')[0]!;
    expect(first.querySelector('time')).not.toBeNull();
  });

  it('shows the same to an auditor, and nothing it could not fetch to someone without the audit log', async () => {
    const first = renderApp({ role: 'auditor', hash: '/admin' });
    expect(await screen.findByRole('region', { name: A.overview.recent.denied })).toBeInTheDocument();
    first.unmount();
    const calls: unknown[] = [];
    renderApp({
      role: 'admin',
      hash: '/admin',
      wrap: (api) => {
        const me = api.me.bind(api);
        api.me = async () => ({ ...(await me()), permissions: ['manage-members'] });
        const f = api.adminAudit.bind(api);
        api.adminAudit = async (q) => { calls.push(q); return f(q); };
        return api;
      },
    });
    await screen.findByRole('group', { name: A.overview.statsLabel });
    expect(screen.queryByRole('region', { name: A.overview.recent.denied })).not.toBeInTheDocument();
    expect(calls).toEqual([]);
  });

  it('says plainly when there is nothing to list, and when the list cannot be loaded', async () => {
    renderApp({
      role: 'admin',
      hash: '/admin',
      wrap: (api) => {
        const f = api.adminAudit.bind(api);
        api.adminAudit = async (q) => {
          if (q.outcome === 'denied') return { events: [], next_cursor: null };
          if (q.outcome === 'approved') throw new ApiError(502, 'upstream_unavailable', 'The tool server did not answer.');
          return f(q);
        };
        return api;
      },
    });
    expect(await screen.findByText(A.overview.recent.noDenied)).toBeInTheDocument();
    expect(await screen.findByText('The tool server did not answer.')).toBeInTheDocument();
  });
});

describe('audit log', () => {
  it('lists events in a real table with headers, and states that it is metadata only', async () => {
    renderApp({ role: 'auditor', hash: '/admin/audit' });
    const table = await screen.findByRole('table', { name: A.audit.tableLabel });
    expect(within(table).getAllByRole('columnheader').map((h) => h.textContent)).toContain('Outcome');
    expect(rows()).toHaveLength(50);
    expect(screen.getByText(A.audit.metadataOnly)).toBeInTheDocument();
  });

  it('loads the next page with the cursor and the same filters, until there are no more', async () => {
    const seen: unknown[] = [];
    renderApp({
      role: 'auditor',
      hash: '/admin/audit',
      wrap: (api) => {
        const f = api.adminAudit.bind(api);
        api.adminAudit = async (q) => { seen.push(q); return f(q); };
        return api;
      },
    });
    await screen.findByRole('table', { name: A.audit.tableLabel });
    const first = rows().length;
    await userEvent.click(screen.getByRole('button', { name: A.audit.loadMore }));
    await waitFor(() => expect(rows().length).toBeGreaterThan(first));
    const [one, two] = seen as { before?: string; limit: number }[];
    expect(one).toMatchObject({ limit: 50 });
    expect(one!.before).toBeUndefined();
    expect(two).toMatchObject({ limit: 50 });
    expect(two!.before).toMatch(/^c1\./);
    // Keep going to the end: the button gives way to a closing line.
    while (screen.queryByRole('button', { name: A.audit.loadMore })) {
      await userEvent.click(screen.getByRole('button', { name: A.audit.loadMore }));
      await waitFor(() => expect(screen.queryByText(A.audit.loadingMore)).not.toBeInTheDocument());
    }
    expect(screen.getByText(A.audit.endLine)).toBeInTheDocument();
    expect(new Set(rows().map((r) => r.textContent)).size).toBe(rows().length);
  });

  it('sends the same filters with the cursor when "Load more" is used on a filtered list', async () => {
    const seen: { from?: string; to?: string; limit?: number; before?: string }[] = [];
    renderApp({
      role: 'auditor',
      hash: '/admin/audit?from=2020-01-01&to=2099-12-31',
      wrap: (api) => {
        const f = api.adminAudit.bind(api);
        api.adminAudit = async (q) => { seen.push(q); return f(q); };
        return api;
      },
    });
    await screen.findByRole('table', { name: A.audit.tableLabel });
    const first = rows().length;
    await userEvent.click(screen.getByRole('button', { name: A.audit.loadMore }));
    await waitFor(() => expect(rows().length).toBeGreaterThan(first));
    const [one, two] = seen;
    expect(one).toMatchObject({ from: '2020-01-01T00:00:00.000Z', to: '2100-01-01T00:00:00.000Z', limit: 50 });
    expect(one!.before).toBeUndefined();
    expect(two).toMatchObject({ from: '2020-01-01T00:00:00.000Z', to: '2100-01-01T00:00:00.000Z', limit: 50 });
    expect(two!.before).toMatch(/^c1\./);
  });

  it('keeps the events shown when the next page fails, and says so', async () => {
    renderApp({
      role: 'auditor',
      hash: '/admin/audit',
      wrap: (api) => {
        const f = api.adminAudit.bind(api);
        api.adminAudit = async (q) => {
          if (q.before) throw new ApiError(502, 'upstream_unavailable', 'The tool server did not answer.');
          return f(q);
        };
        return api;
      },
    });
    await screen.findByRole('table', { name: A.audit.tableLabel });
    await userEvent.click(screen.getByRole('button', { name: A.audit.loadMore }));
    expect(await screen.findByText('The tool server did not answer.')).toBeInTheDocument();
    expect(rows()).toHaveLength(50);
  });

  it('puts the filters in the address and starts over from the newest event', async () => {
    renderApp({ role: 'auditor', hash: '/admin/audit' });
    await screen.findByRole('table', { name: A.audit.tableLabel });
    await userEvent.type(screen.getByLabelText(A.audit.fields.actor), 'ben');
    await userEvent.selectOptions(screen.getByLabelText(A.audit.fields.outcome), 'denied');
    await waitFor(() => expect(hash()).toBe('#/admin/audit?actor=ben&outcome=denied'));
    await waitFor(() => expect(rows().length).toBeGreaterThan(0));
    for (const r of rows()) {
      expect(within(r).getByText('Denied')).toBeInTheDocument();
      expect(within(r).getByText('ben')).toBeInTheDocument();
    }
    await userEvent.click(screen.getByRole('button', { name: A.audit.clear }));
    await waitFor(() => expect(hash()).toBe('#/admin/audit'));
    expect(screen.getByLabelText(A.audit.fields.actor)).toHaveValue('');
  });

  it('opens a bookmarked view with its filters in the boxes', async () => {
    renderApp({ role: 'auditor', hash: '/admin/audit?actor=dana&outcome=rejected&from=2026-01-01' });
    await screen.findByRole('table', { name: A.audit.tableLabel });
    expect(screen.getByLabelText(A.audit.fields.actor)).toHaveValue('dana');
    expect(screen.getByLabelText(A.audit.fields.outcome)).toHaveValue('rejected');
    expect(screen.getByLabelText(A.audit.fields.from)).toHaveValue('2026-01-01');
  });

  it('says plainly when nothing matches, with a way out', async () => {
    renderApp({ role: 'auditor', hash: '/admin/audit?actor=nobody' });
    expect(await screen.findByText(A.audit.emptyFiltered)).toBeInTheDocument();
    await userEvent.click(screen.getAllByRole('button', { name: A.audit.clear })[0]!);
    await screen.findByRole('table', { name: A.audit.tableLabel });
  });

  it('does not ask the server when the first day is after the last', async () => {
    const calls = vi.fn();
    renderApp({
      role: 'auditor',
      hash: '/admin/audit?from=2026-11-10&to=2026-11-01',
      wrap: (api) => {
        const f = api.adminAudit.bind(api);
        api.adminAudit = async (q) => { calls(q); return f(q); };
        return api;
      },
    });
    await screen.findAllByText(A.audit.backwards);
    expect(calls).not.toHaveBeenCalled();
  });

  it('shows an error with Try again, and loads on the second go', async () => {
    let fail = true;
    renderApp({
      role: 'auditor',
      hash: '/admin/audit',
      wrap: (api) => {
        const f = api.adminAudit.bind(api);
        api.adminAudit = async (q) => {
          if (fail) { fail = false; throw new ApiError(502, 'upstream_unavailable', 'The tool server did not answer.'); }
          return f(q);
        };
        return api;
      },
    });
    expect(await screen.findByRole('alert')).toHaveTextContent('The tool server did not answer.');
    await userEvent.click(screen.getByRole('button', { name: copy.errors.retry }));
    await screen.findByRole('table', { name: A.audit.tableLabel });
  });

  it('shows every field of an event, and spells out the ones that are null', async () => {
    renderApp({ role: 'auditor', hash: '/admin/audit' });
    await screen.findByRole('table', { name: A.audit.tableLabel });
    const toggle = within(rows()[0]!).getByRole('button', { name: /^Show details for/ });
    expect(toggle).toHaveAttribute('aria-expanded', 'false');
    await userEvent.click(toggle);
    expect(toggle).toHaveAttribute('aria-expanded', 'true');
    const details = screen.getByRole('group', { name: A.audit.detailsLabel });
    for (const label of Object.values(A.audit.detailFields)) expect(within(details).getByText(label)).toBeInTheDocument();
    // The first sample event is a read: no target, no note, no skill or bot in some cases.
    expect(within(details).getAllByText(A.notRecorded).length).toBeGreaterThan(0);
    expect(within(details).getByText(A.audit.titlesNote)).toBeInTheDocument();
    await userEvent.click(toggle);
    expect(screen.queryByRole('group', { name: A.audit.detailsLabel })).not.toBeInTheDocument();
  });
});

describe('audit export', () => {
  it('saves the file with the current filters and reports the row count', async () => {
    const seen: unknown[] = [];
    renderApp({
      role: 'auditor',
      hash: '/admin/audit?outcome=denied&from=2026-11-01',
      wrap: (api) => {
        const f = api.adminAuditExport.bind(api);
        api.adminAuditExport = async (q) => { seen.push(q); return f(q); };
        return api;
      },
    });
    await screen.findByRole('table', { name: A.audit.tableLabel });
    await userEvent.click(screen.getByRole('button', { name: A.audit.export }));
    const note = await screen.findByRole('status');
    expect(note).toHaveTextContent(/Saved \d+ events? to pmos-audit\.csv\./);
    expect(note).not.toHaveTextContent(/more matched/);
    expect(saveBlob).toHaveBeenCalledTimes(1);
    expect(saveBlob.mock.calls[0]![1]).toBe('pmos-audit.csv');
    expect(seen[0]).toEqual({ outcome: 'denied', from: '2026-11-01T00:00:00.000Z' });
    const csv = await (saveBlob.mock.calls[0]![0] as Blob).text();
    expect(csv.split('\r\n')[0]).toBe('"at","actor","client","action","system","target","outcome","detail","sources","titled_sources","skill","bot","task_id","id","persona"');
  });

  it('tells the person when the export was cut at the row cap', async () => {
    renderApp({ role: 'admin', hash: '/admin/audit', exportCap: 3 });
    await screen.findByRole('table', { name: A.audit.tableLabel });
    await userEvent.click(screen.getByRole('button', { name: A.audit.export }));
    const note = await screen.findByRole('status');
    expect(note).toHaveTextContent('Saved 3 events to pmos-audit.csv.');
    expect(note).toHaveTextContent(A.audit.exportTruncated(3));
    expect(saveBlob).toHaveBeenCalledTimes(1);
  });

  it('shows a plain error and saves nothing when the export fails', async () => {
    renderApp({
      role: 'admin',
      hash: '/admin/audit',
      wrap: (api: Api) => {
        api.adminAuditExport = async () => { throw new ApiError(502, 'upstream_unavailable', 'The tool server did not answer.'); };
        return api;
      },
    });
    await screen.findByRole('table', { name: A.audit.tableLabel });
    await userEvent.click(screen.getByRole('button', { name: A.audit.export }));
    expect(await screen.findByText('The tool server did not answer.')).toBeInTheDocument();
    expect(saveBlob).not.toHaveBeenCalled();
  });
});

describe('connectors', () => {
  it('shows name, mode, health and when it was checked', async () => {
    renderApp({ role: 'admin', hash: '/admin/connectors' });
    const table = await screen.findByRole('table', { name: A.connectors.tableLabel });
    expect(within(table).getAllByRole('row')).toHaveLength(5);
    const jira = within(table).getByRole('row', { name: /Jira/ });
    expect(within(jira).getByText('Live')).toBeInTheDocument();
    expect(within(jira).getByText('Healthy')).toBeInTheDocument();
  });

  it('says why a connector is down', async () => {
    renderApp({ role: 'admin', hash: '/admin/connectors', toolServerDown: true });
    const table = await screen.findByRole('table', { name: A.connectors.tableLabel });
    const docs = within(table).getByRole('row', { name: /Docs/ });
    expect(within(docs).getByText('Down')).toBeInTheDocument();
    expect(within(docs).getByText(A.connectors.errorClasses.timeout!)).toBeInTheDocument();
  });
});
