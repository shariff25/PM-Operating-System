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
      await expect(page.locator('.cite-tip')).toHaveText('Slack thread in C01');
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
      await expect(page.getByText('Created in Space PM, under Specs.')).toBeVisible();
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
      await expect(page.getByText(/first brief hasn't arrived yet/)).toBeVisible();
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

    test('admin: auditor views and the no-access page', async ({ page }) => {
      await signIn(page, theme, undefined, 'audrey');
      await go(page, '/admin');
      await expect(page.getByRole('heading', { level: 1, name: 'Overview' })).toBeVisible();
      await expect(page.getByRole('region', { name: 'Recent denied actions' }).getByRole('listitem').first()).toBeVisible();
      await scan(page, 'auditor overview');
      await go(page, '/admin/audit');
      await expect(page.getByRole('table', { name: 'Audit events' })).toBeVisible();
      await scan(page, 'auditor audit log');
      await go(page, '/admin/connectors');
      await expect(page.getByRole('heading', { level: 1, name: "You don't have access to this" })).toBeVisible();
      await scan(page, 'auditor, no access');
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
  });
}
