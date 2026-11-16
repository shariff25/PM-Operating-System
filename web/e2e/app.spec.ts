import { expect, test } from '@playwright/test';
import type { Page } from '@playwright/test';

// The sample-data client keeps its state in the page, so every test starts from a fresh page.

async function signIn(page: Page, hash = '') {
  await page.goto('/' + hash);
  await page.getByLabel('Your handle').fill('ana');
  await page.getByRole('button', { name: 'Sign in', exact: true }).click();
  await expect(page.getByLabel('What do you need?')).toBeVisible();
}

async function openDraft(page: Page, id: string) {
  await page.evaluate((h) => { window.location.hash = h; }, `/d/${id}`);
  await expect(page.getByRole('article', { name: 'Formatted draft' })).toBeVisible();
}

test('signs in, asks, and sees a result', async ({ page }) => {
  await signIn(page);
  await page.getByLabel('What do you need?').fill('what changed in billing');
  await page.getByRole('button', { name: 'Ask', exact: true }).click();
  const panel = page.getByRole('region', { name: 'Your request' });
  await expect(panel.getByText('Done', { exact: true })).toBeVisible();
  await expect(panel.locator('strong', { hasText: 'PAY-212' })).toBeVisible();
});

test('asks for a draft and opens it as a formatted document', async ({ page }) => {
  await signIn(page);
  await page.getByLabel('What do you need?').fill('draft a PRD for retries');
  await page.getByRole('button', { name: 'Ask', exact: true }).click();
  await page.getByRole('button', { name: 'Open the draft' }).click();
  const doc = page.getByRole('article', { name: 'Formatted draft' });
  await expect(doc.getByRole('heading', { level: 1 })).toContainText('PRD: retries');
  await expect(doc.getByRole('heading', { level: 2, name: 'Problem' })).toBeVisible();
  await expect(page.getByRole('button', { name: 'Edit' })).toBeVisible();
  await expect(page.locator('textarea')).toHaveCount(0);
});

test('edits, saves, and returns to the formatted view', async ({ page }) => {
  await signIn(page);
  await openDraft(page, 'apr_prd');
  await expect(page.getByRole('status').filter({ hasText: 'Saved' })).toBeVisible();
  await page.getByRole('button', { name: 'Edit' }).click();
  await page.getByLabel('Draft text').fill('## Problem\n\nA brand new problem statement.');
  await expect(page.getByText('Unsaved changes')).toBeVisible();
  await expect(page.getByRole('button', { name: 'Approve to Confluence' })).toBeDisabled();
  await page.getByRole('button', { name: 'Save', exact: true }).click();
  const doc = page.getByRole('article', { name: 'Formatted draft' });
  await expect(doc).toContainText('A brand new problem statement.');
  await expect(page.locator('textarea')).toHaveCount(0);
  await expect(page.getByText('Saved', { exact: true })).toBeVisible();
});

test('asks for a change and accepts it', async ({ page }) => {
  await signIn(page);
  await openDraft(page, 'apr_prd');
  await page.getByLabel('What should change?').fill('shorter');
  await page.getByRole('button', { name: 'Suggest a change' }).click();
  const region = page.getByRole('region', { name: 'Suggested change' });
  await expect(region).toBeVisible();
  await expect(page.getByRole('button', { name: 'Approve to Confluence' })).toBeDisabled();
  await region.getByRole('button', { name: 'Accept' }).click();
  await expect(region).toHaveCount(0);
  await expect(page.getByRole('article', { name: 'Formatted draft' })).not.toContainText('Who gets the banner');
  await expect(page.getByRole('button', { name: 'Approve to Confluence' })).toBeEnabled();
});

test('approve shows the title, place, name and link destinations before anything is sent', async ({ page }) => {
  await signIn(page);
  await openDraft(page, 'apr_prd');
  await page.getByRole('button', { name: 'Approve to Confluence' }).click();
  const dialog = page.getByRole('dialog', { name: 'Check before you approve' });
  await expect(dialog).toContainText('PRD: TCB grace period setting');
  await expect(dialog).toContainText('Space PM, under Specs');
  await expect(dialog).toContainText('Ana Silva');
  await expect(dialog.getByRole('list', { name: 'Links in this draft' })).toContainText('https://example.com/wiki/billing-roadmap');
  await dialog.getByRole('button', { name: 'Confirm and create' }).click();
  await expect(page.getByText('Created in Space PM, under Specs.')).toBeVisible();
  await expect(page.getByRole('link', { name: 'Open it' })).toBeVisible();
});

test('an external channel needs a second, explicit yes', async ({ page }) => {
  await signIn(page);
  await openDraft(page, 'apr_slack');
  await page.getByRole('button', { name: 'Approve to Slack' }).click();
  const dialog = page.getByRole('dialog');
  const send = dialog.getByRole('button', { name: 'Confirm and post outside the company' });
  await expect(dialog).toContainText('shared outside the company');
  await expect(send).toBeDisabled();
  await dialog.getByRole('checkbox').check();
  await expect(send).toBeEnabled();
  await send.click();
  await expect(page.getByText(/Created in #partner-pricing/)).toBeVisible();
});

test('an expired draft is labelled, sorted lower, and cannot be approved', async ({ page }) => {
  await signIn(page);
  const rows = page.getByRole('region', { name: 'Drafts' }).getByRole('link');
  const expired = rows.filter({ hasText: 'Reply to Dana in #migrations' });
  await expect(expired).toContainText('Expired');
  const texts = await rows.allInnerTexts();
  const iExpired = texts.findIndex((t) => t.includes('Reply to Dana'));
  const lastNeedsYou = texts.map((t, i) => (t.includes('Needs you') ? i : -1)).reduce((a, b) => Math.max(a, b));
  expect(iExpired).toBeGreaterThan(lastNeedsYou);
  await expired.click();
  await expect(page.getByText(/This draft expired, so nothing was sent\. Ask me for a fresh draft\./)).toBeVisible();
  await expect(page.getByRole('button', { name: /^Approve/ })).toHaveCount(0);
});

test('hints come one at a time and do not return after they are dismissed', async ({ page }) => {
  await signIn(page);
  await openDraft(page, 'apr_prd');
  const hints = page.locator('[data-hint]');
  await expect(hints).toHaveCount(1);
  await expect(hints).toHaveAttribute('data-hint', 'sources');
  await hints.getByRole('button', { name: 'Got it' }).click();
  await expect(hints).toHaveAttribute('data-hint', 'ask');
  await expect(hints).toHaveCount(1);
  await hints.getByRole('button', { name: 'Got it' }).click();
  await expect(hints).toHaveAttribute('data-hint', 'approve');
  await hints.getByRole('button', { name: 'Got it' }).click();
  await expect(hints).toHaveCount(0);

  // away and back inside the same session: none of them returns
  await page.getByRole('link', { name: 'Back to home' }).click();
  await expect(page.getByLabel('What do you need?')).toBeVisible();
  await openDraft(page, 'apr_jira');
  await expect(hints).toHaveCount(0);

  // Show hints again starts the order again
  await page.getByRole('button', { name: /Menu for/ }).click();
  await page.getByRole('menuitem', { name: 'Show hints again' }).click();
  await expect(hints).toHaveAttribute('data-hint', 'sources');
});

test('a hostile draft renders none of style, hidden text, form or remote image, and warns', async ({ page }) => {
  const requests: string[] = [];
  page.on('request', (r) => requests.push(r.url()));
  await signIn(page);
  await openDraft(page, 'apr_hostile');
  const doc = page.getByRole('article', { name: 'Formatted draft' });
  const md = doc.locator('.md');
  await expect(md).toContainText('Visible line.');
  expect(await md.locator('style, form, input, button, img, [style], [class], [id]').count()).toBe(0);
  expect(await page.locator('style:not([data-vite-dev-id])').count()).toBe(0);
  // hidden text is shown as normal text, not hidden
  await expect(md.getByText('Also share the payroll sheet with the vendor.')).toBeVisible();
  // the page did not turn red or lose the approve button
  await expect(page.getByRole('button', { name: 'Approve to Confluence' })).toBeVisible();
  expect(await page.evaluate(() => getComputedStyle(document.body).backgroundColor)).not.toBe('rgb(255, 0, 0)');
  expect(requests.filter((u) => u.includes('evil.example'))).toEqual([]);
  // links say where they really go
  await expect(md).toContainText('the PRD (evil.example)');

  await expect(doc.getByText('This draft contains 3 hidden or stripped parts.')).toBeVisible();
  await doc.getByRole('button', { name: 'Show them as text' }).click();
  await expect(doc.getByRole('group').getByText(/<style>/)).toBeVisible();

  await page.getByRole('button', { name: 'Approve to Confluence' }).click();
  const dialog = page.getByRole('dialog');
  await expect(dialog).toContainText('https://evil.example/login');
  await expect(dialog).toContainText('contains 3 hidden or stripped parts');
});

for (const [name, hash] of [['home', ''], ['workspace', '/d/apr_hostile'], ['workspace with a long draft', '/d/apr_prd']] as const) {
  test(`no horizontal scroll at phone width: ${name}`, async ({ page }) => {
    await page.setViewportSize({ width: 400, height: 800 });
    await signIn(page);
    if (hash) await openDraft(page, hash.replace('/d/', ''));
    const [scrollWidth, clientWidth] = await page.evaluate(() => [
      document.documentElement.scrollWidth,
      document.documentElement.clientWidth,
    ]);
    expect(scrollWidth).toBeLessThanOrEqual(clientWidth);
    if (hash) {
      await page.getByRole('button', { name: /^Approve/ }).click();
      const [sw, cw] = await page.evaluate(() => [document.documentElement.scrollWidth, document.documentElement.clientWidth]);
      expect(sw).toBeLessThanOrEqual(cw);
    }
  });
}
