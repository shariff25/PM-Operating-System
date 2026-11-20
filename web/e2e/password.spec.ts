import { expect, test } from '@playwright/test';
import type { Page } from '@playwright/test';

// Password sign-in against the sample data (web/src/api/mockPassword.ts documents the sample
// people, passwords and codes). Each test starts from a fresh page, so the sample data starts over.

const ANA = { name: 'ana@contoso.example', password: 'paper-lantern-harbour' };
const TOTP = '246810';
const RECOVERY = 'kite-moss-reed-lamp';
const RAVI_CODE = 'SETUP-RAVI1-23456-78900';
const DANA_CODE = 'RESET-DANA1-23456-78900';
const NEW_PASSWORD = 'a calm river at dawn';

/**
 * Everything the page writes where it could leak: the address (every one it visits), web storage,
 * the console and the addresses of the requests it makes. `check` fails if any secret is there.
 */
function watch(page: Page) {
  const seen: string[] = [];
  page.on('console', (m) => seen.push(`console: ${m.text()}`));
  page.on('pageerror', (e) => seen.push(`error: ${e.message}`));
  page.on('request', (r) => seen.push(`request: ${r.url()}`));
  page.on('framenavigated', (f) => seen.push(`url: ${f.url()}`));
  return async (secrets: string[]) => {
    const storage = await page.evaluate(() => {
      const dump = (s: Storage) => Array.from({ length: s.length }, (_, i) => `${s.key(i)}=${s.getItem(s.key(i)!)}`).join('\n');
      return `${dump(window.localStorage)}\n${dump(window.sessionStorage)}\n${window.location.href}\n${window.history.length}`;
    });
    const all = [...seen, storage, page.url()].join('\n');
    const squash = (s: string) => s.replace(/[\s-]/g, '').toLowerCase();
    for (const s of secrets.filter((x) => x.length >= 6)) {
      expect(all.includes(s), `a secret leaked: ${s.slice(0, 3)}…`).toBe(false);
      expect(squash(all).includes(squash(s)), `a secret leaked (normalised): ${s.slice(0, 3)}…`).toBe(false);
    }
  };
}

async function passwordStep(page: Page, name = ANA.name, password = ANA.password) {
  await page.getByLabel('Sign-in name').fill(name);
  await page.getByLabel('Password', { exact: true }).fill(password);
  await page.getByRole('button', { name: 'Continue' }).click();
}

async function devSignIn(page: Page, handle: string, search = '?mock_providers=password,dev') {
  await page.goto('/' + search);
  await page.getByLabel('Your handle').fill(handle);
  await page.getByRole('button', { name: 'Sign in', exact: true }).click();
  await expect(page.getByLabel('What do you need?')).toBeVisible();
}

test('signs in with a password and an authenticator code, leaving no secret behind', async ({ page }) => {
  const check = watch(page);
  await page.goto('/?mock_providers=entra,password,dev');
  await passwordStep(page);
  const code = page.getByLabel('Authenticator code');
  await expect(code).toBeFocused();
  await expect(page.getByRole('link', { name: 'Sign in with Microsoft' })).toHaveCount(0);
  await code.pressSequentially('2468');
  await expect(page.getByRole('button', { name: 'Sign in' })).toBeDisabled();
  await code.press('Enter');
  await expect(code).toBeVisible();
  await code.pressSequentially('10');
  await page.getByRole('button', { name: 'Sign in' }).click();
  await expect(page.getByLabel('What do you need?')).toBeVisible();
  await check([ANA.password, TOTP]);
});

test('signs in with a recovery code and says how many are left', async ({ page }) => {
  const check = watch(page);
  await page.goto('/?mock_providers=password');
  await passwordStep(page);
  await page.getByRole('button', { name: 'Use a recovery code instead' }).click();
  await page.getByLabel('Recovery code').fill(RECOVERY);
  await page.getByRole('button', { name: 'Sign in' }).click();
  await expect(page.getByText('You used a recovery code. You have 9 left.')).toBeVisible();
  await expect(page.getByText(/Make a new set in Sign-in and security/)).toBeVisible();
  await page.getByRole('button', { name: 'Continue to PM OS' }).click();
  await expect(page.getByLabel('What do you need?')).toBeVisible();
  await check([ANA.password, RECOVERY]);
});

test('a wrong password and an unknown name get the same single sentence', async ({ page }) => {
  await page.goto('/?mock_providers=password');
  await passwordStep(page, ANA.name, 'wrong-wrong-wrong');
  const alert = page.getByRole('alert');
  await expect(alert).toHaveText("That sign-in name and password didn't work. Check them and try again, or wait a few minutes.");
  await expect(page.locator('.pw-note')).toBeFocused();
  const first = await alert.textContent();
  await passwordStep(page, 'nobody@contoso.example', 'wrong-wrong-wrong');
  await expect(alert).toHaveText(first!);
  await expect(page.getByLabel('Password', { exact: true })).toHaveValue('');
});

test('too many tries: a countdown, then the form comes back', async ({ page }) => {
  await page.goto('/?mock_providers=password&mock_password=too_many_attempts&mock_retry_after=3');
  await passwordStep(page);
  const note = page.getByTestId('wait-note');
  await expect(note).toBeVisible();
  await expect(note.getByRole('alert')).toHaveText('Too many tries from here. You can try again in 3 seconds.');
  await expect(note.locator('.pw-wait-clock')).toHaveText(/Try again in 0:0[1-3]/);
  await expect(page.getByRole('button', { name: 'Continue' })).toBeDisabled();
  await expect(page.getByText('You can try again now.')).toBeVisible({ timeout: 6000 });
  await expect(note).toHaveCount(0);
  await page.getByRole('button', { name: 'Continue' }).click();
  await expect(page.getByLabel('Authenticator code')).toBeVisible();
});

test('the whole setup, to the recovery codes and into the app, leaves no secret behind', async ({ page }) => {
  const check = watch(page);
  await page.goto('/?mock_providers=password');
  await page.getByRole('link', { name: 'I have a setup code' }).click();
  await expect(page).toHaveURL(/#\/setup$/);
  await expect(page.getByText('Step 1 of 4')).toBeVisible();
  await page.getByLabel('Sign-in name').fill('ravi@contoso.example');
  await page.getByLabel('Setup code').fill(RAVI_CODE);
  await page.getByRole('button', { name: 'Continue' }).click();

  await expect(page.getByText('Step 2 of 4')).toBeVisible();
  await expect(page.getByText('Setting up as ravi.')).toBeVisible();
  await expect(page.getByLabel('New password')).toBeFocused();
  await page.getByLabel('New password').fill(NEW_PASSWORD);
  await page.getByRole('button', { name: 'Continue' }).click();

  await expect(page.getByText('Step 3 of 4')).toBeVisible();
  await expect(page.getByRole('img', { name: /QR code that adds PM OS for ravi@contoso.example/ })).toBeVisible();
  const secret = ((await page.locator('.setup-key-text').textContent()) ?? '').replace(/\s/g, '');
  expect(secret).toMatch(/^[A-Z2-7]{32}$/);
  await page.getByRole('button', { name: "I've added it" }).click();

  await expect(page.getByText('Step 4 of 4')).toBeVisible();
  await page.getByLabel('Authenticator code').fill(TOTP);
  await page.getByRole('button', { name: 'Confirm and finish' }).click();

  await expect(page.getByRole('heading', { name: 'Save your recovery codes' })).toBeVisible();
  const codes = await page.getByRole('list', { name: 'Recovery codes' }).locator('code').allTextContents();
  expect(codes).toHaveLength(10);
  await expect(page.getByRole('img', { name: /QR code/ })).toHaveCount(0);
  const go = page.getByRole('button', { name: 'Continue to PM OS' });
  await expect(go).toBeDisabled();
  await page.getByRole('checkbox', { name: 'I have saved these somewhere safe' }).check();
  await go.click();
  await expect(page.getByLabel('What do you need?')).toBeVisible();
  await check([RAVI_CODE, NEW_PASSWORD, TOTP, secret, ...codes]);
});

test('a reset says plainly what it replaced', async ({ page }) => {
  await page.goto('/?mock_providers=password#/setup');
  await page.getByLabel('Sign-in name').fill('dana@contoso.example');
  await page.getByLabel('Setup code').fill(DANA_CODE.toLowerCase());
  await page.getByRole('button', { name: 'Continue' }).click();
  await expect(page.getByText(/This is a reset\. When you finish, it replaces your password/)).toBeVisible();
  await page.getByLabel('New password').fill(NEW_PASSWORD);
  await page.getByRole('button', { name: 'Continue' }).click();
  await page.getByRole('button', { name: "I've added it" }).click();
  await page.getByLabel('Authenticator code').fill(TOTP);
  await page.getByRole('button', { name: 'Confirm and finish' }).click();
  await expect(page.getByText('Your old password, authenticator and recovery codes no longer work, and your other sessions were signed out.')).toBeVisible();
});

test('changes the password from Sign-in and security', async ({ page }) => {
  const check = watch(page);
  await devSignIn(page, 'ana');
  await page.getByRole('button', { name: 'Menu for Ana Silva' }).click();
  await page.getByRole('menuitem', { name: 'Sign-in and security' }).click();
  await expect(page.getByRole('heading', { level: 1, name: 'Sign-in and security' })).toBeVisible();
  const form = page.getByRole('region', { name: 'Change your password' });
  await expect(form.getByText('Changing it signs you out everywhere else. This browser stays signed in.')).toBeVisible();
  await form.getByLabel('Current password').fill(ANA.password);
  await form.getByLabel('Authenticator code').fill(TOTP);
  await form.getByLabel('New password').fill(NEW_PASSWORD);
  await form.getByRole('button', { name: 'Change password' }).click();
  await expect(form.getByText('Your password is changed. Your other sessions were signed out.')).toBeVisible();
  await check([ANA.password, NEW_PASSWORD, TOTP]);
});

test('makes new recovery codes, shown once', async ({ page }) => {
  await devSignIn(page, 'ana');
  await page.evaluate(() => { window.location.hash = '/security'; });
  const card = page.getByRole('region', { name: 'Recovery codes' });
  await expect(card.getByText('Making a new set replaces all ten: the codes you have now stop working.')).toBeVisible();
  await card.getByLabel('Authenticator code').fill(TOTP);
  await card.getByRole('button', { name: 'Make new codes' }).click();
  await expect(card.getByRole('heading', { name: 'Your new recovery codes' })).toBeFocused();
  await expect(card.getByRole('list', { name: 'Recovery codes' }).locator('li')).toHaveCount(10);
  await expect(card.getByRole('button', { name: 'Done' })).toBeDisabled();
  await card.getByRole('checkbox', { name: 'I have saved these somewhere safe' }).check();
  await card.getByRole('button', { name: 'Done' }).click();
  await expect(card.getByRole('list', { name: 'Recovery codes' })).toHaveCount(0);
});

test('an admin creates a setup code, and an Admin target is sent to the host', async ({ page }) => {
  await devSignIn(page, 'ada', '?mock_providers=entra,password,dev');
  await page.evaluate(() => { window.location.hash = '/admin/team'; });
  const table = page.getByRole('table', { name: 'Team members' });
  await expect(table.getByRole('row', { name: /Ravi Kumar/ })).toContainText('Setup code out');

  await page.getByRole('button', { name: 'Actions for Ravi Kumar' }).click();
  await page.getByRole('menuitem', { name: 'Create a setup code' }).click();
  await page.getByRole('dialog', { name: 'Create a setup code for Ravi Kumar?' }).getByRole('button', { name: 'Create code' }).click();
  const made = page.getByRole('dialog', { name: 'Setup code for Ravi Kumar' });
  await expect(made.locator('.setup-code')).toHaveText(/^[A-Z0-9]{5}(-[A-Z0-9]{5}){3}$/);
  await expect(made.getByText(/This is the only time it is shown\. Copy it now\. Works once, until/)).toBeVisible();
  await expect(made.getByText('Pass it on in person or in a direct message, never in a public channel.')).toBeVisible();
  await expect(made.getByRole('button', { name: 'Copy code' })).toBeFocused();
  await made.getByRole('button', { name: 'Done' }).click();
  await expect(made).toBeHidden();
  await expect(page.getByRole('button', { name: 'Actions for Ravi Kumar' })).toBeFocused();

  await page.getByRole('button', { name: 'Actions for Raj Mehta' }).click();
  await page.getByRole('menuitem', { name: 'Create a setup code' }).click();
  const host = page.getByRole('dialog', { name: 'A setup code for Raj Mehta' });
  await expect(host.getByText(/holds Admin or Auditor, so their code must come from someone with host access/)).toBeVisible();
  await expect(host.getByText('pmos-admin users password-setup raj')).toBeVisible();
  await expect(host.getByRole('button', { name: 'Create code' })).toHaveCount(0);
});

test('the server refusing a code for an Admin says what to do instead', async ({ page }) => {
  await devSignIn(page, 'ada', '?mock_providers=password,dev&mock_users=host_only');
  await page.evaluate(() => { window.location.hash = '/admin/team'; });
  await page.getByRole('button', { name: 'Actions for Ravi Kumar' }).click();
  await page.getByRole('menuitem', { name: 'Create a setup code' }).click();
  const d = page.getByRole('dialog');
  await d.getByRole('button', { name: 'Create code' }).click();
  await expect(d.getByRole('alert')).toContainText('must come from someone with host access, with pmos-admin users password-setup');
  await expect(d.getByRole('alert')).not.toContainText('host_only');
});

for (const [providers, expectMs, expectPw, expectDev] of [
  ['password', false, true, false],
  ['entra,password', true, true, false],
  ['entra,password,dev', true, true, true],
] as const) {
  test(`the sign-in screen with ${providers}`, async ({ page }) => {
    await page.goto(`/?mock_providers=${providers}`);
    await expect(page.getByLabel('Sign-in name')).toBeVisible();
    await expect(page.getByRole('link', { name: 'Sign in with Microsoft' })).toHaveCount(expectMs ? 1 : 0);
    await expect(page.getByLabel('Password', { exact: true })).toHaveCount(expectPw ? 1 : 0);
    await expect(page.getByLabel('Your handle')).toHaveCount(expectDev ? 1 : 0);
    await expect(page.getByRole('button', { name: 'Continue' })).toHaveClass(expectMs ? /^btn\s*$/ : /btn-primary/);
    const overflow = await page.evaluate(() => document.documentElement.scrollWidth > window.innerWidth);
    expect(overflow).toBe(false);
  });
}

test('at 390 px the sign-in and setup pages fit', async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto('/?mock_providers=entra,password,dev');
  await expect(page.getByLabel('Sign-in name')).toBeVisible();
  expect(await page.evaluate(() => document.documentElement.scrollWidth > window.innerWidth)).toBe(false);
  await page.goto('/?mock_providers=password#/setup');
  await page.getByLabel('Sign-in name').fill('ravi@contoso.example');
  await page.getByLabel('Setup code').fill(RAVI_CODE);
  await page.getByRole('button', { name: 'Continue' }).click();
  await expect(page.getByLabel('New password')).toBeVisible();
  expect(await page.evaluate(() => document.documentElement.scrollWidth > window.innerWidth)).toBe(false);
  await page.getByLabel('New password').fill(NEW_PASSWORD);
  await page.getByRole('button', { name: 'Continue' }).click();
  await expect(page.getByRole('img', { name: /QR code/ })).toBeVisible();
  expect(await page.evaluate(() => document.documentElement.scrollWidth > window.innerWidth)).toBe(false);
  await page.getByRole('button', { name: "I've added it" }).click();
  await page.getByLabel('Authenticator code').fill(TOTP);
  await page.getByRole('button', { name: 'Confirm and finish' }).click();
  await expect(page.getByRole('list', { name: 'Recovery codes' })).toBeVisible();
  expect(await page.evaluate(() => document.documentElement.scrollWidth > window.innerWidth)).toBe(false);
});
