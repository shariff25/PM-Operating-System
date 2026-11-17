import { readFileSync } from 'node:fs';
import { expect, test } from '@playwright/test';
import type { Page } from '@playwright/test';

// Each test starts from a fresh page; the sample-data client keeps its state in the page.

async function signIn(page: Page, handle: string, hash = '') {
  await page.goto('/');
  await page.getByLabel('Your handle').fill(handle);
  await page.getByRole('button', { name: 'Sign in', exact: true }).click();
  await expect(page.getByLabel('What do you need?')).toBeVisible();
  if (hash) await page.evaluate((h) => { window.location.hash = h.replace(/^#/, ''); }, hash);
}

async function go(page: Page, hash: string) {
  await page.evaluate((h) => { window.location.hash = h; }, hash);
}

test('an admin finds the Admin area from the rail and walks through all four parts', async ({ page }) => {
  await signIn(page, 'ada');
  const rail = page.getByRole('navigation', { name: 'Main' });
  await rail.getByRole('link', { name: 'Admin' }).click();
  await expect(page.getByRole('heading', { level: 1, name: 'Overview' })).toBeVisible();
  await expect(page.getByRole('group', { name: 'Team counts' })).toBeVisible();
  const sub = rail.getByRole('list', { name: 'Admin sections' });
  await sub.getByRole('link', { name: 'Audit log' }).click();
  await expect(page.getByRole('table', { name: 'Audit events' })).toBeVisible();
  await sub.getByRole('link', { name: 'Team' }).click();
  await expect(page.getByRole('table', { name: 'Team members' })).toBeVisible();
  await sub.getByRole('link', { name: 'Connectors' }).click();
  await expect(page.getByRole('table', { name: 'Connectors' })).toBeVisible();
});

test('the audit filters live in the address, survive a reload and clear', async ({ page }) => {
  await signIn(page, 'audrey', '#/admin/audit');
  await expect(page.getByRole('table', { name: 'Audit events' })).toBeVisible();
  await page.getByLabel('Outcome').selectOption('denied');
  await expect(page).toHaveURL(/#\/admin\/audit\?outcome=denied$/);
  await expect(page.getByText('50 shown')).toHaveCount(0);
  const rows = page.getByRole('table', { name: 'Audit events' }).getByRole('row');
  await expect(rows.nth(1)).toContainText('Denied');
  // A bookmark: the sample client is in the page, so sign in again at the filtered address.
  await signIn(page, 'audrey', '#/admin/audit?outcome=denied&actor=ben');
  await expect(page.getByLabel('Outcome')).toHaveValue('denied');
  await expect(page.getByLabel('Who')).toHaveValue('ben');
  await page.getByRole('button', { name: 'Clear filters' }).click();
  await expect(page).toHaveURL(/#\/admin\/audit$/);
});

test('loads more events, opens a row by keyboard and shows every field', async ({ page }) => {
  await signIn(page, 'audrey', '#/admin/audit');
  const table = page.getByRole('table', { name: 'Audit events' });
  await expect(table.getByRole('row')).toHaveCount(51);
  await page.getByRole('button', { name: 'Load more' }).click();
  await expect(table.getByRole('row')).toHaveCount(101);
  const toggle = table.getByRole('button', { name: /^Show details for/ }).first();
  await toggle.focus();
  await page.keyboard.press('Enter');
  const details = page.getByRole('group', { name: 'Every field of this event' });
  await expect(details).toBeVisible();
  await expect(details.getByText('Event id')).toBeVisible();
  await expect(details.getByText('Not recorded').first()).toBeVisible();
});

test('exports a CSV with the current filters', async ({ page }) => {
  await signIn(page, 'ada', '#/admin/audit?outcome=denied');
  await expect(page.getByRole('table', { name: 'Audit events' })).toBeVisible();
  const download = page.waitForEvent('download');
  await page.getByRole('button', { name: 'Export CSV' }).click();
  const file = await download;
  expect(file.suggestedFilename()).toBe('pmos-audit.csv');
  const text = readFileSync(await file.path(), 'utf8');
  const lines = text.trim().split('\r\n');
  expect(lines[0]).toContain('"at","actor"');
  expect(lines.length).toBeGreaterThan(1);
  for (const l of lines.slice(1)) expect(l).toContain('"denied"');
  await expect(page.getByRole('status')).toContainText('pmos-audit.csv');
});

test('an auditor has no Connectors entry and gets a calm page on a direct visit', async ({ page }) => {
  await signIn(page, 'audrey');
  const rail = page.getByRole('navigation', { name: 'Main' });
  await rail.getByRole('link', { name: 'Admin' }).click();
  await expect(page.getByRole('link', { name: /Connectors/ })).toHaveCount(0);
  await go(page, '/admin/connectors');
  await expect(page.getByRole('heading', { level: 1, name: "You don't have access to this" })).toBeVisible();
});

test('a PM has no Admin entry and gets a calm page on a direct visit', async ({ page }) => {
  await signIn(page, 'ana');
  await expect(page.getByRole('navigation', { name: 'Main' }).getByRole('link', { name: 'Admin' })).toHaveCount(0);
  await go(page, '/admin/audit');
  await expect(page.getByRole('heading', { level: 1, name: "You don't have access to this" })).toBeVisible();
});

test('on a phone the Admin tab opens a row of section tabs', async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await signIn(page, 'ada');
  await page.getByRole('navigation', { name: 'Main' }).getByRole('link', { name: 'Admin' }).click();
  const tabs = page.getByRole('navigation', { name: 'Admin sections' });
  await tabs.getByRole('link', { name: 'Team' }).click();
  await expect(page.getByRole('table', { name: 'Team members' })).toBeVisible();
  await expect(page.locator('body')).not.toHaveJSProperty('scrollWidth', 0);
  const overflow = await page.evaluate(() => document.documentElement.scrollWidth > window.innerWidth);
  expect(overflow).toBe(false);
});
