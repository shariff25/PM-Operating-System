import { screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it } from 'vitest';
import { copy } from '../copy';
import { renderApp } from '../test/util';

describe('home', () => {
  it('asks in one box and shows the result as rendered markdown', async () => {
    const user = userEvent.setup();
    renderApp();
    await user.type(await screen.findByLabelText(copy.home.askLabel), 'what changed in billing');
    await user.click(screen.getByRole('button', { name: copy.home.askButton }));

    await screen.findByText(copy.task.status.done);
    const panel = screen.getByRole('region', { name: 'Your request' });
    expect(within(panel).getByText('PAY-212').tagName).toBe('STRONG');
    expect(within(panel).getAllByRole('listitem').length).toBeGreaterThan(2);
  });

  it('links to the draft when the task produced one', async () => {
    const user = userEvent.setup();
    renderApp();
    await user.type(await screen.findByLabelText(copy.home.askLabel), 'draft a PRD for grace periods');
    await user.click(screen.getByRole('button', { name: copy.home.askButton }));
    await user.click(await screen.findByRole('button', { name: copy.task.openDraft }));
    await screen.findByRole('heading', { name: copy.draft.askHeading });
    expect(window.location.hash).toMatch(/^#\/d\/apr_/);
  });

  it('says plainly why a task failed and offers Try again', async () => {
    const user = userEvent.setup();
    renderApp();
    await user.type(await screen.findByLabelText(copy.home.askLabel), 'this will fail');
    await user.click(screen.getByRole('button', { name: copy.home.askButton }));

    const alert = await screen.findByRole('alert');
    expect(alert).toHaveTextContent(/I could not reach Jira just now/);
    await user.click(within(alert).getByRole('button', { name: copy.task.tryAgain }));
    await screen.findByText(copy.task.status.done);
    expect(screen.queryByRole('alert')).not.toBeInTheDocument();
  });

  it('lists drafts that need a decision first, then expired ones, then decided ones', async () => {
    renderApp();
    const list = await screen.findByRole('region', { name: 'Drafts' });
    await within(list).findByText('PRD: TCB grace period setting');
    const links = within(list).getAllByRole('link');
    expect(links.length).toBeGreaterThanOrEqual(5);
    const badges = links.map((l) => within(l).getAllByText(/^(Needs you|Expired|Approved|Rejected|Failed)$/)[0]!.textContent);
    expect(badges).toEqual(['Needs you', 'Needs you', 'Needs you', 'Expired', 'Approved']);
    expect(links[3]).toHaveTextContent('Reply to Dana in #migrations');
    expect(links[4]).toHaveTextContent('Weekly update');
    expect(within(list).getByText(/Expires in \d+ h/)).toBeInTheDocument();
  });

  it('labels a draft whose time has passed as Expired even while the server still says pending', async () => {
    renderApp();
    const list = await screen.findByRole('region', { name: 'Drafts' });
    const row = (await within(list).findByText('Reply to Dana in #migrations')).closest('a')!;
    expect(within(row).getByText('Expired')).toHaveClass('badge-expired');
    expect(within(row).queryByText('Needs you')).not.toBeInTheDocument();
  });

  it('shows waiting-on lists from the brief and drafts a reply into the same panel', async () => {
    const user = userEvent.setup();
    renderApp();
    await screen.findByRole('heading', { name: copy.home.waitingOnYou });
    expect(screen.getByRole('heading', { name: copy.home.youWaitOn })).toBeInTheDocument();
    await user.click(screen.getAllByRole('button', { name: /Draft a reply/ })[0]!);
    await screen.findByRole('button', { name: copy.task.openDraft });
    expect(screen.getByText(/I made a draft for you/)).toBeInTheDocument();
  });

  it('removes a follow-up with Not mine', async () => {
    const user = userEvent.setup();
    renderApp();
    await screen.findByText('asked about the pricing page');
    await user.click(screen.getAllByRole('button', { name: /Not mine/ })[0]!);
    await waitFor(() => expect(screen.queryByText('asked about the pricing page')).not.toBeInTheDocument());
  });

  it('hides the brief entirely when it answers 501 not_built', async () => {
    renderApp({ briefBuilt: false });
    await screen.findByRole('region', { name: 'Drafts' });
    await screen.findByText('PRD: TCB grace period setting');
    expect(screen.queryByText(copy.home.waitingOnYou)).not.toBeInTheDocument();
    expect(screen.queryByText(copy.home.youWaitOn)).not.toBeInTheDocument();
  });

  it('greets a first sign-in that already has drafts', async () => {
    renderApp({ firstVisit: true });
    await screen.findByText(copy.welcomeWithDrafts);
  });

  it('shows the empty state when there are no drafts and it is not a first visit', async () => {
    renderApp({ noDrafts: true, firstVisit: false });
    await screen.findByText(copy.emptyDrafts);
  });

  it('shows only http and https follow-up links, and plain text for anything else', async () => {
    const user = userEvent.setup();
    renderApp({
      wrap: (api) => {
        const orig = api.brief.bind(api);
        api.brief = async () => {
          const b = await orig();
          b.waiting_on_you[0]!.url = 'javascript:alert(1)';
          b.changed[0]!.url = 'data:text/html,x';
          return b;
        };
        return api;
      },
    });
    await screen.findByText('asked about the pricing page');
    expect(document.querySelector('a[href^="javascript:"], a[href^="data:"]')).toBeNull();
    expect(screen.getAllByText(/not a web link/).length).toBe(2);
    await user.click(screen.getAllByRole('button', { name: /Not mine/ })[0]!);
  });

  it('shows the sign-in page when signed out, and signs in with a handle', async () => {
    const user = userEvent.setup();
    renderApp({ signedIn: false });
    await user.type(await screen.findByLabelText(copy.signIn.handleLabel), 'nobody');
    await user.click(screen.getByRole('button', { name: copy.signIn.submit }));
    expect(await screen.findByRole('alert')).toHaveTextContent(/not on the team/);
    await user.clear(screen.getByLabelText(copy.signIn.handleLabel));
    await user.type(screen.getByLabelText(copy.signIn.handleLabel), 'ana');
    await user.click(screen.getByRole('button', { name: copy.signIn.submit }));
    await screen.findByLabelText(copy.home.askLabel);
  });
});
