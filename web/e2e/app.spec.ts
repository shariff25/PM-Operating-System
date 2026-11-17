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
  await page.getByRole('button', { name: 'Send', exact: true }).click();
  const thread = page.getByRole('region', { name: 'Conversation' });
  await expect(thread.getByText('Done', { exact: true })).toBeVisible();
  await expect(thread.locator('strong', { hasText: 'PAY-212' })).toBeVisible();
  await expect(page).toHaveURL(/#\/a\/tsk_/);
  // only milestones in the steps
  await thread.getByRole('button', { name: /Show steps/ }).click();
  await expect(thread.getByRole('list', { name: 'Steps' }).getByRole('listitem')).toHaveText(['Reading Jira', 'Reading Confluence', 'Putting the answer together']);
  await expect(thread.getByText('Claimed by pm-ana')).toHaveCount(0);
  await expect(thread.getByText('2 of 3 done')).toHaveCount(0);
});

test('asks for a draft and opens it as a formatted document', async ({ page }) => {
  await signIn(page);
  await page.getByLabel('What do you need?').fill('draft a PRD for retries');
  await page.getByLabel('What do you need?').press('Enter');
  await page.getByRole('button', { name: 'Open draft' }).click();
  const doc = page.getByRole('article', { name: 'Formatted draft' });
  await expect(doc.getByRole('heading', { level: 1 })).toContainText('PRD: retries');
  await expect(doc.getByRole('heading', { level: 2, name: 'Problem' })).toBeVisible();
  await expect(page.getByRole('button', { name: 'Edit' })).toBeVisible();
  // no editor: the only text box is the change request docked under the document
  await expect(page.getByLabel('Draft text')).toHaveCount(0);
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
  // no editor: the only text box is the change request docked under the document
  await expect(page.getByLabel('Draft text')).toHaveCount(0);
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
  await expect(page.getByRole('region', { name: 'Before you approve' })).toContainText('A suggested change is still open');
  await region.getByRole('button', { name: 'Accept change' }).click();
  await expect(region).toHaveCount(0);
  await expect(page.getByRole('article', { name: 'Formatted draft' })).not.toContainText('Who gets the banner');
  await expect(page.getByRole('region', { name: 'Before you approve' })).not.toContainText('A suggested change is still open');
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
  await expect(page.getByText(/Posted in #partner-pricing/)).toBeVisible();
});

test('an expired draft is labelled, sorted lower, and cannot be approved', async ({ page }) => {
  await signIn(page);
  await page.getByRole('navigation', { name: 'Main' }).getByRole('link', { name: /^Drafts/ }).click();
  await page.getByRole('tab', { name: /^All/ }).click();
  const rows = page.getByRole('tabpanel').getByRole('link');
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
  await page.getByRole('navigation', { name: 'Main' }).getByRole('link', { name: 'Today', exact: true }).click();
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

const noHScroll = async (page: Page, what: string) => {
  const [sw, cw] = await page.evaluate(() => [document.documentElement.scrollWidth, document.documentElement.clientWidth]);
  expect(sw, `horizontal scroll on ${what}`).toBeLessThanOrEqual(cw);
};

test('no horizontal scroll at 390px on every page, dialog and palette', async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await signIn(page);
  await noHScroll(page, 'home');
  for (const hash of ['/drafts', '/followups', '/activity', '/a/tsk_seed_prd', '/a/tsk_seed_failed', '/d/apr_hostile', '/d/apr_slack', '/d/apr_slack_old', '/d/apr_prd']) {
    await page.evaluate((h) => { window.location.hash = h; }, hash);
    await page.waitForTimeout(150);
    await noHScroll(page, hash);
  }
  for (const tab of ['Sources', 'Details', 'Draft']) {
    await page.getByRole('tab', { name: new RegExp(`^${tab}`) }).click();
    await noHScroll(page, `draft tab ${tab}`);
  }
  await page.getByRole('button', { name: /^Approve/ }).click();
  await noHScroll(page, 'approve dialog');
  await page.keyboard.press('Escape');
  await page.getByRole('button', { name: 'Ask or jump to…' }).click();
  await expect(page.getByRole('dialog', { name: 'Ask PM OS or jump to a page' })).toBeVisible();
  await noHScroll(page, 'palette');
});

test('phone: a bottom tab bar replaces the rail, and the user menu sits behind the avatar', async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await signIn(page);
  const bar = page.getByRole('navigation', { name: 'Main' });
  await expect(bar).toHaveClass(/tabbar/);
  const box = await bar.boundingBox();
  expect(box!.y + box!.height).toBeGreaterThan(830);
  await expect(page.locator('.rail')).toHaveCount(0);
  await bar.getByRole('link', { name: /^Activity/ }).click();
  await expect(bar.getByRole('link', { name: /^Activity/ })).toHaveAttribute('aria-current', 'page');
  await page.getByRole('button', { name: /Menu for/ }).click();
  await expect(page.getByRole('menuitem', { name: 'Sign out' })).toBeVisible();
});

test('desktop: the rail marks the current page, and turns to icons on a draft', async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 });
  await signIn(page);
  const nav = page.getByRole('navigation', { name: 'Main' });
  await expect(nav.getByRole('link', { name: 'Today', exact: true })).toHaveAttribute('aria-current', 'page');
  await nav.getByRole('link', { name: /^Follow-ups/ }).click();
  await expect(page.getByRole('heading', { level: 1, name: 'Follow-ups' })).toBeVisible();
  await expect(nav.getByRole('link', { name: /^Follow-ups/ })).toHaveAttribute('aria-current', 'page');
  await expect(nav.getByRole('link', { name: 'Today', exact: true })).not.toHaveAttribute('aria-current', 'page');
  const rail = page.locator('header.rail');
  expect((await rail.boundingBox())!.width).toBeGreaterThan(200);
  await openDraft(page, 'apr_prd');
  await expect(rail).toHaveClass(/rail-compact/);
  expect((await rail.boundingBox())!.width).toBeLessThanOrEqual(65);
  await expect(nav.getByRole('link', { name: /^Drafts/ })).toHaveAttribute('aria-current', 'page');
});

test('fonts come from this origin only', async ({ page }) => {
  const fonts: string[] = [];
  page.on('request', (r) => { if (r.resourceType() === 'font') fonts.push(r.url()); });
  await signIn(page);
  await page.evaluate(() => document.fonts.ready);
  expect(await page.evaluate(() => document.fonts.check('16px "Geist Variable"'))).toBe(true);
  expect(fonts.length).toBeGreaterThan(0);
  for (const u of fonts) expect(new URL(u).origin).toBe(new URL(page.url()).origin);
});

test('command palette: Ctrl+K, jump to a draft, ask from anywhere, Escape', async ({ page }) => {
  await signIn(page);
  await page.keyboard.press('Control+k');
  const dialog = page.getByRole('dialog', { name: 'Ask PM OS or jump to a page' });
  await expect(dialog).toBeVisible();
  await page.keyboard.press('Escape');
  await expect(dialog).toHaveCount(0);

  await page.keyboard.press('Control+k');
  await expect(dialog.getByRole('combobox')).toBeFocused();
  await page.keyboard.type('show grace');
  await page.keyboard.press('Enter');
  await expect(page).toHaveURL(/#\/d\/apr_jira$/);

  await expect(page.getByRole('article', { name: 'Formatted draft' })).toBeVisible();
  await page.keyboard.press('Control+k');
  await expect(dialog.getByRole('combobox')).toBeFocused();
  await page.keyboard.type('what is blocked');
  await expect(dialog.getByRole('option', { selected: true })).toHaveText(/Ask: what is blocked/);
  await page.keyboard.press('Enter');
  // the thread polls every two seconds in the dev app
  await expect(page.getByRole('region', { name: 'Conversation' }).getByText('Done', { exact: true })).toBeVisible({ timeout: 10_000 });
});

test('theme: dark and light from the menu change the page colours and survive a reload', async ({ page }) => {
  await signIn(page);
  const bg = () => page.evaluate(() => getComputedStyle(document.body).backgroundColor);
  const light = await bg();
  await page.getByRole('button', { name: /Menu for/ }).click();
  await page.getByRole('menuitemradio', { name: 'Dark' }).click();
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'dark');
  const dark = await bg();
  expect(dark).not.toBe(light);
  await page.reload();
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'dark');
});
