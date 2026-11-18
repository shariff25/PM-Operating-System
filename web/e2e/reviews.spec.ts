import { expect, test } from '@playwright/test';
import type { Page } from '@playwright/test';

// Reviews against the sample data: Ben reviews Ana and Dana. Each test starts from a fresh page,
// so the sample data starts over each time. The server runs with hostile sample text on.

async function signIn(page: Page, handle: string) {
  await page.goto('/');
  await page.getByLabel('Your handle').fill(handle);
  await page.getByRole('button', { name: 'Sign in', exact: true }).click();
  await expect(page.getByLabel('What do you need?')).toBeVisible();
}

const go = (page: Page, hash: string) => page.evaluate((h) => { window.location.hash = h; }, hash);
const panel = (page: Page) => page.getByRole('region', { name: 'Your advice' });
const inboxRow = (page: Page, id: string) => page.getByRole('list', { name: 'Reviews' }).locator(`a[href="#/reviews/${id}"]`);

test('a reviewer recommends approving from the inbox', async ({ page }) => {
  await signIn(page, 'ben');
  const nav = page.getByRole('navigation', { name: 'Main' });
  await nav.getByRole('link', { name: /Reviews/ }).click();
  await expect(page.getByRole('heading', { level: 1, name: 'Reviews' })).toBeVisible();
  await expect(inboxRow(page, 'apr_dana_onboarding')).toContainText('Needs your advice');
  await inboxRow(page, 'apr_dana_onboarding').click();
  await expect(page.getByRole('heading', { level: 1, name: 'Onboarding checklist for new workspaces' })).toBeVisible();
  // Read only: no way to approve, edit or send.
  await expect(page.getByRole('button', { name: /^Approve|^Edit$|^Reject|Suggest a change/ })).toHaveCount(0);
  await panel(page).getByRole('button', { name: 'Recommend approving' }).click();
  await expect(panel(page).getByText('You recommend approving', { exact: true })).toBeVisible();
  await expect(panel(page).getByText('Recorded just now')).toBeVisible();
  await go(page, '/reviews');
  await expect(inboxRow(page, 'apr_dana_onboarding')).toContainText('You recommend approving');
});

test('a reviewer suggests changes with a note, by keyboard', async ({ page }) => {
  await signIn(page, 'ben');
  await go(page, '/reviews/apr_jira');
  const note = panel(page).getByLabel('Note for Ana Silva');
  await note.focus();
  await page.keyboard.type('The end date needs a time zone.\nThe rest reads well.');
  await expect(panel(page).getByText('52 / 1000')).toBeVisible();
  await page.keyboard.press('Tab');
  await page.keyboard.press('Tab');
  await expect(panel(page).getByRole('button', { name: 'Suggest changes' })).toBeFocused();
  await page.keyboard.press('Enter');
  await expect(panel(page).getByText('You suggested changes', { exact: true })).toBeVisible();
  await expect(panel(page).locator('.rec-note')).toHaveText('The end date needs a time zone.\nThe rest reads well.');
  await expect(note).toHaveValue('');
});

test('the draft changes under the reviewer: the new version shows and the note stays', async ({ page }) => {
  await signIn(page, 'ben');
  await go(page, '/reviews/apr_dana_exports');
  await expect(page.getByText(/at most three times/)).toBeVisible();
  await panel(page).getByLabel('Note for dana').fill('Say who gets the email.');
  await panel(page).getByRole('button', { name: 'Suggest changes' }).click();
  await expect(panel(page).getByRole('alert')).toContainText('dana changed this draft after you opened it, so your recommendation was not saved. You are now looking at the new version');
  await expect(page.getByText(/at most twice, then stop/)).toBeVisible();
  await expect(page.getByText('Edited by its owner').first()).toBeVisible();
  await expect(panel(page).getByLabel('Note for dana')).toHaveValue('Say who gets the email.');
  await panel(page).getByRole('button', { name: 'Suggest changes' }).click();
  await expect(panel(page).getByText('You suggested changes', { exact: true })).toBeVisible();
});

test('a restricted draft shows metadata only, with no recommend panel', async ({ page }) => {
  await signIn(page, 'ben');
  await go(page, '/reviews');
  await expect(inboxRow(page, 'apr_prd')).toContainText('Restricted draft');
  await expect(inboxRow(page, 'apr_prd')).not.toContainText('PRD: TCB grace period setting');
  await inboxRow(page, 'apr_prd').click();
  await expect(page.getByText('This draft uses restricted sources; its owner can share it with you.')).toBeVisible();
  await expect(page.getByText('Ana Silva')).toBeVisible();
  await expect(panel(page)).toHaveCount(0);
  await expect(page.getByRole('button', { name: 'Recommend approving' })).toHaveCount(0);
  await expect(page.getByText('PRD: TCB grace period setting')).toHaveCount(0);
});

test('someone who is not a reviewer has no Reviews entry, and the route refuses calmly', async ({ page }) => {
  await signIn(page, 'ana');
  await expect(page.getByRole('navigation', { name: 'Main' }).getByRole('link', { name: /Drafts/ })).toBeVisible();
  await expect(page.getByRole('navigation', { name: 'Main' }).getByRole('link', { name: /Reviews/ })).toHaveCount(0);
  await go(page, '/reviews');
  await expect(page.getByText('Reviews are for people who hold the Reviewer role. If you should have it, ask an admin.')).toBeVisible();
  await expect(page.getByRole('alert')).toHaveCount(0);
  await go(page, '/reviews/apr_dana_onboarding');
  await expect(page.getByText('Reviews are for people who hold the Reviewer role. If you should have it, ask an admin.')).toBeVisible();
  await expect(page.getByText('Onboarding checklist for new workspaces')).toHaveCount(0);
});

test('the owner sees the advice, marked stale after an edit, in the workspace and the approve dialog', async ({ page }) => {
  await signIn(page, 'ana');
  await go(page, '/d/apr_jira');
  const block = page.getByRole('region', { name: 'Reviews' });
  await expect(block).toContainText('Ben Carter suggests changes');
  await expect(block).toContainText("Say which time zone the end date uses: the admin's or the workspace's.");
  await page.getByRole('button', { name: 'Approve to Jira' }).click();
  await expect(page.getByRole('dialog').getByText('Ben Carter suggests changes', { exact: true })).toBeVisible();
  await page.keyboard.press('Escape');
  await page.getByRole('button', { name: 'Edit', exact: true }).click();
  await page.getByLabel('Draft text').fill('Show the banner in the admin’s time zone.');
  await page.getByRole('button', { name: 'Save', exact: true }).click();
  await expect(block).toContainText('Ben Carter suggested changes on an earlier version');
  await expect(block.getByText('Earlier version', { exact: true })).toBeVisible();
  await page.getByRole('button', { name: 'Approve to Jira' }).click();
  const dialog = page.getByRole('dialog');
  await expect(dialog.getByText('Ben Carter suggested changes on an earlier version')).toBeVisible();
  // Advice never blocks the owner's own decision.
  await dialog.getByRole('button', { name: 'Confirm and create' }).click();
  await expect(page.getByText(/^Created in Project PAY/)).toBeVisible();
});

test('a hostile name and note render as plain text, for the reviewer and the owner', async ({ page }) => {
  const hostileName = '<img src=x onerror="window.__pwned=1"> <!channel>';
  await signIn(page, 'ben');
  await go(page, '/reviews');
  await expect(inboxRow(page, 'apr_zed')).toContainText(hostileName);
  await inboxRow(page, 'apr_zed').click();
  await expect(panel(page)).toContainText(`Advice only. ${hostileName} decides`);
  await expect(page.locator('main img')).toHaveCount(0);

  await signIn(page, 'ana');
  await go(page, '/d/apr_hostile');
  const block = page.getByRole('region', { name: 'Reviews' });
  await expect(block).toContainText(`${hostileName} suggests changes`);
  await expect(block.locator('.rec-note')).toHaveText('<img src=x onerror="window.__pwned=1"> <!channel> **not bold** [not a link](https://evil.example/login)');
  await expect(block.locator('img, a, strong')).toHaveCount(0);
  expect(await page.evaluate(() => (window as unknown as { __pwned?: number }).__pwned)).toBeUndefined();
});
