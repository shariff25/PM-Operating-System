import { expect, test } from '@playwright/test';
import type { Page } from '@playwright/test';

async function signIn(page: Page, base = '') {
  await page.goto(base + '/');
  await page.getByLabel('Your handle').fill('ana');
  await page.getByRole('button', { name: 'Sign in', exact: true }).click();
  await expect(page.getByLabel('What do you need?')).toBeVisible();
}

const go = (page: Page, hash: string) => page.evaluate((h) => { window.location.hash = h; }, hash);

type Box = { x: number; y: number; width: number; height: number };
const overlaps = (a: Box, b: Box) => a.x < b.x + b.width && b.x < a.x + a.width && a.y < b.y + b.height && b.y < a.y + a.height;
const box = async (loc: ReturnType<Page['locator']>) => (await loc.boundingBox()) as Box;

test('desktop: no hint covers the tabs, the first source or the approve button', async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 });
  await signIn(page);
  await go(page, '/d/apr_prd');
  const hint = page.locator('[data-hint]');
  const tabs = page.getByRole('tablist', { name: 'Draft panel' });
  const firstSource = page.getByRole('button', { name: /TCB grace period/ }).first();
  const approve = page.getByRole('button', { name: 'Approve to Confluence' });
  for (const name of ['sources', 'ask', 'approve']) {
    await expect(hint).toHaveAttribute('data-hint', name);
    await page.waitForTimeout(300);
    const h = await box(hint);
    for (const [what, target] of [['tabs', tabs], ['first source', firstSource], ['approve', approve]] as const) {
      expect(overlaps(h, await box(target)), `the ${name} hint overlaps the ${what}`).toBe(false);
    }
    await hint.getByRole('button', { name: 'Got it' }).click();
  }
  await expect(hint).toHaveCount(0);
});

test('phone: the hint is a one-line banner under the header and covers nothing', async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await signIn(page);
  await go(page, '/d/apr_prd');
  const hint = page.locator('[data-hint]');
  await expect(hint).toHaveClass(/coach-banner/);
  await page.waitForTimeout(300); // after the entrance animation
  const h = await box(hint);
  expect(h.height).toBeLessThan(60);
  const header = await box(page.locator('header.topbar'));
  expect(h.y).toBeGreaterThanOrEqual(header.y + header.height - 1);
  for (const t of [page.getByRole('tablist'), page.getByRole('button', { name: /^Approve to/ }), page.getByRole('button', { name: 'More' })]) {
    expect(overlaps(h, await box(t))).toBe(false);
  }
  for (let i = 0; i < 3; i++) await hint.getByRole('button', { name: 'Got it' }).click();
  await expect(hint).toHaveCount(0);
});

test('phone: the draft header is the primary button and one More menu', async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await signIn(page);
  await go(page, '/d/apr_jira');
  await expect(page.getByRole('button', { name: 'Approve to Jira' })).toBeVisible();
  await expect(page.getByRole('button', { name: 'Edit', exact: true })).toHaveCount(0);
  await expect(page.getByRole('button', { name: 'Reject draft' })).toHaveCount(0);
  await page.getByRole('button', { name: 'More' }).click();
  await expect(page.getByRole('menuitem')).toHaveText(['Edit', 'Reject draft']);
  await page.getByRole('menuitem', { name: 'Reject draft' }).click();
  await expect(page.getByRole('group', { name: 'Why are you rejecting it?' })).toBeVisible();
  const [sw, cw] = await page.evaluate(() => [document.documentElement.scrollWidth, document.documentElement.clientWidth]);
  expect(sw).toBeLessThanOrEqual(cw);
});

test('a suggestion shows changed words inline and folds unchanged paragraphs', async ({ page }) => {
  await signIn(page);
  await go(page, '/d/apr_prd');
  await page.getByRole('tab', { name: 'Ask for a change' }).click();
  await page.getByLabel('What should change?').fill('shorter');
  await page.getByRole('button', { name: 'Suggest a change' }).click();
  const region = page.getByRole('region', { name: 'Suggested change' });
  await expect(region.locator('ins.ba-word').first()).toContainText('only');
  await expect(region.locator('del.ba-word').first()).toBeVisible();
  const fold = region.getByRole('button', { name: /unchanged paragraphs?/ });
  await expect(fold).toHaveAttribute('aria-expanded', 'false');
  await fold.click();
  await expect(fold).toHaveAttribute('aria-expanded', 'true');
});

test('the drafts table says where each draft goes', async ({ page }) => {
  await signIn(page);
  await page.getByRole('navigation', { name: 'Main' }).getByRole('link', { name: /^Drafts/ }).click();
  await page.getByRole('tab', { name: /^All/ }).click();
  const rows = page.getByRole('tabpanel').getByRole('link');
  await expect(rows.filter({ hasText: 'Reply to Ben' })).toContainText('Slack reply · #partner-pricing');
  await expect(rows.filter({ hasText: 'PRD: TCB grace period' })).toContainText('Confluence page');
});

test('a running request shows its progress and the retry line, and the status does not jump', async ({ page }) => {
  await signIn(page);
  await page.getByLabel('What do you need?').fill('this is busy, what changed?');
  await page.getByLabel('What do you need?').press('Enter');
  await expect(page.locator('.live-line')).toBeVisible();
  await expect(page.getByRole('progressbar', { name: 'Progress' })).toBeVisible();
  await expect(page.getByText(/catching its breath/)).toBeVisible();
  // the steps toggle stays where it is while the status text, the count and the retry line change
  const toggle = page.getByRole('button', { name: /Show steps/ });
  const y = (await box(toggle)).y;
  // Position and "still working" are read in one step: once the request settles the answer
  // arrives above the toggle and may move it, and that must not be measured as a jump.
  const sample = () =>
    toggle.evaluate((el) => ({
      y: el.getBoundingClientRect().y,
      working: document.querySelector('.live-line')?.classList.contains('live-line-on') ?? false,
    }));
  for (let i = 0; i < 8; i++) {
    const s = await sample();
    if (!s.working) break;
    expect(Math.abs(s.y - y)).toBeLessThanOrEqual(1);
    await page.waitForTimeout(80);
  }
});

test('a brand-new account sees an icon, one line and one action on every empty page', async ({ page }) => {
  await signIn(page, 'http://127.0.0.1:5192');
  for (const [hash, action] of [['/drafts', 'Ask for a draft'], ['/followups', 'Ask what is open'], ['/activity', 'Ask your first question']] as const) {
    await go(page, hash);
    const empty = page.locator('.empty');
    await expect(empty).toHaveCount(1);
    await expect(empty.locator('svg')).toHaveCount(1);
    await expect(empty.getByRole('button')).toHaveCount(1);
    await empty.getByRole('button', { name: action }).click();
    await expect(page.getByRole('dialog', { name: 'Ask PM OS or jump to a page' })).toBeVisible();
    await page.keyboard.press('Escape');
  }
});
