import AxeBuilder from '@axe-core/playwright';
import { expect, test } from '@playwright/test';
import type { Page } from '@playwright/test';

// The Admin's view of the bots (slice A1) in the browser: the catalog, the drafts on hold and the
// release dialog by keyboard, the audit log's bot filter, and a held draft as its owner sees it.
// axe runs with every rule it ships on each screen and state, in both themes, at desktop and
// phone width.

async function signIn(page: Page, handle: string, search = '', theme: 'light' | 'dark' = 'light') {
  await page.addInitScript((t) => {
    try {
      window.localStorage.setItem('pmos-theme', t);
    } catch {
      /* the page still loads */
    }
  }, theme);
  await page.goto('/' + search);
  await page.getByLabel('Your handle').fill(handle);
  await page.getByRole('button', { name: 'Sign in', exact: true }).click();
  await expect(page.locator('html')).toHaveAttribute('data-theme', theme);
}

const go = (page: Page, hash: string) => page.evaluate((h) => { window.location.hash = h; }, hash);

async function scan(page: Page, what: string) {
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

/** No part of the page is wider than the window. */
async function noSideScroll(page: Page) {
  const over = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
  expect(over).toBeLessThanOrEqual(0);
}

test('an admin finds Bots in the Admin area and releases a held draft by keyboard', async ({ page }) => {
  await signIn(page, 'ada');
  const rail = page.getByRole('navigation', { name: 'Main' });
  await rail.getByRole('link', { name: 'Admin' }).click();
  await rail.getByRole('list', { name: 'Admin sections' }).getByRole('link', { name: 'Bots' }).click();
  await expect(page.getByRole('heading', { level: 1, name: 'Bots' })).toBeVisible();
  const catalog = page.getByRole('table', { name: 'Bots in the catalog' });
  await expect(catalog.getByRole('row', { name: /Scout/ })).toContainText('Withdrawn');
  await expect(catalog.getByRole('row', { name: /Chief/ })).toContainText('Preview');

  const held = page.getByRole('table', { name: 'Drafts on hold' });
  const release = held.getByRole('button', { name: 'Release draft apr_held, owned by ana' });
  await release.focus();
  await page.keyboard.press('Enter');
  const dialog = page.getByRole('dialog', { name: 'Release this draft?' });
  await expect(dialog).toBeVisible();
  await expect(dialog.getByText('What releasing means')).toBeVisible();
  await page.keyboard.press('Escape');
  await expect(dialog).toBeHidden();
  await expect(release).toBeFocused();
  await page.keyboard.press('Enter');
  await expect(dialog).toBeVisible();
  await dialog.getByRole('button', { name: 'Release draft' }).click();
  await expect(dialog).toBeHidden();
  await expect(page.getByText('Released apr_held. Its owner can decide it now.')).toBeVisible();
  await expect(held.getByRole('button', { name: 'Release draft apr_held, owned by ana' })).toHaveCount(0);
});

test('the audit log filters by bot from the address', async ({ page }) => {
  await signIn(page, 'audrey');
  await go(page, '/admin/audit');
  const bot = page.getByLabel('Bot', { exact: true });
  await expect(bot.getByRole('option', { name: 'Chief' })).toHaveCount(1);
  await bot.selectOption('chief');
  await expect(page).toHaveURL(/#\/admin\/audit\?persona=chief$/);
  const rows = page.getByRole('table', { name: 'Audit events' }).getByRole('row');
  await expect(rows.nth(1)).toBeVisible();
  await page.getByRole('button', { name: 'Clear filters' }).click();
  await expect(page).toHaveURL(/#\/admin\/audit$/);
});

test('a held draft says so, and only rejecting is offered', async ({ page }) => {
  await signIn(page, 'ana', '?mock_held=1');
  await go(page, '/d/apr_held');
  await expect(page.getByText('On hold: the bot that drafted this isn’t offered any more')).toBeVisible();
  await expect(page.getByRole('button', { name: /^Approve/ })).toBeDisabled();
  await expect(page.getByRole('button', { name: 'Edit' })).toBeDisabled();
  await expect(page.getByRole('button', { name: 'Reject draft' })).toBeEnabled();
});

for (const theme of ['light', 'dark'] as const) {
  for (const width of [1280, 390]) {
    test(`axe on the Bots page, the release dialog and a held draft, ${theme}, ${width}px`, async ({ page }) => {
      test.slow();
      await page.setViewportSize({ width, height: width === 390 ? 844 : 900 });
      await signIn(page, 'ada', '?mock_held=1', theme);
      await go(page, '/admin/bots');
      await expect(page.getByRole('table', { name: 'Drafts on hold' })).toBeVisible();
      await noSideScroll(page);
      await scan(page, `the Bots page (${theme}, ${width})`);
      await page.getByRole('button', { name: 'Release draft apr_held, owned by ana' }).click();
      await expect(page.getByRole('dialog', { name: 'Release this draft?' })).toBeVisible();
      await scan(page, `the release dialog (${theme}, ${width})`);
      await page.keyboard.press('Escape');
      await go(page, '/admin/audit?persona=chief');
      await expect(page.getByRole('table', { name: 'Audit events' })).toBeVisible();
      await noSideScroll(page);
      await scan(page, `the audit log by bot (${theme}, ${width})`);
      await go(page, '/d/apr_held');
      await expect(page.getByText('On hold: the bot that drafted this isn’t offered any more')).toBeVisible();
      await noSideScroll(page);
      await scan(page, `a held draft (${theme}, ${width})`);
      await go(page, '/drafts');
      await expect(page.getByText('On hold').first()).toBeVisible();
      await scan(page, `the drafts list with a held draft (${theme}, ${width})`);
    });
  }
}
