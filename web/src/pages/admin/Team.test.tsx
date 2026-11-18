import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it } from 'vitest';
import { ApiError } from '../../api';
import type { Api } from '../../api';
import { writeOutcome } from '../../admin/team';
import { copy } from '../../copy';
import { renderApp } from '../../test/util';

const T = copy.admin.team;
const table = () => screen.findByRole('table', { name: T.tableLabel });
const rowOf = async (name: string | RegExp) => within(await table()).getByRole('row', { name });
const dialog = () => screen.getByRole('dialog');
const alertDialog = () => screen.getByRole('alertdialog');

async function openMenu(who: string) {
  await userEvent.click(await screen.findByRole('button', { name: T.menu.button(who) }));
  return screen.getByRole('menu', { name: T.menu.button(who) });
}

async function pick(who: string, item: string) {
  const menu = await openMenu(who);
  await userEvent.click(within(menu).getByRole('menuitem', { name: item }));
}

/** Count the reads of the list, and refuse the next write of one kind once with the given error. */
function refuseOnce(method: keyof Api, error: ApiError, reads: { n: number }) {
  return (api: Api) => {
    const list = api.adminUsers.bind(api);
    api.adminUsers = async () => {
      reads.n += 1;
      return list();
    };
    let done = false;
    const real = (api[method] as (...a: unknown[]) => Promise<unknown>).bind(api);
    (api as unknown as Record<string, unknown>)[method] = async (...a: unknown[]) => {
      if (!done) {
        done = true;
        throw error;
      }
      return real(...a);
    };
    return api;
  };
}

describe('team: the admin view', () => {
  it('lists every person with roles, status, sign-in, accounts and reviews', async () => {
    renderApp({ role: 'admin', hash: '/admin/team' });
    const t = await table();
    expect(within(t).getAllByRole('row')).toHaveLength(14);
    const ben = await rowOf(/Ben Carter/);
    expect(within(ben).getByText('Product Manager')).toBeInTheDocument();
    expect(within(ben).getByText('Reviewer')).toBeInTheDocument();
    expect(within(ben).getByText('Reviews ana and dana')).toBeInTheDocument();
    expect(within(ben).getByText('ben@contoso.example')).toBeInTheDocument();
    expect(within(ben).getByText(T.accounts.linked('GitHub'))).toBeInTheDocument();
    const ravi = await rowOf(/Ravi Kumar/);
    expect(within(ravi).getByText(T.accounts.notLinked('GitHub'))).toBeInTheDocument();
    expect(within(ravi).getByText(T.signIn.notYet)).toBeInTheDocument();
    expect(within(ravi).getByText('Reviewed by priya')).toBeInTheDocument();
  });

  it('marks Admin and Auditor together with one badge, and the sign-in states that need someone', async () => {
    renderApp({ role: 'admin', hash: '/admin/team' });
    const raj = await rowOf(/Raj Mehta/);
    expect(within(raj).getByText(T.dualRole)).toBeInTheDocument();
    expect(within(raj).queryByText('Auditor')).not.toBeInTheDocument();
    const sam = await rowOf(/Sam Lee/);
    expect(within(sam).getByText(T.signIn.unconfirmed)).toBeInTheDocument();
    expect(within(sam).getByRole('button', { name: T.signIn.confirmFor('Sam Lee') })).toBeInTheDocument();
    const noor = await rowOf(/Noor Haddad/);
    expect(within(noor).getByText(T.signIn.needsHost)).toBeInTheDocument();
    // A host-only person's unconfirmed name is never offered to confirm here.
    expect(within(noor).queryByRole('button', { name: /Confirm/ })).not.toBeInTheDocument();
    expect(screen.getByText(/^The pilot exception ends on \d+ \w+ \d{4}\. After that/)).toBeInTheDocument();
  });

  it('shows a deactivated person with when the grace period ends, or that it has', async () => {
    renderApp({ role: 'admin', hash: '/admin/team' });
    const tom = await rowOf(/Tom Reyes/);
    expect(within(tom).getByText(T.status.deactivated!)).toBeInTheDocument();
    expect(within(tom).getByText(/^Undo until \d+ \w+, \d\d:\d\d UTC$/)).toBeInTheDocument();
    const lena = await rowOf(/Lena Fischer/);
    expect(within(lena).getByText(/^Grace ended \d+ \w+, \d\d:\d\d UTC$/)).toBeInTheDocument();
  });

  it('filters to the people who need an admin, and to the deactivated, with counts', async () => {
    renderApp({ role: 'admin', hash: '/admin/team' });
    await table();
    await userEvent.click(screen.getByRole('tab', { name: /Needs you/ }));
    const names = within(await table()).getAllByRole('row').slice(1).map((r) => r.querySelector('.member-name')?.textContent);
    expect(names).toEqual(['Noor Haddad', 'Raj Mehta', 'Sam Lee']);
    await userEvent.click(screen.getByRole('tab', { name: /Deactivated/ }));
    expect(within(await table()).getAllByRole('row')).toHaveLength(3);
  });

  it('opens on the filter and the person a link names', async () => {
    renderApp({ role: 'admin', hash: '/admin/team?show=attention&person=sam' });
    await table();
    expect(screen.getByRole('tab', { name: /Needs you/ })).toHaveAttribute('aria-selected', 'true');
    expect(document.getElementById('person-sam')).toHaveClass('row-target');
  });

  it('offers yourself only your own details, and says why', async () => {
    renderApp({ role: 'admin', hash: '/admin/team' });
    const menu = await openMenu('Ada Okafor');
    expect(within(menu).getAllByRole('menuitem').map((m) => m.textContent)).toEqual([T.fields.menu]);
    expect(screen.getByText(T.menu.self)).toBeInTheDocument();
  });

  it('offers each action that applies to a person, and only those', async () => {
    renderApp({ role: 'admin', hash: '/admin/team' });
    const ben = await openMenu('Ben Carter');
    expect(within(ben).getAllByRole('menuitem').map((m) => m.textContent)).toEqual([
      T.roles.menu, T.fields.menu, T.reviewers.menu, 'Unlink Microsoft', 'Unlink Atlassian', 'Unlink GitHub', T.deactivate.menu,
    ]);
    await userEvent.keyboard('{Escape}');
    expect(screen.getByRole('button', { name: T.menu.button('Ben Carter') })).toHaveFocus();
    const tom = await openMenu('Tom Reyes');
    expect(within(tom).getAllByRole('menuitem').map((m) => m.textContent)).toContain(T.reactivate.menu);
    expect(within(tom).queryByRole('menuitem', { name: T.roles.menu })).not.toBeInTheDocument();
  });

  it('moves through the menu with the arrow keys', async () => {
    renderApp({ role: 'admin', hash: '/admin/team' });
    await openMenu('Ben Carter');
    const items = screen.getAllByRole('menuitem');
    expect(items[0]).toHaveFocus();
    await userEvent.keyboard('{ArrowDown}');
    expect(items[1]).toHaveFocus();
    await userEvent.keyboard('{End}');
    expect(items[items.length - 1]).toHaveFocus();
    await userEvent.keyboard('{ArrowDown}');
    expect(items[0]).toHaveFocus();
  });
});

describe('team: the auditor view', () => {
  it('makes sense without addresses or actions', async () => {
    renderApp({ role: 'auditor', hash: '/admin/team' });
    const t = await table();
    expect(screen.getByText(T.auditorView)).toBeInTheDocument();
    expect(within(t).queryByText(/@contoso\.example/)).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /^Actions for/ })).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: T.invite.button })).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /^Confirm/ })).not.toBeInTheDocument();
    expect(screen.queryByText(/deactivations left this hour/)).not.toBeInTheDocument();
    // The states still read: who is unconfirmed, who holds both.
    expect(within(await rowOf(/Sam Lee/)).getByText(T.signIn.unconfirmed)).toBeInTheDocument();
    expect(within(await rowOf(/Raj Mehta/)).getByText(T.dualRole)).toBeInTheDocument();
    expect(screen.getByRole('tab', { name: /Flagged/ })).toBeInTheDocument();
  });

  it('shows a calm page to a PM', async () => {
    renderApp({ role: 'pm', hash: '/admin/team' });
    expect(await screen.findByRole('heading', { level: 1, name: copy.admin.noAccess.title })).toBeInTheDocument();
  });
});

describe('team: display names are untrusted text', () => {
  it('renders markup in a display name as plain text', async () => {
    renderApp({ role: 'admin', hash: '/admin/team', hostile: true });
    const t = await table();
    const row = within(t).getByRole('row', { name: /Mallory/ });
    expect(row.querySelector('img')).toBeNull();
    expect(row.querySelector('b')).toBeNull();
    expect(row.textContent).toContain('<img src=x onerror="window.__pwned=1">**Mallory** <b>Admin</b>');
    expect((window as unknown as { __pwned?: number }).__pwned).toBeUndefined();
    // The menu and its dialogs use it as text too.
    await pick('<img src=x onerror="window.__pwned=1">**Mallory** <b>Admin</b>', T.deactivate.menu);
    expect(alertDialog().querySelector('img')).toBeNull();
    expect(within(alertDialog()).getByRole('heading', { level: 2 }).textContent).toBe(T.deactivate.heading('<img src=x onerror="window.__pwned=1">**Mallory** <b>Admin</b>'));
  });
});

describe('team: invite', () => {
  it('invites a person and shows them from the server answer', async () => {
    renderApp({ role: 'admin', hash: '/admin/team' });
    await table();
    await userEvent.click(screen.getByRole('button', { name: T.invite.button }));
    const d = dialog();
    expect(within(d).getByText(/Admin is not offered here/)).toBeInTheDocument();
    expect(within(d).queryByRole('checkbox', { name: /Admin/ })).not.toBeInTheDocument();
    const submit = within(d).getByRole('button', { name: T.invite.submit });
    expect(submit).toBeDisabled();
    await userEvent.type(within(d).getByLabelText(T.invite.handle), 'cy');
    await userEvent.type(within(d).getByLabelText(T.invite.displayName), 'Cy Young');
    await userEvent.type(within(d).getByLabelText(T.invite.signInName), 'cy@contoso.example');
    await userEvent.type(within(d).getByLabelText(T.invite.atlassianEmail), 'cy@contoso.example');
    await userEvent.click(within(d).getByRole('checkbox', { name: /Reviewer/ }));
    await userEvent.click(submit);
    await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument());
    expect(await screen.findByText(T.invite.done('Cy Young'))).toBeInTheDocument();
    const cy = await rowOf(/Cy Young/);
    expect(within(cy).getByText('Reviewer')).toBeInTheDocument();
    expect(within(cy).getByText(T.signIn.notYet)).toBeInTheDocument();
    expect(screen.getByRole('button', { name: T.invite.button })).toHaveFocus();
  });

  it('says plainly when the handle is taken, and keeps what was typed', async () => {
    renderApp({ role: 'admin', hash: '/admin/team' });
    await table();
    await userEvent.click(screen.getByRole('button', { name: T.invite.button }));
    const d = dialog();
    await userEvent.type(within(d).getByLabelText(T.invite.handle), 'ana');
    await userEvent.type(within(d).getByLabelText(T.invite.signInName), 'ana2@contoso.example');
    await userEvent.type(within(d).getByLabelText(T.invite.atlassianEmail), 'ana2@contoso.example');
    await userEvent.click(within(d).getByRole('button', { name: T.invite.submit }));
    expect(await within(d).findByRole('alert')).toHaveTextContent(T.errors.exists!);
    expect(within(d).getByLabelText(T.invite.handle)).toHaveValue('ana');
    expect(within(d).queryByText(/exists/)).not.toBeInTheDocument();
  });

  it('closes on Escape and gives the focus back', async () => {
    renderApp({ role: 'admin', hash: '/admin/team' });
    await table();
    await userEvent.click(screen.getByRole('button', { name: T.invite.button }));
    expect(within(dialog()).getByLabelText(T.invite.handle)).toHaveFocus();
    await userEvent.keyboard('{Escape}');
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
    await waitFor(() => expect(screen.getByRole('button', { name: T.invite.button })).toHaveFocus());
  });
});

describe('team: roles', () => {
  it('changes the whole set, and Admin can be removed but not given', async () => {
    renderApp({ role: 'admin', hash: '/admin/team' });
    await pick('Priya Nair', T.roles.menu);
    const d = dialog();
    expect(within(d).getByRole('checkbox', { name: /Admin/ })).toBeDisabled();
    expect(within(d).getByText(T.roles.adminHost)).toBeInTheDocument();
    expect(within(d).getByRole('button', { name: T.roles.submit })).toBeDisabled();
    await userEvent.click(within(d).getByRole('checkbox', { name: /Product Manager/ }));
    await userEvent.click(within(d).getByRole('button', { name: T.roles.submit }));
    expect(await screen.findByText(T.roles.done('Priya Nair', 'Product Manager and Reviewer'))).toBeInTheDocument();
    expect(within(await rowOf(/Priya Nair/)).getByText('Product Manager')).toBeInTheDocument();
  });

  it('lets an admin remove Admin from someone else', async () => {
    renderApp({ role: 'admin', hash: '/admin/team' });
    await pick('Raj Mehta', T.roles.menu);
    const d = dialog();
    const box = within(d).getByRole('checkbox', { name: /Admin/ });
    expect(box).toBeEnabled();
    await userEvent.click(box);
    await userEvent.click(within(d).getByRole('button', { name: T.roles.submit }));
    expect(await screen.findByText(T.roles.done('Raj Mehta', 'Auditor'))).toBeInTheDocument();
    expect(within(await rowOf(/Raj Mehta/)).queryByText(T.dualRole)).not.toBeInTheDocument();
  });

  it('reloads the list and says so when the team moved on (409 stale)', async () => {
    const reads = { n: 0 };
    renderApp({ role: 'admin', hash: '/admin/team', wrap: refuseOnce('adminSetRoles', new ApiError(409, 'stale', 'moved'), reads) });
    await pick('Priya Nair', T.roles.menu);
    const d = dialog();
    await userEvent.click(within(d).getByRole('checkbox', { name: /Product Manager/ }));
    const before = reads.n;
    await userEvent.click(within(d).getByRole('button', { name: T.roles.submit }));
    expect(await within(d).findByRole('alert')).toHaveTextContent(T.staleReloaded);
    expect(reads.n).toBe(before + 1);
    // The choice is kept, and the second try goes through.
    expect(within(d).getByRole('checkbox', { name: /Product Manager/ })).toBeChecked();
    await userEvent.click(within(d).getByRole('button', { name: T.roles.submit }));
    expect(await screen.findByText(T.roles.done('Priya Nair', 'Product Manager and Reviewer'))).toBeInTheDocument();
  });

  it('never says nothing changed when the answer was lost (unknown_outcome)', async () => {
    const reads = { n: 0 };
    renderApp({ role: 'admin', hash: '/admin/team', wrap: refuseOnce('adminSetRoles', new ApiError(502, 'unknown_outcome', 'lost'), reads) });
    await pick('Priya Nair', T.roles.menu);
    const d = dialog();
    await userEvent.click(within(d).getByRole('checkbox', { name: /Product Manager/ }));
    const before = reads.n;
    await userEvent.click(within(d).getByRole('button', { name: T.roles.submit }));
    const note = await within(d).findByRole('alert');
    expect(note).toHaveTextContent(T.unknownReloaded);
    expect(note.textContent).not.toMatch(/nothing changed/i);
    expect(reads.n).toBe(before + 1);
  });

  it('shows the refusal for Admin and Auditor without the exception', async () => {
    renderApp({ role: 'admin', hash: '/admin/team', pilotLapsed: true });
    expect(await screen.findByText(/The pilot exception ended on/)).toBeInTheDocument();
    await pick('Raj Mehta', T.roles.menu);
    const d = dialog();
    // Holding both now is no reason to warn; a change that keeps both is.
    expect(within(d).queryByText(T.roles.dualNo)).not.toBeInTheDocument();
    await userEvent.click(within(d).getByRole('checkbox', { name: /Product Manager/ }));
    expect(within(d).getByText(T.roles.dualNo)).toBeInTheDocument();
    await userEvent.click(within(d).getByRole('button', { name: T.roles.submit }));
    expect(await within(d).findByRole('alert')).toHaveTextContent(T.errors.dual_role!);
  });
});

describe('team: details', () => {
  it('saves only what changed, from the server answer', async () => {
    const sent: unknown[] = [];
    renderApp({
      role: 'admin',
      hash: '/admin/team',
      wrap: (api) => {
        const f = api.adminSetFields.bind(api);
        api.adminSetFields = async (h, fields, v) => {
          sent.push({ h, fields, v });
          return f(h, fields, v);
        };
        return api;
      },
    });
    await pick('Ravi Kumar', T.fields.menu);
    const d = dialog();
    const name = within(d).getByLabelText(T.invite.displayName);
    await userEvent.clear(name);
    await userEvent.type(name, 'Ravi K.');
    await userEvent.click(within(d).getByRole('button', { name: T.fields.submit }));
    expect(await screen.findByText(T.fields.done('Ravi K.'))).toBeInTheDocument();
    expect(sent).toEqual([{ h: 'ravi', fields: { display_name: 'Ravi K.' }, v: 42 }]);
  });

  it('shows a host-only person\u2019s addresses read-only with the reason', async () => {
    renderApp({ role: 'admin', hash: '/admin/team' });
    await pick('Raj Mehta', T.fields.menu);
    const d = dialog();
    expect(within(d).getByText(T.fields.hostOnly('Raj Mehta'))).toBeInTheDocument();
    expect(within(d).getByLabelText(T.invite.signInName)).toHaveAttribute('readonly');
    expect(within(d).getByLabelText(T.invite.displayName)).not.toHaveAttribute('readonly');
  });

  it('locks a linked account\u2019s address and says to unlink first', async () => {
    renderApp({ role: 'admin', hash: '/admin/team' });
    await pick('Ana Silva', T.fields.menu);
    const d = dialog();
    expect(within(d).getByLabelText(T.invite.atlassianEmail)).toHaveAttribute('readonly');
    expect(within(d).getByText(T.fields.atlassianLinked)).toBeInTheDocument();
    expect(within(d).getByLabelText(T.invite.githubLogin)).not.toHaveAttribute('readonly');
  });

  it('says plainly when an address belongs to someone else', async () => {
    renderApp({ role: 'admin', hash: '/admin/team' });
    await pick('Ravi Kumar', T.fields.menu);
    const d = dialog();
    const box = within(d).getByLabelText(T.invite.atlassianEmail);
    await userEvent.clear(box);
    await userEvent.type(box, 'ana@contoso.example');
    await userEvent.click(within(d).getByRole('button', { name: T.fields.submit }));
    expect(await within(d).findByRole('alert')).toHaveTextContent(T.errors.taken!);
  });

  it('refuses to clear a field before asking the server', async () => {
    renderApp({ role: 'admin', hash: '/admin/team' });
    await pick('Ravi Kumar', T.fields.menu);
    const d = dialog();
    await userEvent.clear(within(d).getByLabelText(T.invite.atlassianEmail));
    expect(within(d).getByText(T.fields.cantClear)).toBeInTheDocument();
    expect(within(d).getByRole('button', { name: T.fields.submit })).toBeDisabled();
  });
});

describe('team: confirm a sign-in name', () => {
  it('confirms from the row, and the row then reads not signed in yet', async () => {
    renderApp({ role: 'admin', hash: '/admin/team' });
    const sam = await rowOf(/Sam Lee/);
    await userEvent.click(within(sam).getByRole('button', { name: T.signIn.confirmFor('Sam Lee') }));
    const d = dialog();
    expect(within(d).getByLabelText(T.confirm.field)).toHaveValue('sam.lee@contoso.example');
    await userEvent.click(within(d).getByRole('button', { name: T.confirm.submit }));
    expect(await screen.findByText(T.confirm.done('Sam Lee'))).toBeInTheDocument();
    await waitFor(async () => expect(within(await rowOf(/Sam Lee/)).queryByText(T.signIn.unconfirmed)).not.toBeInTheDocument());
  });

  it('shows the refusal when it is not allowed', async () => {
    renderApp({ role: 'admin', hash: '/admin/team', usersError: 'assertion_required' });
    const sam = await rowOf(/Sam Lee/);
    await userEvent.click(within(sam).getByRole('button', { name: T.signIn.confirmFor('Sam Lee') }));
    await userEvent.click(within(dialog()).getByRole('button', { name: T.confirm.submit }));
    expect(await within(dialog()).findByRole('alert')).toHaveTextContent(T.errors.assertion_required!);
  });
});

describe('team: deactivate and reactivate', () => {
  it('says what happens, that it can be undone, and how many are left; then shows the server note', async () => {
    renderApp({ role: 'admin', hash: '/admin/team' });
    await pick('Ben Carter', T.deactivate.menu);
    const d = alertDialog();
    expect(within(d).getByText(T.deactivate.sessions)).toBeInTheDocument();
    expect(within(d).getByText(T.deactivate.accounts)).toBeInTheDocument();
    expect(within(d).getByText(T.deactivate.undo(24))).toBeInTheDocument();
    expect(within(d).getByText(T.deactivate.left(2, 3))).toBeInTheDocument();
    expect(within(d).queryByText(T.deactivate.admin)).not.toBeInTheDocument();
    // The safer button has the focus.
    expect(within(d).getByRole('button', { name: T.cancel })).toHaveFocus();
    await userEvent.click(within(d).getByRole('button', { name: T.deactivate.submit }));
    expect(await screen.findByText(T.deactivate.done('Ben Carter'))).toBeInTheDocument();
    expect(screen.getByText(/pmos-admin pause pm-ben/)).toBeInTheDocument();
    const ben = await rowOf(/Ben Carter/);
    expect(within(ben).getByText(/^Undo until/)).toBeInTheDocument();
    expect(screen.getByText(T.deactivationsLeft(1, 3))).toBeInTheDocument();
  });

  it('says Admin goes when an admin is deactivated', async () => {
    renderApp({ role: 'admin', hash: '/admin/team' });
    await pick('Raj Mehta', T.deactivate.menu);
    expect(within(alertDialog()).getByText(T.deactivate.admin)).toBeInTheDocument();
  });

  it('holds back when the person owns team bots, or the hourly cap is reached', async () => {
    const first = renderApp({ role: 'admin', hash: '/admin/team' });
    await pick('Ana Silva', T.deactivate.menu);
    expect(within(alertDialog()).getByText(T.deactivate.ownsBots('scout'))).toBeInTheDocument();
    expect(within(alertDialog()).getByRole('button', { name: T.deactivate.submit })).toBeDisabled();
    first.unmount();
    renderApp({ role: 'admin', hash: '/admin/team', deactivationCapReached: true });
    await pick('Ben Carter', T.deactivate.menu);
    expect(within(alertDialog()).getByText(T.deactivate.capped)).toBeInTheDocument();
    expect(within(alertDialog()).getByRole('button', { name: T.deactivate.submit })).toBeDisabled();
  });

  it('shows the cap refusal from the server in plain words', async () => {
    renderApp({ role: 'admin', hash: '/admin/team', usersError: 'deactivation_limit' });
    await pick('Ben Carter', T.deactivate.menu);
    await userEvent.click(within(alertDialog()).getByRole('button', { name: T.deactivate.submit }));
    expect(await within(alertDialog()).findByRole('alert')).toHaveTextContent(T.errors.deactivation_limit!);
  });

  it('reactivates within the grace, saying Admin is not restored', async () => {
    renderApp({ role: 'admin', hash: '/admin/team' });
    await pick('Tom Reyes', T.reactivate.menu);
    const d = dialog();
    expect(within(d).getByText(/Still inside the grace period/)).toBeInTheDocument();
    expect(within(d).getByText(T.reactivate.noAdmin)).toBeInTheDocument();
    await userEvent.click(within(d).getByRole('button', { name: T.reactivate.submit }));
    expect(await screen.findByText(T.reactivate.done('Tom Reyes'))).toBeInTheDocument();
    expect(within(await rowOf(/Tom Reyes/)).getByText(T.status.active!)).toBeInTheDocument();
  });

  it('says what is gone after the grace', async () => {
    renderApp({ role: 'admin', hash: '/admin/team' });
    await pick('Lena Fischer', T.reactivate.menu);
    expect(within(dialog()).getByText(/The grace period ended/)).toBeInTheDocument();
  });
});

describe('team: unlink and reviewers', () => {
  it('unlinks an account after a confirm', async () => {
    renderApp({ role: 'admin', hash: '/admin/team' });
    await pick('Ben Carter', 'Unlink GitHub');
    const d = alertDialog();
    expect(within(d).getByText(T.unlink.other('GitHub', 'GitHub login'))).toBeInTheDocument();
    await userEvent.click(within(d).getByRole('button', { name: T.unlink.submit('GitHub') }));
    expect(await screen.findByText(T.unlink.done('GitHub', 'Ben Carter'))).toBeInTheDocument();
    expect(within(await rowOf(/Ben Carter/)).getByText(T.accounts.notLinked('GitHub'))).toBeInTheDocument();
  });

  it('assigns PMs to a reviewer, and shows it on both sides', async () => {
    renderApp({ role: 'admin', hash: '/admin/team' });
    await pick('Priya Nair', T.reviewers.menu);
    const d = dialog();
    expect(within(d).getByRole('checkbox', { name: /Ravi Kumar/ })).toBeChecked();
    await userEvent.click(within(d).getByRole('checkbox', { name: /Sam Lee/ }));
    await userEvent.click(within(d).getByRole('button', { name: T.reviewers.submit }));
    expect(await screen.findByText(T.reviewers.done('Priya Nair', 2))).toBeInTheDocument();
    expect(within(await rowOf(/Priya Nair/)).getByText('Reviews ravi and sam')).toBeInTheDocument();
    await waitFor(async () => expect(within(await rowOf(/Sam Lee/)).getByText('Reviewed by priya')).toBeInTheDocument());
  });

  it('shows a reviewer refusal in plain words', async () => {
    const reads = { n: 0 };
    renderApp({ role: 'admin', hash: '/admin/team', wrap: refuseOnce('adminAssignReviewer', new ApiError(409, 'not_pm', 'x'), reads) });
    await pick('Priya Nair', T.reviewers.menu);
    const d = dialog();
    await userEvent.click(within(d).getByRole('checkbox', { name: /Sam Lee/ }));
    await userEvent.click(within(d).getByRole('button', { name: T.reviewers.submit }));
    expect(await within(d).findByRole('alert')).toHaveTextContent(T.errors.not_pm!);
  });
});

describe('team: every refusal has its own words', () => {
  // docs/web-api.md, "Admin: users and roles", "Errors": every code there has a sentence here.
  const doc = readFileSync(resolve(__dirname, '../../../../docs/web-api.md'), 'utf8');
  const section = doc.split(/^## Admin: users and roles$/m)[1]!.split(/^## /m)[0]!.split(/^### Errors$/m)[1]!.split(/^### /m)[0]!;
  const codes = [...new Set(section.split('\n').filter((l) => l.startsWith('| `')).flatMap((l) => [...l.split('|')[2]!.matchAll(/`([a-z_]+)`/g)].map((m) => m[1]!)))];

  it('finds the table', () => {
    expect(codes).toContain('self_change');
    expect(codes).toContain('deactivation_limit');
    expect(codes.length).toBeGreaterThan(20);
  });

  it('has a sentence for each code, never the code itself', () => {
    for (const code of codes) {
      const out = writeOutcome(new ApiError(code === 'unknown_outcome' ? 502 : 409, code, 'server words'));
      if (code === 'stale') expect(out.kind).toBe('stale');
      else if (code === 'unknown_outcome') expect(out.kind).toBe('unknown');
      else {
        expect(out, code).toEqual({ kind: 'refused', code, text: T.errors[code] });
        // A code with an underscore is never a word of English, so it must not show.
        if (code.includes('_')) expect(out.kind === 'refused' && out.text.includes(code), code).toBe(false);
      }
    }
  });

  it('treats a lost connection as unknown, never as refused', () => {
    expect(writeOutcome(new ApiError(0, 'network', 'x')).kind).toBe('unknown');
    expect(writeOutcome(new ApiError(504, 'unknown_outcome', 'x')).kind).toBe('unknown');
    expect(writeOutcome(new Error('boom')).kind).toBe('unknown');
  });
});
