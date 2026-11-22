import AxeBuilder from '@axe-core/playwright';
import { expect, test } from '@playwright/test';
import type { Page } from '@playwright/test';

// The bot marketplace in the browser: "Pick your team", the note for people who were here before,
// the Bots page, the picker in the ask box and attribution. axe runs with every rule it ships on
// each screen and state, in both themes, at desktop and phone width.

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

const go = (page: Page, hash: string) => page.evaluate((h) => { window.location.hash = h; }, hash);
const step = (page: Page) => page.getByRole('heading', { level: 1, name: 'Pick your team' });
const card = (page: Page, name: string) => page.getByRole('listitem', { name });

test('a new PM picks a team in the guided start, then asks the bot they picked', async ({ page }) => {
  await signIn(page, '?mock_new=1');
  await expect(step(page)).toBeVisible();
  await expect(page.getByText('Getting started · Step 1 of 4')).toBeVisible();
  await expect(step(page)).toBeFocused();
  await expect(card(page, 'Nudge').getByRole('button', { name: 'Nudge: Always on your team' })).toBeDisabled();
  await expect(card(page, 'Chief').getByText('Not yet benchmarked')).toBeVisible();

  // keyboard: Tab to Chief's toggle and press Space
  const chief = card(page, 'Chief').getByRole('button', { name: /^Chief: / });
  await chief.focus();
  await page.keyboard.press('Space');
  await expect(chief).toHaveAttribute('aria-pressed', 'true');
  await expect(page.getByText('2 on your team:')).toBeVisible();
  await page.getByRole('button', { name: 'Continue with 2 bots' }).click();

  // back on Today: step 1 of 4 is done, and the picker offers Chief
  const welcome = page.getByRole('region', { name: "Hi Ana, I'm Nudge." });
  await expect(welcome.getByText('Getting started: 1 of 4 done')).toBeAttached();
  const picker = page.getByRole('button', { name: /^Asking Nudge/ });
  await expect(picker).toBeVisible();
  await picker.click();
  const list = page.getByRole('listbox', { name: 'Ask' });
  await expect(list).toBeFocused();
  await page.keyboard.press('ArrowDown');
  await page.keyboard.press('Enter');
  await expect(page.getByRole('button', { name: /^Asking Chief/ })).toBeFocused();
  // the chips are Chief's; a chip fills the box and sends nothing
  await page.getByRole('button', { name: 'What slipped since Monday?' }).click();
  await expect(page.getByLabel('What do you need?')).toHaveValue('What slipped since Monday?');
  await expect(page).not.toHaveURL(/#\/a\//);
  await page.keyboard.press('Enter');
  await expect(page).toHaveURL(/#\/a\//);
  await expect(page.getByRole('article', { name: 'Chief' })).toBeVisible();
});

test('"Skip for now" goes straight on, and Today does not send you back', async ({ page }) => {
  await signIn(page, '?mock_new=1');
  await expect(step(page)).toBeVisible();
  await page.getByRole('button', { name: 'Skip for now' }).click();
  await expect(page.getByLabel('What do you need?')).toBeVisible();
  await page.reload();
  await page.getByLabel('Your handle').fill('ana');
  await page.getByRole('button', { name: 'Sign in', exact: true }).click();
  await expect(page.getByLabel('What do you need?')).toBeVisible();
  await expect(step(page)).toHaveCount(0);
});

test('the Bots page adds and removes, asking about a bot\'s routines', async ({ page }) => {
  await signIn(page);
  await expect(page.getByLabel('What do you need?')).toBeVisible();
  await page.getByRole('navigation', { name: 'Main' }).getByRole('link', { name: /^Bots/ }).click();
  await expect(page.getByRole('heading', { level: 1, name: 'Bots' })).toBeVisible();
  await page.getByRole('button', { name: 'Add Echo to your team' }).click();
  await expect(page.getByText('Echo is on your team.')).toBeVisible();
  await expect(page.getByRole('button', { name: 'Remove Echo from your team' })).toBeFocused();

  await page.getByRole('button', { name: 'Remove Chief from your team' }).click();
  const dialog = page.getByRole('alertdialog', { name: 'Remove Chief from your team?' });
  await expect(dialog).toBeVisible();
  await expect(dialog.getByRole('radio', { name: /Move them to Nudge/ })).toBeChecked();
  await dialog.getByRole('radio', { name: /Pause them/ }).check();
  await dialog.getByRole('button', { name: 'Remove' }).click();
  await expect(page.getByText('Chief is off your team. Its drafts stay yours.')).toBeVisible();
  await expect(page.getByRole('button', { name: 'Add Chief to your team' })).toBeFocused();
});

test('a refused team change says so, and a withdrawn bot shows as no longer offered', async ({ page }) => {
  await signIn(page, '?mock_team=team_full&mock_bots=withdrawn');
  await expect(page.getByLabel('What do you need?')).toBeVisible();
  await go(page, '/bots');
  await expect(page.getByRole('heading', { level: 2, name: /^No longer offered/ })).toBeVisible();
  await page.getByRole('button', { name: 'Add Sprout to your team' }).click();
  await expect(page.getByRole('alert')).toContainText('A team holds at most 16 bots.');
});

test('drafts say which bot drafted them', async ({ page }) => {
  await signIn(page);
  await expect(page.getByLabel('What do you need?')).toBeVisible();
  await go(page, '/d/apr_jira');
  await expect(page.getByText('Drafted by Chief')).toBeVisible();
});

test('a Preview bot is tagged on a Today decide row and in the Drafts list, Nudge is not', async ({ page }) => {
  await signIn(page);
  await expect(page.getByLabel('What do you need?')).toBeVisible();
  const rows = page.locator('a.decide-row');
  await expect(rows.first()).toBeVisible();
  const chiefRow = rows.filter({ hasText: 'By Chief' }).first();
  const nudgeRow = rows.filter({ hasText: 'By Nudge' }).first();
  await expect(chiefRow.locator('.tag-preview')).toBeVisible();
  await expect(chiefRow.locator('.tag-preview')).toHaveText(/^Preview/);
  await expect(nudgeRow).toBeVisible();
  await expect(nudgeRow.locator('.tag-preview')).toHaveCount(0);
  await scan(page, 'Today with Preview tags on the decide rows');

  await go(page, '/drafts');
  const list = page.locator('a.trow');
  await expect(list.first()).toBeVisible();
  const chiefDraft = list.filter({ hasText: 'By Chief' }).first();
  const nudgeDraft = list.filter({ hasText: 'By Nudge' }).first();
  await expect(chiefDraft.locator('.tag-preview')).toBeVisible();
  await expect(nudgeDraft).toBeVisible();
  await expect(nudgeDraft.locator('.tag-preview')).toHaveCount(0);
  await scan(page, 'Drafts with Preview tags');
});

for (const theme of ['light', 'dark'] as const) {
  for (const width of [1280, 390] as const) {
    test(`axe on the marketplace screens, ${theme} theme, ${width}px wide`, async ({ page }) => {
      test.slow();
      await page.setViewportSize({ width, height: width === 390 ? 844 : 900 });
      await signIn(page, '?mock_new=1', theme);
      await expect(step(page)).toBeVisible();
      await expect(page.locator('html')).toHaveAttribute('data-theme', theme);
      await scan(page, 'Pick your team');
      await card(page, 'Sprout').getByRole('button', { name: /^Sprout: / }).click();
      await scan(page, 'Pick your team, one added');
      await page.getByRole('button', { name: 'Continue with 2 bots' }).click();
      await expect(page.getByLabel('What do you need?')).toBeVisible();
      await scan(page, 'Today with the picker and four steps');
      await page.getByRole('button', { name: /^Asking/ }).click();
      await expect(page.getByRole('listbox', { name: 'Ask' })).toBeVisible();
      await scan(page, 'the picker, open');
      await page.keyboard.press('Escape');
      await go(page, '/bots');
      await expect(page.getByRole('heading', { level: 1, name: 'Bots' })).toBeVisible();
      await scan(page, 'the Bots page');
      await page.getByRole('button', { name: 'Remove Sprout from your team' }).click();
      await expect(page.getByRole('alertdialog')).toBeVisible();
      await scan(page, 'the removal question');
      await page.keyboard.press('Escape');
      await page.getByRole('button', { name: 'Settings for Sprout' }).click();
      await expect(page.getByRole('dialog')).toBeVisible();
      await scan(page, 'a bot\'s settings');
      await page.keyboard.press('Escape');
      await go(page, '/d/apr_jira');
      await expect(page.getByText('Drafted by Chief')).toBeVisible();
      await scan(page, 'an attributed draft');
      await go(page, '/activity');
      await expect(page.getByRole('heading', { level: 1, name: 'Activity' })).toBeVisible();
      await scan(page, 'Activity with bots');
    });
  }

  test(`axe on the note for people who were here before, and a withdrawn bot, ${theme} theme`, async ({ page }) => {
    // Ana with Nudge only: the server says to show the note. A withdrawn bot is on her team too.
    await signIn(page, '?mock_bots=withdrawn', theme);
    await expect(page.getByLabel('What do you need?')).toBeVisible();
    await go(page, '/bots');
    await expect(page.getByRole('heading', { level: 2, name: /^No longer offered/ })).toBeVisible();
    await page.getByRole('button', { name: 'Details for A bot no longer offered' }).click();
    await scan(page, 'the Bots page with a withdrawn bot');
    await page.getByRole('button', { name: 'Remove Chief from your team' }).click();
    await page.getByRole('alertdialog').getByRole('button', { name: 'Remove' }).click();
    await expect(page.getByText('Chief is off your team. Its drafts stay yours.')).toBeVisible();
    await go(page, '/');
    await expect(page.getByRole('region', { name: 'New: you can add bots to your team' })).toBeVisible();
    await scan(page, 'the note on Today');
    await page.getByRole('button', { name: 'Not now' }).click();
    await expect(page.getByRole('region', { name: 'New: you can add bots to your team' })).toHaveCount(0);
  });
}
