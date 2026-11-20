import AxeBuilder from '@axe-core/playwright';
import { expect, test } from '@playwright/test';
import type { Page } from '@playwright/test';

// axe runs with every rule it ships (no rules are switched off) on every page, in both themes.
// A state is reached the way a person would reach it, then scanned.

const EMPTY = 'http://127.0.0.1:5192';
const BRIEF_OFF = 'http://127.0.0.1:5193';

async function signIn(page: Page, theme: 'light' | 'dark', base?: string, handle = 'ana', search = '') {
  await page.addInitScript((t) => {
    try {
      window.localStorage.setItem('pmos-theme', t);
    } catch {
      /* the page still loads */
    }
  }, theme);
  await page.goto((base ?? '') + '/' + search);
  await page.getByLabel('Your handle').fill(handle);
  await page.getByRole('button', { name: 'Sign in', exact: true }).click();
  await expect(page.getByLabel('What do you need?')).toBeVisible();
  await expect(page.locator('html')).toHaveAttribute('data-theme', theme);
}

async function go(page: Page, hash: string) {
  await page.evaluate((h) => { window.location.hash = h; }, hash);
}

async function scan(page: Page, what: string) {
  // Let entrance animations finish, so colours are final when they are measured.
  await page.waitForTimeout(350);
  const results = await new AxeBuilder({ page }).analyze();
  const summary = results.violations.map((v) => ({
    rule: v.id,
    impact: v.impact,
    help: v.help,
    nodes: v.nodes.map((n) => `${n.target.join(' ')} :: ${n.failureSummary?.split('\n').slice(0, 3).join(' | ')}`),
  }));
  expect(summary, `axe violations on ${what}`).toEqual([]);
}

for (const theme of ['light', 'dark'] as const) {
  test.describe(`axe, ${theme} theme`, () => {
    // Each of these walks a whole screen through many states and scans every one. On a
    // slow runner that takes longer than the default 30 s, so they get three times as long.
    test.slow();

    test('sign-in with each set of providers, and the signed-out page', async ({ page }) => {
      await page.addInitScript((t) => {
        try {
          window.localStorage.setItem('pmos-theme', t);
        } catch {
          /* the page still loads */
        }
      }, theme);
      const open = async (providers: string, hash = '') => {
        await page.goto(`/?mock_providers=${providers}${hash ? `#${hash}` : ''}`);
        await page.reload();
        await expect(page.locator('html')).toHaveAttribute('data-theme', theme);
      };
      await open('dev');
      await expect(page.getByLabel('Your handle')).toBeVisible();
      await scan(page, 'sign-in, development only');
      await open('entra');
      await expect(page.getByRole('link', { name: 'Sign in with Microsoft' })).toBeVisible();
      await scan(page, 'sign-in, Microsoft only');
      await open('entra,dev');
      await expect(page.getByLabel('Your handle')).toBeVisible();
      await scan(page, 'sign-in, both');
      await open('');
      await expect(page.getByText("Sign-in isn't set up yet.")).toBeVisible();
      await scan(page, 'sign-in, nothing set up');
      for (const reason of ['not_a_member', 'cancelled', 'expired', 'unavailable', 'failed']) {
        await open('entra', `/signed-out?reason=${reason}`);
        await expect(page.getByRole('heading', { level: 1, name: 'You are signed out' })).toBeVisible();
        await scan(page, `signed out, ${reason}`);
      }
      await page.setViewportSize({ width: 390, height: 844 });
      await open('entra,dev');
      await expect(page.getByRole('link', { name: 'Sign in with Microsoft' })).toBeVisible();
      await scan(page, 'phone sign-in, both');
      await open('entra', '/signed-out?reason=not_a_member');
      await expect(page.getByRole('button', { name: 'Try again' })).toBeVisible();
      await scan(page, 'phone signed out');
      await page.getByRole('button', { name: 'Try again' }).click();
      await expect(page.getByRole('link', { name: 'Sign in with Microsoft' })).toBeVisible();
    });

    test('home, drafts, follow-ups and activity', async ({ page }) => {
      await signIn(page, theme);
      await expect(page.getByRole('heading', { level: 1 })).toBeVisible();
      await scan(page, 'home');

      await go(page, '/drafts');
      await expect(page.getByRole('heading', { level: 1, name: 'Drafts' })).toBeVisible();
      await scan(page, 'drafts (needs you)');
      await page.getByRole('tab', { name: /^All/ }).click();
      await scan(page, 'drafts (all)');

      await go(page, '/followups');
      await expect(page.getByRole('heading', { level: 1, name: 'Follow-ups' })).toBeVisible();
      await scan(page, 'follow-ups');

      await go(page, '/activity');
      await expect(page.getByRole('heading', { level: 1, name: 'Activity' })).toBeVisible();
      await scan(page, 'activity');
    });

    test('threads: working, done, failed and with a draft', async ({ page }) => {
      await signIn(page, theme);
      await page.getByLabel('What do you need?').fill('this is busy, what changed?');
      await page.getByLabel('What do you need?').press('Enter');
      await expect(page.getByRole('region', { name: 'Conversation' })).toBeVisible();
      await scan(page, 'thread, working');
      await expect(page.getByRole('region', { name: 'Conversation' }).getByText('Done', { exact: true })).toBeVisible();
      await page.getByRole('button', { name: /Show steps/ }).click();
      await scan(page, 'thread, done, steps open');

      await go(page, '/a/tsk_seed_failed');
      await expect(page.getByText('That did not work')).toBeVisible();
      await scan(page, 'thread, failed');

      await go(page, '/a/tsk_seed_prd');
      await expect(page.getByRole('button', { name: 'Open draft' })).toBeVisible();
      await scan(page, 'thread, with a draft');
    });

    test('a draft: hints, every tab, a before and after, the approve dialog', async ({ page }) => {
      await signIn(page, theme);
      await go(page, '/d/apr_prd');
      await expect(page.getByRole('article', { name: 'Formatted draft' })).toBeVisible();
      for (const hint of ['sources', 'ask', 'approve']) {
        await expect(page.locator('[data-hint]')).toHaveAttribute('data-hint', hint);
        await scan(page, `draft with the ${hint} hint`);
        await page.locator('[data-hint]').getByRole('button', { name: 'Got it' }).click();
      }
      // The numbered markers: at rest, with a title on hover and on focus, and with a source lit up.
      const doc = page.getByRole('article', { name: 'Formatted draft' });
      await expect(doc.getByRole('button', { name: 'Source 2: PAY-212: Payment failure handling' }).first()).toBeVisible();
      await scan(page, 'draft with source markers');
      await doc.getByRole('button', { name: /^Source 1:/ }).first().hover();
      await expect(page.locator('.cite-tip')).toHaveText('TCB grace period');
      await scan(page, 'draft, a marker hovered');
      await doc.getByRole('button', { name: /^Source 4:/ }).first().focus();
      await expect(page.locator('.cite-tip')).toHaveText('Slack thread');
      await scan(page, 'draft, a marker focused');
      await page.keyboard.press('Enter');
      await expect(page.getByRole('button', { name: /^Source 4:/ }).and(page.locator('.source'))).toBeFocused();
      await expect(doc.locator('button.cite-on')).toHaveCount(2);
      await scan(page, 'draft, a source lit up from its marker');
      await page.getByRole('button', { name: /PAY-212/ }).and(page.locator('.source')).click();
      await scan(page, 'draft, a source selected');
      await page.getByLabel('What should change?').fill('shorter');
      await page.getByRole('button', { name: 'Suggest a change' }).click();
      const region = page.getByRole('region', { name: 'Suggested change' });
      await expect(region).toBeVisible();
      await scan(page, 'draft, a suggestion in the document');
      await region.getByRole('button', { name: 'Accept change' }).click();
      await page.getByRole('button', { name: 'Reject draft' }).click();
      await scan(page, 'draft, reject reasons');
      await page.keyboard.press('Escape');
      await page.getByRole('button', { name: 'Edit', exact: true }).click();
      await scan(page, 'draft, editing');
      await page.getByRole('button', { name: 'Discard changes' }).click();
      await page.getByRole('button', { name: 'Approve to Confluence' }).click();
      await expect(page.getByRole('dialog')).toBeVisible();
      await scan(page, 'approve dialog');
      await page.getByRole('button', { name: 'Confirm and create' }).click();
      await expect(page.getByText('Created in Space PM, under “Specs”.')).toBeVisible();
      await scan(page, 'draft, approved');
    });

    test('drafts without markers: sources not linked, and one the PM edited', async ({ page }) => {
      await signIn(page, theme);
      await go(page, '/d/apr_jira');
      await expect(page.getByText('No sentence is linked to a source')).toBeVisible();
      await scan(page, 'draft with sources and no markers');
      await go(page, '/d/apr_prd');
      await page.getByRole('button', { name: 'Edit', exact: true }).click();
      await page.getByLabel('Title').fill('PRD: grace period, second pass');
      await page.getByRole('button', { name: 'Save', exact: true }).click();
      await expect(page.getByText('You edited this draft, so source links were removed')).toBeVisible();
      await expect(page.locator('button.cite')).toHaveCount(0);
      await scan(page, 'draft the PM edited');
    });

    test('approve dialogs for a Jira issue and a Slack reply', async ({ page }) => {
      await signIn(page, theme);
      await go(page, '/d/apr_jira');
      await page.getByRole('button', { name: 'Approve to Jira' }).click();
      await expect(page.getByRole('dialog').getByText(/^This will create a Jira issue in /)).toBeVisible();
      await scan(page, 'approve dialog, Jira');
    });

    test('home before any brief has run', async ({ page }) => {
      await signIn(page, theme, undefined, 'ana', '?mock_brief=never');
      await expect(page.getByText(/No brief yet\. Want a morning brief/)).toBeVisible();
      await scan(page, 'home, no brief yet');
      await go(page, '/followups');
      await expect(page.getByText(/your first one has not arrived/)).toBeVisible();
      await scan(page, 'follow-ups, no brief yet');
    });

    test('other drafts: external Slack, expired, practice, hostile, no sources', async ({ page }) => {
      await signIn(page, theme);
      for (const id of ['apr_slack', 'apr_slack_old', 'apr_hostile']) {
        await go(page, `/d/${id}`);
        await expect(page.getByRole('article', { name: 'Formatted draft' })).toBeVisible();
        await scan(page, `draft ${id}`);
      }
      await go(page, '/d/apr_slack');
      await page.getByRole('button', { name: 'Approve to Slack' }).click();
      await scan(page, 'external approve dialog');
    });

    test('home with a running request, home without the brief, and the icon rail', async ({ page }) => {
      await page.setViewportSize({ width: 1440, height: 900 });
      await signIn(page, theme);
      await page.getByLabel('What do you need?').fill('this is busy, what changed?');
      await page.getByLabel('What do you need?').press('Enter');
      await expect(page.getByRole('region', { name: 'Conversation' })).toBeVisible();
      await go(page, '/');
      await expect(page.getByRole('region', { name: 'Needs your decision' }).getByRole('progressbar')).toBeVisible();
      await scan(page, 'home, a request running');
      await signIn(page, theme, BRIEF_OFF);
      await expect(page.getByRole('region', { name: 'Today in numbers' })).toContainText('Running now');
      await scan(page, 'home, brief not built');
      await go(page, '/d/apr_prd');
      await expect(page.locator('header.rail')).toHaveClass(/rail-compact/);
      await scan(page, 'draft with the icon rail');
    });

    test('palette and user menu', async ({ page }) => {
      await signIn(page, theme);
      await page.keyboard.press('Control+k');
      await expect(page.getByRole('dialog', { name: 'Ask PM OS or jump to a page' })).toBeVisible();
      await scan(page, 'palette');
      await page.keyboard.type('what is');
      await scan(page, 'palette with a query');
      await page.keyboard.press('Escape');
      await page.getByRole('button', { name: /Menu for/ }).click();
      await scan(page, 'user menu');
    });

    test('Nudge: the welcome card, a hint, a calm empty state, the finish and the Show Nudge control', async ({ page }) => {
      await signIn(page, theme, undefined, 'ana', '?mock_new=1');
      const welcome = page.getByRole('region', { name: "Hi Ana, I'm Nudge." });
      await expect(welcome).toBeVisible();
      await expect(welcome.locator('svg[data-nudge="hello"]')).toBeVisible();
      await scan(page, 'welcome card');
      await page.getByRole('link', { name: /Practice: why PMs should take the stairs/ }).click();
      await expect(page.locator('[data-hint="sources"] svg[data-nudge="nudge"]')).toBeVisible();
      await scan(page, 'a hint with Nudge');
      await page.getByRole('button', { name: 'Approve to Confluence' }).click();
      await page.getByRole('dialog').getByRole('button', { name: /^Confirm/ }).click();
      await expect(page.getByText('Approved!', { exact: true })).toBeVisible();
      await scan(page, 'the Approved! moment');
      await page.getByRole('button', { name: /Menu for/ }).click();
      await expect(page.getByRole('menuitemcheckbox', { name: /Show Nudge/ })).toBeVisible();
      await scan(page, 'user menu with Show Nudge');
      await page.getByRole('menuitemcheckbox', { name: /Show Nudge/ }).click();
      await expect(page.getByRole('menuitemcheckbox', { name: /Show Nudge/ })).toHaveAttribute('aria-checked', 'false');
      await scan(page, 'user menu, Show Nudge off');
    });

    test('Nudge: calm empty states and the sign-in screen', async ({ page }) => {
      await page.addInitScript((t) => window.localStorage.setItem('pmos-theme', t), theme);
      await page.goto(EMPTY + '/');
      await expect(page.locator('.signin svg[data-nudge="hello"]')).toBeVisible();
      await scan(page, 'sign-in with Nudge');
      await page.getByLabel('Your handle').fill('ana');
      await page.getByRole('button', { name: 'Sign in', exact: true }).click();
      await expect(page.locator('svg[data-nudge="calm"]')).toBeVisible();
      await scan(page, 'calm Today');
      await go(page, '/drafts');
      await expect(page.locator('.empty svg[data-nudge="calm"]')).toBeVisible();
      await scan(page, 'calm Drafts');
    });

    test('a brand-new account: every empty state', async ({ page }) => {
      await signIn(page, theme, EMPTY);
      await scan(page, 'home, new account');
      for (const [hash, heading] of [['/drafts', 'Drafts'], ['/followups', 'Follow-ups'], ['/activity', 'Activity']] as const) {
        await go(page, hash);
        await expect(page.getByRole('heading', { level: 1, name: heading })).toBeVisible();
        await expect(page.locator('.empty')).toBeVisible();
        await scan(page, `${heading}, empty`);
      }
    });

    test('phone: home, a draft with the banner hint, the More menu, and a thread', async ({ page }) => {
      await page.setViewportSize({ width: 390, height: 844 });
      await signIn(page, theme);
      await scan(page, 'phone home');
      await go(page, '/d/apr_prd');
      await expect(page.locator('[data-hint]')).toHaveClass(/coach-banner/);
      await scan(page, 'phone draft with the hint banner');
      await page.locator('[data-hint]').getByRole('button', { name: 'Got it' }).click();
      await scan(page, 'phone draft with source markers');
      await page.locator('.sheet button.cite').first().click();
      await expect(page.getByRole('tab', { name: /^Sources/ })).toHaveAttribute('aria-selected', 'true');
      await scan(page, 'phone draft, a marker opened its source');
      await page.getByRole('button', { name: 'More' }).click();
      await scan(page, 'phone More menu');
      await page.keyboard.press('Escape');
      await page.getByRole('tab', { name: 'Details' }).click();
      await scan(page, 'phone draft details and checklist');
      await page.getByRole('tab', { name: /^Sources/ }).click();
      await scan(page, 'phone draft sources');
      await go(page, '/a/tsk_seed_week');
      await expect(page.getByRole('region', { name: 'Conversation' })).toBeVisible();
      await scan(page, 'phone thread');
    });

    test('admin: overview, audit log (open row, empty, filtered), team, connectors, as admin', async ({ page }) => {
      await signIn(page, theme, undefined, 'ada');
      await go(page, '/admin');
      await expect(page.getByRole('heading', { level: 1, name: 'Overview' })).toBeVisible();
      await expect(page.getByRole('region', { name: 'Recent denied actions' }).getByRole('listitem').first()).toBeVisible();
      await expect(page.getByRole('region', { name: 'Recent approvals' }).getByRole('listitem').first()).toBeVisible();
      await scan(page, 'admin overview');
      await go(page, '/admin/audit');
      await expect(page.getByRole('table', { name: 'Audit events' })).toBeVisible();
      await scan(page, 'audit log');
      await page.getByRole('button', { name: /^Show details for/ }).first().click();
      await expect(page.getByRole('group', { name: 'Every field of this event' })).toBeVisible();
      await scan(page, 'audit log, row open');
      await go(page, '/admin/audit?actor=nobody');
      await expect(page.getByText('Nothing matches these filters.')).toBeVisible();
      await scan(page, 'audit log, nothing matches');
      await go(page, '/admin/audit?from=2026-11-10&to=2026-11-01');
      await expect(page.getByRole('alert')).toBeVisible();
      await scan(page, 'audit log, backwards dates');
      await go(page, '/admin/team');
      await expect(page.getByRole('table', { name: 'Team members' })).toBeVisible();
      await scan(page, 'team');
      await go(page, '/admin/connectors');
      await expect(page.getByRole('table', { name: 'Connectors' })).toBeVisible();
      await scan(page, 'connectors');
    });

    test('admin: the Team page, its menu and every dialog, a refusal and a success', async ({ page }) => {
      await signIn(page, theme, undefined, 'ada', '?mock_deactivations=capped');
      await go(page, '/admin/team');
      await expect(page.getByRole('table', { name: 'Team members' })).toBeVisible();
      await page.getByRole('button', { name: 'Actions for Ben Carter' }).click();
      await expect(page.getByRole('menu', { name: 'Actions for Ben Carter' })).toBeVisible();
      await scan(page, 'team, action menu');
      await page.keyboard.press('Escape');
      await page.getByRole('button', { name: 'Actions for Ada Okafor' }).click();
      await expect(page.getByText(/Nobody changes their own roles/)).toBeVisible();
      await scan(page, 'team, own action menu');
      await page.keyboard.press('Escape');

      const each: [string, string, RegExp][] = [
        ['Ben Carter', 'Change roles', /^Roles for/],
        ['Raj Mehta', 'Edit details', /^Details for/],
        ['Ben Carter', 'Assign PMs to review', /^PMs Ben Carter reviews/],
        ['Tom Reyes', 'Reactivate', /^Reactivate/],
      ];
      for (const [who, item, name] of each) {
        await page.getByRole('button', { name: `Actions for ${who}` }).click();
        await page.getByRole('menuitem', { name: item }).click();
        await expect(page.getByRole('dialog', { name })).toBeVisible();
        await scan(page, `team, ${item} dialog`);
        await page.keyboard.press('Escape');
      }
      for (const [who, item] of [['Ben Carter', 'Deactivate'], ['Ben Carter', 'Unlink GitHub']] as const) {
        await page.getByRole('button', { name: `Actions for ${who}` }).click();
        await page.getByRole('menuitem', { name: item }).click();
        await expect(page.getByRole('alertdialog')).toBeVisible();
        await scan(page, `team, ${item} dialog`);
        await page.keyboard.press('Escape');
      }
      await page.getByRole('button', { name: 'Confirm the sign-in name for Sam Lee' }).click();
      await expect(page.getByRole('dialog', { name: /^Confirm the sign-in name/ })).toBeVisible();
      await scan(page, 'team, confirm sign-in dialog');
      await page.keyboard.press('Escape');

      await page.getByRole('button', { name: 'Invite a person' }).click();
      const invite = page.getByRole('dialog', { name: 'Invite a person' });
      await scan(page, 'team, invite dialog');
      await invite.getByLabel('Handle').fill('ana');
      await invite.getByLabel('Microsoft sign-in name').fill('ana2@contoso.example');
      await invite.getByLabel('Atlassian email').fill('ana2@contoso.example');
      await invite.getByRole('button', { name: 'Invite', exact: true }).click();
      await expect(invite.getByRole('alert')).toBeVisible();
      await scan(page, 'team, invite refused');
      await invite.getByLabel('Handle').fill('cy');
      await invite.getByLabel('Microsoft sign-in name').fill('cy@contoso.example');
      await invite.getByLabel('Atlassian email').fill('cy@contoso.example');
      await invite.getByRole('button', { name: 'Invite', exact: true }).click();
      await expect(invite).toBeHidden();
      await scan(page, 'team, after an invite');

      await page.getByRole('tab', { name: /Needs you/ }).click();
      await scan(page, 'team, needs you');

      await go(page, '/admin');
      await expect(page.getByRole('region', { name: /People who need an admin/ })).toBeVisible();
      await scan(page, 'overview with people flags and the cap notice');
    });

    test('admin: the overview once the pilot exception has lapsed, and the Team page on a phone', async ({ page }) => {
      await signIn(page, theme, undefined, 'ada', '?mock_pilot=lapsed');
      await go(page, '/admin');
      await expect(page.getByText(/^The pilot exception ended on/)).toBeVisible();
      await scan(page, 'overview, pilot exception lapsed');
      await go(page, '/admin/team');
      await expect(page.getByText(/^The pilot exception ended on/)).toBeVisible();
      await scan(page, 'team, pilot exception lapsed');
      await page.setViewportSize({ width: 390, height: 844 });
      await expect(page.getByRole('table', { name: 'Team members' })).toBeVisible();
      await scan(page, 'phone team');
    });

    test('admin: auditor views and the no-access page', async ({ page }) => {
      await signIn(page, theme, undefined, 'audrey');
      await go(page, '/admin');
      await expect(page.getByRole('heading', { level: 1, name: 'Overview' })).toBeVisible();
      await expect(page.getByRole('region', { name: 'Recent denied actions' }).getByRole('listitem').first()).toBeVisible();
      await scan(page, 'auditor overview');
      await go(page, '/admin/audit');
      await expect(page.getByRole('table', { name: 'Audit events' })).toBeVisible();
      await scan(page, 'auditor audit log');
      await go(page, '/admin/team');
      await expect(page.getByText(/You're seeing the auditor's view/)).toBeVisible();
      await scan(page, 'auditor team');
      await go(page, '/admin/connectors');
      await expect(page.getByRole('heading', { level: 1, name: "You don't have access to this" })).toBeVisible();
      await scan(page, 'auditor, no access');
    });

    test('admin: policy, its check result, the pull-request dialog, a refusal and the result', async ({ page }) => {
      await signIn(page, theme, undefined, 'ada', '?mock_proposal=stale_base');
      await go(page, '/admin/policy');
      await expect(page.getByRole('heading', { name: /^What each bot may do/ })).toBeVisible();
      await scan(page, 'policy overview, cards collapsed');
      await page.getByRole('button', { name: 'Expand all' }).click();
      await page.getByText('Fixed in code').click();
      await page.getByText('Policy files').click();
      await page.getByText('Show how policies/bots/pm-ben.yaml differs').click();
      await page.getByText('Read it as sentences').first().click();
      await scan(page, 'policy, everything open');
      await go(page, '/admin/policy/proposals');
      await expect(page.getByRole('list', { name: 'Open policy proposals' })).toBeVisible();
      await scan(page, 'policy, proposals');
      await go(page, '/admin/policy/propose');
      await page.getByRole('tab', { name: 'Roles' }).click();
      await scan(page, 'policy editor, roles');
      await page.getByRole('tab', { name: 'JSON' }).click();
      await scan(page, 'policy editor, JSON');
      await page.getByRole('tab', { name: 'Skills' }).click();
      await page.getByLabel('Tools for docs').fill('docs.search docs.read jira.search');
      await page.getByRole('button', { name: 'Check this change' }).click();
      await expect(page.getByRole('region', { name: 'Wider: 1' })).toBeVisible();
      await page.getByText('Files this change writes (2)').click();
      await page.locator('summary', { hasText: 'policies/bots/docs.yaml' }).click();
      await scan(page, 'policy check result with a widening');
      await page.getByRole('button', { name: 'Open a pull request' }).click();
      const dialog = page.getByRole('dialog', { name: 'Open a pull request' });
      await scan(page, 'pull-request dialog');
      await dialog.getByLabel('Title').fill('Let the docs bot search Jira');
      await dialog.getByLabel('Why this change').fill('Support asked.');
      await dialog.getByRole('button', { name: 'Open the pull request' }).click();
      await expect(dialog.getByRole('alert')).toBeVisible();
      await scan(page, 'pull-request dialog, stale_base');
    });

    test('admin: a pull request opened, and GitHub not linked', async ({ page }) => {
      await signIn(page, theme, undefined, 'ada');
      await go(page, '/admin/policy/propose');
      await page.getByRole('tab', { name: 'Bots' }).click();
      await page.getByLabel('Owner of docs').fill('ana');
      await page.getByRole('button', { name: 'Check this change' }).click();
      await page.getByRole('button', { name: 'Open a pull request' }).click();
      const dialog = page.getByRole('dialog', { name: 'Open a pull request' });
      await dialog.getByLabel('Title').fill('Give the docs bot an owner');
      await dialog.getByLabel('Why this change').fill('Ana runs the docs now.');
      await dialog.getByRole('button', { name: 'Open the pull request' }).click();
      await expect(page.getByRole('heading', { name: 'Pull request #42 is open' })).toBeVisible();
      await scan(page, 'pull request opened');
      await signIn(page, theme, undefined, 'ada', '?mock_protection=not_linked');
      await go(page, '/admin/policy/proposals');
      await expect(page.getByText('Link GitHub to propose changes')).toBeVisible();
      await expect(page.getByText(/Link your GitHub account to see proposals/)).toBeVisible();
      await scan(page, 'policy, GitHub not linked');
    });

    test('admin: content tree, a file, the editor, the change tray with a check, and the harness', async ({ page }) => {
      await signIn(page, theme, undefined, 'ada');
      await go(page, '/admin/content');
      await expect(page.getByRole('table', { name: 'Bots in the harness' })).toBeVisible();
      await page.getByText('Model roles').click();
      await scan(page, 'content tree and harness');
      await go(page, '/admin/content?file=content%2Fskills%2Fdocs%2FSKILL.md');
      await expect(page.getByText('Front matter')).toBeVisible();
      await scan(page, 'content file, rendered');
      await page.getByRole('radio', { name: 'Raw' }).click();
      await scan(page, 'content file, raw');
      await page.getByRole('button', { name: 'Edit', exact: true }).click();
      const editor = page.getByLabel('Text of content/skills/docs/SKILL.md');
      await editor.fill((await editor.inputValue()).replace('tools: "docs.search docs.read"', 'tools: "docs.search docs.read slack.search"'));
      await scan(page, 'content editor');
      await page.getByRole('button', { name: 'New file' }).click();
      await scan(page, 'content, new file form');
      await page.getByLabel('Path', { exact: true }).fill('content/playbooks/handoff.md');
      await page.getByRole('button', { name: 'Add to the change' }).click();
      const tray = page.getByRole('region', { name: 'Changes to propose' });
      await tray.getByRole('button', { name: 'Check these changes' }).click();
      await expect(tray.getByRole('heading', { name: 'Check result' })).toBeVisible();
      await scan(page, 'content tray with a check result');
    });

    test('admin: policy and content as an auditor, with locked files', async ({ page }) => {
      await signIn(page, theme, undefined, 'audrey');
      await go(page, '/admin/policy');
      await expect(page.getByRole('heading', { name: /^What each bot may do/ })).toBeVisible();
      await scan(page, 'auditor policy');
      await go(page, '/admin/content');
      await expect(page.getByRole('navigation', { name: 'Content files' })).toBeVisible();
      await scan(page, 'auditor content with locked files');
      await go(page, '/admin/content?file=content%2Fcontext%2Fproducts%2Froadmap.md');
      await expect(page.getByRole('navigation', { name: 'Content files' })).toBeVisible();
      await scan(page, 'auditor following a link to a locked file');
    });

    test('admin on a phone: policy and content', async ({ page }) => {
      await page.setViewportSize({ width: 390, height: 844 });
      await signIn(page, theme, undefined, 'ada');
      await go(page, '/admin/policy');
      await expect(page.getByRole('heading', { name: /^What each bot may do/ })).toBeVisible();
      await scan(page, 'phone policy');
      await go(page, '/admin/content?file=content%2Fskills%2Fdocs%2FSKILL.md');
      await expect(page.getByText('Front matter')).toBeVisible();
      await scan(page, 'phone content file');
    });

    test('admin on a phone: every part', async ({ page }) => {
      await page.setViewportSize({ width: 390, height: 844 });
      await signIn(page, theme, undefined, 'ada');
      for (const [hash, heading] of [['/admin', 'Overview'], ['/admin/audit', 'Audit log'], ['/admin/team', 'Team'], ['/admin/connectors', 'Connectors']] as const) {
        await go(page, hash);
        await expect(page.getByRole('heading', { level: 1, name: heading })).toBeVisible();
        if (heading === 'Overview') await expect(page.getByRole('region', { name: 'Recent approvals' }).getByRole('listitem').first()).toBeVisible();
        await scan(page, `phone admin ${heading}`);
      }
      await go(page, '/admin/audit');
      await page.getByRole('button', { name: /^Show details for/ }).first().click();
      await scan(page, 'phone audit log, row open');
    });

    test('reviews: the inbox, a draft and its panel, a recorded and a changed recommendation, restricted, own and refused', async ({ page }) => {
      await signIn(page, theme, undefined, 'ben');
      await go(page, '/reviews');
      await expect(page.getByRole('list', { name: 'Reviews' }).getByRole('link').first()).toBeVisible();
      await scan(page, 'reviews inbox');
      await go(page, '/reviews/apr_dana_onboarding');
      await expect(page.getByRole('region', { name: 'Your advice' })).toBeVisible();
      await scan(page, 'review page with the panel');
      await page.getByLabel('Note for dana').fill('x'.repeat(1001));
      await expect(page.getByText('Keep the note to 1000 characters.', { exact: false })).toBeVisible();
      await scan(page, 'review panel, note too long');
      await page.getByLabel('Note for dana').fill('Looks right.');
      await page.getByRole('button', { name: 'Recommend approving' }).click();
      await expect(page.getByRole('region', { name: 'Your advice' }).getByText('You recommend approving', { exact: true })).toBeVisible();
      await scan(page, 'review panel, recorded');
      await go(page, '/reviews/apr_dana_exports');
      await page.getByLabel('Note for dana').fill('Say who gets the email.');
      await page.getByRole('button', { name: 'Suggest changes' }).click();
      await expect(page.getByText(/changed this draft after you opened it/)).toBeVisible();
      await scan(page, 'review panel, the draft changed');
      await go(page, '/reviews/apr_prd');
      await expect(page.getByText('This draft uses restricted sources; its owner can share it with you.')).toBeVisible();
      await scan(page, 'review of a restricted draft');
      await go(page, '/reviews/apr_ben_own');
      await expect(page.getByText(/This is your own draft/)).toBeVisible();
      await scan(page, 'review of their own draft');
      await page.setViewportSize({ width: 390, height: 844 });
      await go(page, '/reviews');
      await expect(page.getByRole('list', { name: 'Reviews' }).getByRole('link').first()).toBeVisible();
      await scan(page, 'phone reviews inbox');
      await go(page, '/reviews/apr_jira');
      await expect(page.getByRole('region', { name: 'Your advice' })).toBeVisible();
      await scan(page, 'phone review page');
    });

    test('reviews: the owner’s Reviews block, stale, and the approve dialog with the advice', async ({ page }) => {
      await signIn(page, theme);
      await go(page, '/d/apr_jira');
      await expect(page.getByRole('region', { name: 'Reviews' })).toBeVisible();
      await page.getByRole('button', { name: 'Edit', exact: true }).click();
      await page.getByLabel('Draft text').fill('Show the banner in the admin’s time zone.');
      await page.getByRole('button', { name: 'Save', exact: true }).click();
      await expect(page.getByRole('region', { name: 'Reviews' }).getByText('Earlier version', { exact: true })).toBeVisible();
      await scan(page, 'owner Reviews block, stale');
      await page.getByRole('button', { name: 'Approve to Jira' }).click();
      await expect(page.getByRole('dialog').getByText('Ben Carter suggested changes on an earlier version')).toBeVisible();
      await scan(page, 'approve dialog with advice');
      await page.keyboard.press('Escape');
      await go(page, '/d/apr_slack_old');
      await expect(page.getByText(/The note was removed/)).toBeVisible();
      await scan(page, 'owner Reviews block, note removed');
      await page.setViewportSize({ width: 390, height: 844 });
      await go(page, '/d/apr_slack');
      await expect(page.getByRole('region', { name: 'Reviews' })).toBeVisible();
      await scan(page, 'phone owner Reviews block');
      await signIn(page, theme);
      await go(page, '/reviews');
      await expect(page.getByText('Reviews are for people who hold the Reviewer role', { exact: false })).toBeVisible();
      await scan(page, 'reviews refused for a PM');
    });
  });
}
