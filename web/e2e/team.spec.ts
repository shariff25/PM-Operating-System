import { expect, test } from '@playwright/test';
import type { Page } from '@playwright/test';

// The Team screen against the sample data. Each test starts from a fresh page, so the sample
// user database starts over each time.

async function signIn(page: Page, handle: string, search = '') {
  await page.goto('/' + search);
  await page.getByLabel('Your handle').fill(handle);
  await page.getByRole('button', { name: 'Sign in', exact: true }).click();
  await expect(page.getByLabel('What do you need?')).toBeVisible();
  await page.evaluate(() => { window.location.hash = '/admin/team'; });
  await expect(page.getByRole('table', { name: 'Team members' })).toBeVisible();
}

const row = (page: Page, name: string | RegExp) => page.getByRole('table', { name: 'Team members' }).getByRole('row', { name });

async function action(page: Page, who: string, item: string) {
  await page.getByRole('button', { name: `Actions for ${who}` }).click();
  await page.getByRole('menu', { name: `Actions for ${who}` }).getByRole('menuitem', { name: item }).click();
}

test('an admin invites a person, who then shows on the list', async ({ page }) => {
  await signIn(page, 'ada');
  await page.getByRole('button', { name: 'Invite a person' }).click();
  const d = page.getByRole('dialog', { name: 'Invite a person' });
  await expect(d.getByText(/Admin is not offered here/)).toBeVisible();
  await d.getByLabel('Handle').fill('cy');
  await d.getByLabel('Display name').fill('Cy Young');
  await d.getByLabel('Microsoft sign-in name').fill('cy@contoso.example');
  await d.getByLabel('Atlassian email').fill('cy@contoso.example');
  await d.getByLabel('GitHub login').fill('cy-young');
  await d.getByRole('checkbox', { name: /Reviewer/ }).check();
  await d.getByRole('button', { name: 'Invite', exact: true }).click();
  await expect(d).toBeHidden();
  await expect(page.getByText('Cy Young is in. They can sign in with Microsoft now.')).toBeVisible();
  await expect(row(page, /Cy Young/)).toContainText('Reviewer');
  await expect(row(page, /Cy Young/)).toContainText('cy@contoso.example');
});

test('an admin changes roles by keyboard, and Admin cannot be given', async ({ page }) => {
  await signIn(page, 'ada');
  const btn = page.getByRole('button', { name: 'Actions for Priya Nair' });
  await btn.focus();
  await page.keyboard.press('Enter');
  await expect(page.getByRole('menuitem', { name: 'Change roles' })).toBeFocused();
  await page.keyboard.press('Enter');
  const d = page.getByRole('dialog', { name: 'Roles for Priya Nair' });
  await expect(d.getByRole('checkbox', { name: /Admin/ })).toBeDisabled();
  await d.getByRole('checkbox', { name: /Product Manager/ }).check();
  await d.getByRole('button', { name: 'Save roles' }).click();
  await expect(d).toBeHidden();
  await expect(row(page, /Priya Nair/)).toContainText('Product Manager');
  await expect(btn).toBeFocused();
});

test('an admin deactivates someone and reactivates them within the grace', async ({ page }) => {
  await signIn(page, 'ada');
  await expect(page.getByText('2 of 3 deactivations left this hour')).toBeVisible();
  await action(page, 'Ben Carter', 'Deactivate');
  const d = page.getByRole('alertdialog', { name: 'Deactivate Ben Carter?' });
  await expect(d.getByText(/You can undo this for 24 hours/)).toBeVisible();
  await expect(d.getByText('2 of 3 deactivations left this hour, web and Slack together.')).toBeVisible();
  await d.getByRole('button', { name: 'Deactivate', exact: true }).click();
  await expect(d).toBeHidden();
  await expect(row(page, /Ben Carter/)).toContainText('Deactivated');
  await expect(row(page, /Ben Carter/)).toContainText(/Undo until/);
  await expect(page.getByText(/pmos-admin pause pm-ben/)).toBeVisible();
  await expect(page.getByText('1 of 3 deactivations left this hour')).toBeVisible();

  await action(page, 'Ben Carter', 'Reactivate');
  const r = page.getByRole('dialog', { name: 'Reactivate Ben Carter?' });
  await expect(r.getByText(/Admin is never restored here/)).toBeVisible();
  await r.getByRole('button', { name: 'Reactivate', exact: true }).click();
  await expect(r).toBeHidden();
  await expect(row(page, /Ben Carter/)).toContainText('Active');
});

test('an admin confirms a sign-in name for someone who joined from Slack', async ({ page }) => {
  await signIn(page, 'ada');
  await expect(row(page, /Sam Lee/)).toContainText('Not confirmed');
  await row(page, /Sam Lee/).getByRole('button', { name: 'Confirm the sign-in name for Sam Lee' }).click();
  const d = page.getByRole('dialog', { name: 'Confirm the sign-in name for Sam Lee' });
  await expect(d.getByLabel('Microsoft sign-in name')).toHaveValue('sam.lee@contoso.example');
  await d.getByRole('button', { name: 'Confirm sign-in name' }).click();
  await expect(d).toBeHidden();
  await expect(row(page, /Sam Lee/)).not.toContainText('Not confirmed');
  await expect(row(page, /Sam Lee/)).toContainText('Not signed in yet');
});

test('a refusal is said in plain words, never as a code', async ({ page }) => {
  await signIn(page, 'ada', '?mock_users=assertion_required');
  await action(page, 'Ravi Kumar', 'Edit details');
  const d = page.getByRole('dialog', { name: 'Details for Ravi Kumar' });
  await d.getByLabel('Display name').fill('Ravi K.');
  await d.getByRole('button', { name: 'Save details' }).click();
  await expect(d.getByRole('alert')).toContainText("isn't running with signed user assertions");
  await expect(d.getByRole('alert')).not.toContainText('assertion_required');
  await expect(row(page, /Ravi Kumar/)).toBeVisible();
});

test('an auditor reads the list without addresses or actions', async ({ page }) => {
  await signIn(page, 'audrey');
  await expect(page.getByText(/You're seeing the auditor's view/)).toBeVisible();
  await expect(page.getByRole('button', { name: 'Invite a person' })).toHaveCount(0);
  await expect(page.getByRole('button', { name: /^Actions for/ })).toHaveCount(0);
  await expect(page.getByRole('table', { name: 'Team members' })).not.toContainText('@contoso.example');
  await expect(row(page, /Raj Mehta/)).toContainText('Admin and Auditor');
  await expect(row(page, /Sam Lee/)).toContainText('Not confirmed');
});

test('a display name with markup is shown as text', async ({ page }) => {
  // The test server runs with the hostile sample data on.
  await signIn(page, 'ada');
  const mallory = row(page, /Mallory/);
  await expect(mallory).toContainText('<img src=x onerror="window.__pwned=1">**Mallory** <b>Admin</b>');
  await expect(mallory.locator('img, b')).toHaveCount(0);
  expect(await page.evaluate(() => (window as unknown as { __pwned?: number }).__pwned)).toBeUndefined();
});

test('the overview links each flagged person to the Team page', async ({ page }) => {
  await signIn(page, 'ada');
  await page.evaluate(() => { window.location.hash = '/admin'; });
  const card = page.getByRole('region', { name: /People who need an admin/ });
  await card.getByRole('link', { name: 'sam', exact: true }).click();
  await expect(page).toHaveURL(/#\/admin\/team\?person=sam$/);
  await expect(page.locator('#person-sam')).toHaveClass(/row-target/);
});

test('on a phone the Team page scrolls inside its card, and the invite button stays', async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await signIn(page, 'ada');
  await expect(page.getByRole('button', { name: 'Invite a person' })).toBeVisible();
  const overflow = await page.evaluate(() => document.documentElement.scrollWidth > window.innerWidth);
  expect(overflow).toBe(false);
  await page.getByRole('button', { name: 'Invite a person' }).click();
  await expect(page.getByRole('dialog', { name: 'Invite a person' })).toBeVisible();
  const dialogOverflow = await page.evaluate(() => document.documentElement.scrollWidth > window.innerWidth);
  expect(dialogOverflow).toBe(false);
});
