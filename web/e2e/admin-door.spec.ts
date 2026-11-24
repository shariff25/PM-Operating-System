import AxeBuilder from '@axe-core/playwright';
import { expect, test } from '@playwright/test';
import type { Page } from '@playwright/test';

// Slice H10 (docs/openshell-any-host-spec.md §3.8): the admin screens live on the admin door, its
// own origin. These run the sample-data client as each door would serve it: the admin door at a
// deployment-unique `*.localhost` name, and the public door, where an Admin's or an Auditor's
// Admin entry opens the admin door's address. The real server's cookies and checks are
// crates/web/tests/admin_door.rs's.

const ADMIN = 'http://pmos-test.localhost:5194';
const PUBLIC = 'http://127.0.0.1:5195';

async function signIn(page: Page, base: string, handle: string) {
  await page.goto(`${base}/`);
  await page.getByLabel('Your handle').fill(handle);
  await page.getByRole('button', { name: 'Sign in', exact: true }).click();
}

test('the admin door shows only the admin and audit screens', async ({ page }) => {
  await signIn(page, ADMIN, 'ada');
  await expect(page.getByRole('heading', { level: 1, name: 'Overview' })).toBeVisible();
  const rail = page.getByRole('navigation', { name: 'Main' });
  await expect(rail.getByRole('link', { name: 'Admin' })).toBeVisible();
  for (const name of ['Today', 'Drafts', 'Activity', 'Bots']) {
    await expect(rail.getByRole('link', { name, exact: true })).toHaveCount(0);
  }
  // A PM's route is the admin overview here.
  await page.evaluate(() => { window.location.hash = '/drafts'; });
  await expect(page.getByRole('heading', { level: 1, name: 'Overview' })).toBeVisible();
  // The admin screens themselves work as before.
  await page.evaluate(() => { window.location.hash = '/admin/audit'; });
  await expect(page.getByRole('table', { name: 'Audit events' })).toBeVisible();
  const results = await new AxeBuilder({ page }).analyze();
  expect(results.violations).toEqual([]);
});

test("an auditor on the admin door sees the audit log, and no PM's screens", async ({ page }) => {
  await signIn(page, ADMIN, 'audrey');
  const rail = page.getByRole('navigation', { name: 'Main' });
  await expect(rail.getByRole('link', { name: 'Admin' })).toBeVisible();
  await expect(rail.getByRole('link', { name: 'Today', exact: true })).toHaveCount(0);
  await page.evaluate(() => { window.location.hash = '/admin/audit'; });
  await expect(page.getByRole('table', { name: 'Audit events' })).toBeVisible();
});

test("on the public door an admin's Admin entry opens the admin door's address", async ({ page }) => {
  await signIn(page, PUBLIC, 'ada');
  await expect(page.getByLabel('What do you need?')).toBeVisible();
  const rail = page.getByRole('navigation', { name: 'Main' });
  await expect(rail.getByRole('link', { name: 'Admin' })).toHaveAttribute('href', `${ADMIN}/#/admin`);
  // The old address of the admin screens says where they are now, and loads nothing of them.
  await page.evaluate(() => { window.location.hash = '/admin/audit'; });
  await expect(page.getByRole('heading', { level: 1, name: 'The admin screens have their own address' })).toBeVisible();
  await expect(page.getByRole('link', { name: 'Open the admin address' })).toHaveAttribute('href', `${ADMIN}/#/admin`);
  await expect(page.getByRole('table', { name: 'Audit events' })).toHaveCount(0);
  const results = await new AxeBuilder({ page }).analyze();
  expect(results.violations).toEqual([]);
});

test('on the public door a PM has no Admin entry at all', async ({ page }) => {
  await signIn(page, PUBLIC, 'ana');
  await expect(page.getByLabel('What do you need?')).toBeVisible();
  await expect(page.getByRole('navigation', { name: 'Main' }).getByRole('link', { name: 'Admin' })).toHaveCount(0);
});
