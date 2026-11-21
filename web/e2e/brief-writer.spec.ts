import AxeBuilder from '@axe-core/playwright';
import { expect, test } from '@playwright/test';
import type { Page } from '@playwright/test';

// Slice R2 in the browser: the switch "Chief writes my daily brief" in Chief's Settings on the
// Bots page, by keyboard, and Today's brief line saying who wrote the brief. axe runs with every
// rule it ships on each state, in both themes, at desktop and phone width, with no sideways scroll.

async function signIn(page: Page, search = '', theme: 'light' | 'dark' = 'light') {
  await page.addInitScript((t) => {
    try {
      window.localStorage.setItem('pmos-theme', t);
    } catch {
      /* the page still loads */
    }
  }, theme);
  await page.goto('/' + search);
  await page.getByLabel('Your handle').fill('ana');
  await page.getByRole('button', { name: 'Sign in', exact: true }).click();
  await expect(page.locator('html')).toHaveAttribute('data-theme', theme);
}

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

async function noSideScroll(page: Page) {
  const over = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
  expect(over).toBeLessThanOrEqual(0);
}

const go = (page: Page, hash: string) => page.evaluate((h) => { window.location.hash = h; }, hash);

async function openChiefSettings(page: Page) {
  await go(page, '/bots');
  const team = page.getByRole('heading', { level: 2, name: /^On your team/ }).locator('xpath=ancestor::section[1]');
  const settings = team.getByRole('button', { name: 'Settings for Chief' });
  await settings.focus();
  await page.keyboard.press('Enter');
  const dialog = page.getByRole('dialog', { name: 'Chief settings' });
  await expect(dialog).toBeVisible();
  return { dialog, settings };
}

test('a PM hands the daily brief to Chief and back by keyboard, and is told what changes', async ({ page }) => {
  await signIn(page);
  const { dialog, settings } = await openChiefSettings(page);
  const sw = dialog.getByRole('switch', { name: 'Chief writes my daily brief' });
  await expect(sw).toHaveAttribute('aria-checked', 'false');
  await expect(dialog.getByText(/^Nudge writes it now\. Same brief, same time, same Slack DM\./)).toBeVisible();
  // Tab reaches the switch; Space turns it on.
  await sw.focus();
  await page.keyboard.press('Space');
  await expect(sw).toHaveAttribute('aria-checked', 'true');
  await expect(dialog.getByRole('status')).toHaveText('Chief writes your daily brief from the next one.');
  await expect(sw).toBeFocused();
  await expect(dialog.getByText('Your daily brief', { exact: true })).toBeVisible();
  // Enter turns it off again.
  await page.keyboard.press('Enter');
  await expect(sw).toHaveAttribute('aria-checked', 'false');
  await expect(dialog.getByRole('status')).toHaveText('Nudge writes your daily brief again, from the next one.');
  await page.keyboard.press('Escape');
  await expect(dialog).toBeHidden();
  await expect(settings).toBeFocused();
});

test("Today's brief line says Chief wrote it", async ({ page }) => {
  await signIn(page, '?mock_brief_writer=chief');
  const line = page.locator('.today-brief').first();
  await expect(line).toContainText('Your daily brief is in');
  await expect(line).toContainText('Written by Chief');
  await expect(line.locator('svg[data-mascot="chief"]')).toHaveCount(1);
});

for (const width of [1280, 390]) {
  for (const theme of ['light', 'dark'] as const) {
    test(`the brief switch and the attributed brief pass axe at ${width}px, ${theme}`, async ({ page }) => {
      await page.setViewportSize({ width, height: 900 });
      await signIn(page, '?mock_brief_writer=chief', theme);
      await expect(page.locator('.today-brief').first()).toContainText('Written by Chief');
      await noSideScroll(page);
      await scan(page, `Today with Chief's brief, ${width}px ${theme}`);
      const { dialog } = await openChiefSettings(page);
      await expect(dialog.getByRole('switch', { name: 'Chief writes my daily brief' })).toHaveAttribute('aria-checked', 'true');
      await noSideScroll(page);
      await scan(page, `Chief's Settings with the brief switch on, ${width}px ${theme}`);
      // And with it off, after a change.
      await dialog.getByRole('switch', { name: 'Chief writes my daily brief' }).click();
      await expect(dialog.getByRole('status')).toHaveText('Nudge writes your daily brief again, from the next one.');
      await scan(page, `Chief's Settings after the switch, ${width}px ${theme}`);
    });
  }
}
