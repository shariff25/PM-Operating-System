import { expect, test } from '@playwright/test';
import type { Page } from '@playwright/test';

// Nudge and the guided start. The sample-data client keeps each person's settings in
// sessionStorage, as the server keeps them, so a reload keeps them too. These walk the three-step
// start, where nothing is offered besides Nudge (`mock_bots=nudge_only`); the four-step start with
// "Pick your team" is in market.spec.ts.

const EMPTY = 'http://127.0.0.1:5192';
const WELCOME = "Hi Ana, I'm Nudge.";

async function signIn(page: Page, search = '', base = '') {
  await page.goto(base + '/' + search);
  await page.getByLabel('Your handle').fill('ana');
  await page.getByRole('button', { name: 'Sign in', exact: true }).click();
  await expect(page.getByLabel('What do you need?')).toBeVisible();
}
const card = (page: Page) => page.getByRole('region', { name: WELCOME });
const today = (page: Page) => page.getByRole('navigation', { name: 'Main' }).getByRole('link', { name: /^Today/ });
const nudges = (page: Page) => page.locator('svg[data-nudge]');

test('a new PM is guided through the three steps, and sees "Approved!" once', async ({ page }) => {
  await signIn(page, '?mock_new=1&mock_bots=nudge_only');
  await expect(card(page)).toBeVisible();
  await expect(card(page).locator('svg[data-nudge="hello"]')).toBeVisible();
  await expect(card(page).getByText('Getting started: 0 of 3 done')).toBeAttached();

  // 1. ask, from the card's own pointer to the box
  await card(page).getByRole('button', { name: 'Take me to the box' }).click();
  await expect(page.getByLabel('What do you need?')).toBeFocused();
  await page.keyboard.type('what changed in billing');
  await page.keyboard.press('Enter');
  await expect(page).toHaveURL(/#\/a\//);
  await today(page).click();
  await expect(card(page).getByText('Getting started: 1 of 3 done')).toBeAttached();

  // progress is kept across a reload
  await page.reload();
  await page.getByLabel('Your handle').fill('ana');
  await page.getByRole('button', { name: 'Sign in', exact: true }).click();
  await expect(card(page).getByText('Getting started: 1 of 3 done')).toBeAttached();

  // 2. open the practice draft, and see what the bot read
  await card(page).getByRole('link', { name: /Open the practice draft/ }).click();
  await expect(page.getByRole('region', { name: 'What the bot read' })).toBeVisible();
  await today(page).click();
  await expect(card(page).getByText('Getting started: 2 of 3 done')).toBeAttached();

  // 3. approve it: nothing is sent, and the one "Approved!" moment
  await card(page).getByRole('link', { name: /Back to the practice draft/ }).click();
  await page.getByRole('button', { name: 'Approve to Confluence' }).click();
  await page.getByRole('dialog').getByRole('button', { name: /^Confirm/ }).click();
  const outcome = page.getByRole('status', { name: 'Outcome' });
  await expect(outcome.getByText('Approved!', { exact: true })).toBeVisible();
  await expect(outcome.locator('svg[data-nudge="yay"]')).toBeVisible();
  await expect(outcome).toContainText('Nothing was created, because this was practice.');

  // the guided start is over, and the moment does not come back
  await today(page).click();
  await expect(page.getByLabel('What do you need?')).toBeVisible();
  await expect(card(page)).toHaveCount(0);
  await page.evaluate(() => { window.location.hash = '/d/apr_jira'; });
  await page.getByRole('button', { name: 'Approve to Jira' }).click();
  await page.getByRole('dialog').getByRole('button', { name: /^Confirm/ }).click();
  await expect(page.getByRole('status', { name: 'Outcome' })).toBeVisible();
  await expect(page.getByText('Approved!', { exact: true })).toHaveCount(0);
});

test('"Skip the tour" ends it, it stays ended after a reload, and "Show hints again" restarts it', async ({ page }) => {
  await signIn(page, '?mock_new=1&mock_bots=nudge_only');
  await card(page).getByRole('button', { name: 'Skip the tour' }).click();
  await expect(card(page)).toHaveCount(0);
  await page.reload();
  await page.getByLabel('Your handle').fill('ana');
  await page.getByRole('button', { name: 'Sign in', exact: true }).click();
  await expect(page.getByLabel('What do you need?')).toBeVisible();
  await expect(card(page)).toHaveCount(0);
  await page.getByRole('button', { name: /Menu for/ }).click();
  await page.getByRole('menuitem', { name: 'Show hints again' }).click();
  await expect(card(page)).toBeVisible();
  await expect(card(page).getByText('Getting started: 0 of 3 done')).toBeAttached();
});

test('hiding Nudge removes the character on every page and keeps every text; it comes back when turned on', async ({ page }) => {
  await signIn(page, '?mock_new=1&mock_bots=nudge_only');
  await expect(nudges(page).first()).toBeVisible();
  await page.getByRole('button', { name: /Menu for/ }).click();
  await page.getByRole('menuitemcheckbox', { name: /Show Nudge/ }).click();
  await expect(page.getByRole('menuitemcheckbox', { name: /Show Nudge/ })).toHaveAttribute('aria-checked', 'false');
  await page.keyboard.press('Escape');

  // Today: the welcome card is all there, without the picture
  await expect(card(page)).toBeVisible();
  await expect(card(page).getByText('Ask me for something')).toBeVisible();
  await expect(nudges(page)).toHaveCount(0);
  // a draft: the hint keeps its words
  await page.getByRole('link', { name: /Practice: why PMs should take the stairs/ }).click();
  await expect(page.locator('[data-hint="sources"]')).toContainText('Everything I claimed');
  await expect(nudges(page)).toHaveCount(0);
  // every other page
  for (const hash of ['/drafts', '/followups', '/activity']) {
    await page.evaluate((h) => { window.location.hash = h; }, hash);
    await expect(page.getByRole('heading', { level: 1 })).toBeVisible();
    await expect(nudges(page)).toHaveCount(0);
  }
  // kept after a reload
  await page.reload();
  await page.getByLabel('Your handle').fill('ana');
  await page.getByRole('button', { name: 'Sign in', exact: true }).click();
  await today(page).click();
  await expect(card(page)).toBeVisible();
  await expect(nudges(page)).toHaveCount(0);

  // and back on
  await page.getByRole('button', { name: /Menu for/ }).click();
  await page.getByRole('menuitemcheckbox', { name: /Show Nudge/ }).click();
  await page.keyboard.press('Escape');
  await expect(card(page).locator('svg[data-nudge="hello"]')).toBeVisible();
});

test('a calm empty account gets the calm Nudge, and its line', async ({ page }) => {
  await signIn(page, '', EMPTY);
  const calm = page.locator('.empty', { has: page.locator('svg[data-nudge="calm"]') });
  await expect(calm).toContainText('Suspiciously calm.');
  await page.evaluate(() => { window.location.hash = '/drafts'; });
  await expect(page.locator('.empty svg[data-nudge="calm"]')).toBeVisible();
});

test('a returning PM sees no guided start', async ({ page }) => {
  await signIn(page);
  await expect(page.getByRole('link', { name: /PRD: TCB grace period setting/ }).first()).toBeVisible();
  await expect(card(page)).toHaveCount(0);
  await expect(page.getByText('Getting started', { exact: false })).toHaveCount(0);
});
