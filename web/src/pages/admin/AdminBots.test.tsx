import { screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it } from 'vitest';
import { ApiError } from '../../api';
import { copy } from '../../copy';
import { renderApp } from '../../test/util';

// The Admin's view of the bots (slice A1): the catalog, who has which bot, the checks, the drafts
// on hold and their release, the audit log's bot filter, and a held draft as its owner sees it.

const B = copy.admin.bots;
const catalog = () => screen.getByRole('table', { name: B.tableLabel });
const held = () => screen.getByRole('table', { name: B.heldTableLabel });
const rowOf = (table: HTMLElement, name: string) => within(table).getByRole('row', { name: new RegExp(name) });

describe('the Admin Bots page', () => {
  it('lists every bot with its state, its people and its checks, and links to the content admin', async () => {
    renderApp({ role: 'admin', hash: '/admin/bots' });
    await screen.findByRole('heading', { level: 1, name: copy.admin.sections.bots });
    await screen.findByRole('table', { name: B.tableLabel });

    const chief = rowOf(catalog(), 'Chief');
    // The state pill (the name beside it also carries the Preview tag, tested below).
    expect(within(chief).getByText('Preview', { selector: '.pill' })).toBeInTheDocument();
    expect(within(chief).getByText(B.people(4))).toBeInTheDocument();
    expect(within(chief).getByLabelText(B.handlesLabel('Chief'))).toHaveTextContent('ana, ben, dana, ravi');
    expect(within(chief).getByRole('link', { name: B.changeFileLabel('Chief') })).toHaveAttribute(
      'href',
      `#/admin/content?file=${encodeURIComponent('content/personas/chief/PERSONA.md')}`,
    );

    const nudge = rowOf(catalog(), 'Nudge');
    expect(within(nudge).getByText('Offered')).toBeInTheDocument();
    expect(within(nudge).getAllByText(B.alwaysOn).length).toBeGreaterThan(0);

    const scout = rowOf(catalog(), 'Scout');
    expect(within(scout).getByText('Withdrawn')).toBeInTheDocument();

    // A name clash and a lint warning are flagged; an unmet need is named.
    const echo = rowOf(catalog(), 'Echo');
    expect(within(echo).getByText(B.clash(1))).toBeInTheDocument();
    expect(within(echo).getByText(B.lint.warning(1))).toBeInTheDocument();
    expect(within(rowOf(catalog(), 'Sprout')).getByText(B.needsUnmet('an analytics source'))).toBeInTheDocument();

    // A key teams still name, gone from the catalog.
    expect(screen.getByText(B.unknownHeading)).toBeInTheDocument();
    expect(screen.getByText(/^atlas: on 1 team/)).toBeInTheDocument();

    // Nothing here offers, withdraws or changes a team: no switch, checkbox or offer button.
    expect(screen.queryByRole('switch')).not.toBeInTheDocument();
    expect(screen.queryByRole('checkbox')).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /offer|withdraw|add|remove/i })).not.toBeInTheDocument();
  });

  // The review of 2efb73f: the Admin's view named bots without the Preview tag. Every bot named
  // here that is a Preview carries the short tag, in the catalog and in the drafts on hold;
  // a benchmarked one (Nudge) and a bot gone from the catalog don't.
  it('names every Preview bot with the Preview tag, in the catalog and in the drafts on hold', async () => {
    renderApp({ role: 'admin', hash: '/admin/bots' });
    await screen.findByRole('table', { name: B.tableLabel });
    const tag = `${copy.bots.preview}: ${copy.bots.previewNote}`;
    const tagIn = (el: HTMLElement) => within(el).queryAllByText((_, node) => node?.classList.contains('tag-preview') === true);

    for (const name of ['Chief', 'Sprout', 'Echo', 'Scout']) {
      const row = rowOf(catalog(), name);
      const tags = tagIn(row);
      expect(tags, name).toHaveLength(1);
      expect(tags[0]).toHaveTextContent(tag);
      // Beside the bot's name, in the row's header cell.
      expect(within(row).getByRole('rowheader')).toContainElement(tags[0] ?? null);
    }
    expect(tagIn(rowOf(catalog(), 'Nudge'))).toHaveLength(0);

    await screen.findByRole('table', { name: B.heldTableLabel });
    const heldRows = within(held()).getAllByRole('row').slice(1);
    const scoutRows = heldRows.filter((r) => within(r).queryByText('Scout'));
    expect(scoutRows).toHaveLength(2);
    for (const r of scoutRows) {
      expect(tagIn(r)).toHaveLength(1);
      expect(tagIn(r)[0]).toHaveTextContent(tag);
    }
    const gone = heldRows.filter((r) => within(r).queryByText(B.heldGoneBot('atlas')));
    expect(gone).toHaveLength(1);
    for (const r of gone) expect(tagIn(r)).toHaveLength(0);
  });

  it('lets an admin release a held draft after a confirm that says what releasing means', async () => {
    const user = userEvent.setup();
    const released: string[] = [];
    renderApp({
      role: 'admin',
      hash: '/admin/bots',
      wrap: (api) => {
        const f = api.adminReleaseHeld.bind(api);
        api.adminReleaseHeld = async (id) => {
          released.push(id);
          return f(id);
        };
        return api;
      },
    });
    await screen.findByRole('table', { name: B.heldTableLabel });
    expect(within(held()).getAllByRole('row')).toHaveLength(4); // the header and three drafts

    const button = within(held()).getByRole('button', { name: B.releaseLabel('apr_held', 'ana') });
    await user.click(button);
    let dialog = await screen.findByRole('dialog', { name: B.releaseDialog.title });
    expect(within(dialog).getByText(B.releaseDialog.what)).toBeInTheDocument();
    for (const p of B.releaseDialog.points('ana')) expect(within(dialog).getByText(p)).toBeInTheDocument();
    // Escape closes it, sends nothing, and gives focus back.
    await user.keyboard('{Escape}');
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
    expect(button).toHaveFocus();
    expect(released).toEqual([]);

    await user.click(button);
    dialog = await screen.findByRole('dialog', { name: B.releaseDialog.title });
    await user.click(within(dialog).getByRole('button', { name: B.releaseDialog.submit }));
    await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument());
    expect(released).toEqual(['apr_held']);
    expect(await screen.findByText(B.released('apr_held'))).toBeInTheDocument();
    await waitFor(() => expect(within(held()).queryByRole('button', { name: B.releaseLabel('apr_held', 'ana') })).not.toBeInTheDocument());
  });

  it("shows the server's refusal in the dialog and changes nothing", async () => {
    const user = userEvent.setup();
    renderApp({
      role: 'admin',
      hash: '/admin/bots',
      wrap: (api) => {
        api.adminReleaseHeld = async () => {
          throw new ApiError(409, 'not_held', "That draft isn't on hold any more. Nothing changed.");
        };
        return api;
      },
    });
    await screen.findByRole('table', { name: B.heldTableLabel });
    await user.click(within(held()).getByRole('button', { name: B.releaseLabel('apr_held', 'ana') }));
    const dialog = await screen.findByRole('dialog', { name: B.releaseDialog.title });
    await user.click(within(dialog).getByRole('button', { name: B.releaseDialog.submit }));
    expect(await within(dialog).findByRole('alert')).toHaveTextContent("That draft isn't on hold any more. Nothing changed.");
    expect(within(dialog).getByRole('button', { name: B.releaseDialog.submit })).toBeEnabled();
  });

  it('shows an auditor the same view, with no release', async () => {
    renderApp({ role: 'auditor', hash: '/admin/bots' });
    await screen.findByRole('table', { name: B.heldTableLabel });
    expect(within(held()).queryByRole('button')).not.toBeInTheDocument();
    expect(screen.getByText(B.releaseAdminOnly)).toBeInTheDocument();
    expect(within(rowOf(catalog(), 'Chief')).getByRole('link', { name: B.openFileLabel('Chief') })).toBeInTheDocument();
    expect(screen.queryByRole('link', { name: B.openContent })).not.toBeInTheDocument();
  });

  it('keeps a PM out without asking the server', async () => {
    const calls: string[] = [];
    renderApp({
      role: 'pm',
      hash: '/admin/bots',
      wrap: (api) => {
        api.adminBots = async () => {
          calls.push('bots');
          throw new ApiError(403, 'forbidden', 'No.');
        };
        return api;
      },
    });
    await screen.findByRole('heading', { level: 1, name: copy.admin.noAccess.title });
    expect(calls).toEqual([]);
  });
});

describe('the audit log by bot', () => {
  it('filters by a bot, keeps it in the address, and shows it in the details', async () => {
    const user = userEvent.setup();
    const seen: (string | undefined)[] = [];
    renderApp({
      role: 'auditor',
      hash: '/admin/audit',
      wrap: (api) => {
        const f = api.adminAudit.bind(api);
        api.adminAudit = async (q) => {
          seen.push(q.persona);
          return f(q);
        };
        return api;
      },
    });
    const select = await screen.findByLabelText(copy.admin.audit.fields.persona);
    await waitFor(() => expect(within(select).getByRole('option', { name: 'Chief' })).toBeInTheDocument());
    expect(within(select).queryByRole('option', { name: 'Nudge' })).not.toBeInTheDocument();
    expect(within(select).getByRole('option', { name: copy.admin.audit.fields.personaGone('atlas') })).toBeInTheDocument();
    await user.selectOptions(select, 'chief');
    await waitFor(() => expect(window.location.hash).toBe('#/admin/audit?persona=chief'));
    await waitFor(() => expect(seen.at(-1)).toBe('chief'));
    const table = await screen.findByRole('table', { name: copy.admin.audit.tableLabel });
    const first = within(table).getAllByRole('row')[1]!;
    await user.click(within(first).getByRole('button'));
    const details = await screen.findByRole('group', { name: copy.admin.audit.detailsLabel });
    expect(within(details).getByText(copy.admin.audit.detailFields.persona)).toBeInTheDocument();
    expect(within(details).getByText('chief')).toBeInTheDocument();
  });
});

describe('a held draft', () => {
  it('says it is on hold, and its approve and edit are off with the reason', async () => {
    const user = userEvent.setup();
    renderApp({ heldDraft: true, hash: '/d/apr_held' });
    await screen.findByRole('heading', { level: 1, name: 'Customer research notes, November' });
    const note = await screen.findByText(copy.draft.held.titleGone);
    expect(note).toBeInTheDocument();
    const approve = screen.getByRole('button', { name: /^Approve/ });
    expect(approve).toBeDisabled();
    expect(approve).toHaveAccessibleDescription(copy.draft.held.blocked);
    const edit = screen.getByRole('button', { name: copy.draft.edit });
    expect(edit).toBeDisabled();
    expect(edit).toHaveAccessibleDescription(new RegExp(copy.draft.held.body.slice(0, 30)));
    expect(screen.queryByLabelText(copy.draft.askLabel)).not.toBeInTheDocument();
    // Rejecting stays open.
    await user.click(screen.getByRole('button', { name: copy.reject.button }));
    expect(await screen.findByText(copy.reject.heading)).toBeInTheDocument();
  });

  it('is marked on hold in the drafts list', async () => {
    renderApp({ heldDraft: true, hash: '/drafts' });
    const link = await screen.findByRole('link', { name: /Customer research notes, November/ });
    expect(within(link).getByText(copy.home.heldBadge)).toBeInTheDocument();
  });
});
