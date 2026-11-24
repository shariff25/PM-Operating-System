import { expect, test } from '@playwright/test';
import type { Page } from '@playwright/test';

// Policy and content admin, end to end on the sample data. Each test starts from a fresh page.

async function signIn(page: Page, handle: string, search = '') {
  await page.goto('/' + search);
  await page.getByLabel('Your handle').fill(handle);
  await page.getByRole('button', { name: 'Sign in', exact: true }).click();
  await expect(page.getByLabel('What do you need?')).toBeVisible();
}

async function go(page: Page, hash: string) {
  await page.evaluate((h) => { window.location.hash = h; }, hash);
}

test('an admin changes a policy, checks it, sees the widening, and opens a pull request', async ({ page }) => {
  await signIn(page, 'ada');
  await page.getByRole('navigation', { name: 'Main' }).getByRole('link', { name: 'Admin' }).click();
  await page.getByRole('list', { name: 'Admin sections' }).getByRole('link', { name: 'Policy' }).click();
  await expect(page.getByRole('heading', { name: /^What each bot may do/ })).toBeVisible();
  await page.getByRole('navigation', { name: 'Parts of the policy page' }).getByRole('link', { name: 'Propose a change' }).click();
  await expect(page).toHaveURL(/#\/admin\/policy\/propose$/);
  const open = page.getByRole('button', { name: 'Open a pull request' });
  await expect(open).toBeDisabled();

  await page.getByRole('tab', { name: 'Skills' }).click();
  await page.getByLabel('Tools for docs').fill('docs.search docs.read jira.search');
  await page.getByRole('button', { name: 'Check this change' }).click();
  const wider = page.getByRole('region', { name: 'Wider: 1' });
  await expect(wider).toContainText('docs may now call the tool jira_search');
  await page.getByText('Files this change writes (2)').click();
  await expect(page.getByRole('region', { name: 'Changes to policies/bots/docs.yaml' })).toBeHidden();
  await page.locator('summary', { hasText: 'policies/bots/docs.yaml' }).click();
  await expect(page.getByRole('region', { name: 'Changes to policies/bots/docs.yaml' })).toContainText('jira_search');

  await open.click();
  const dialog = page.getByRole('dialog', { name: 'Open a pull request' });
  await expect(dialog.getByRole('group', { name: 'This widens what bots may do (1)' })).toContainText('jira_search');
  await expect(dialog.getByLabel('Title')).toBeFocused();
  await dialog.getByLabel('Title').fill('Let the docs bot search Jira');
  await dialog.getByLabel('Why this change').fill('Support asked for answers that cite the Jira issue.');
  await dialog.getByRole('button', { name: 'Open the pull request' }).click();
  await expect(page.getByRole('heading', { name: 'Pull request #42 is open' })).toBeVisible();
  await expect(page.getByRole('link', { name: /Open in GitHub/ })).toHaveAttribute('href', 'https://github.com/acme/pmos/pull/42');
  await page.getByRole('navigation', { name: 'Parts of the policy page' }).getByRole('link', { name: 'Proposals' }).click();
  await expect(page).toHaveURL(/#\/admin\/policy\/proposals$/);
  await expect(page.getByRole('list', { name: 'Open policy proposals' })).toContainText('PM OS policy: Let the docs bot search Jira');
});

test('a refused proposal reads as its own sentence, and an unknown outcome sends the admin to the list', async ({ page }) => {
  await signIn(page, 'ada', '?mock_proposal=unknown_outcome');
  await go(page, '/admin/policy/propose');
  await page.getByRole('tab', { name: 'Bots' }).click();
  await page.getByLabel('Owner of docs').fill('ana');
  await page.getByRole('button', { name: 'Check this change' }).click();
  await page.getByRole('button', { name: 'Open a pull request' }).click();
  const dialog = page.getByRole('dialog', { name: 'Open a pull request' });
  await dialog.getByLabel('Title').fill('Give the docs bot an owner');
  await dialog.getByLabel('Why this change').fill('Ana runs the docs now.');
  await dialog.getByRole('button', { name: 'Open the pull request' }).click();
  const alert = dialog.getByRole('alert');
  await expect(alert).toContainText("I can't tell whether the pull request was opened");
  await alert.getByRole('button', { name: 'Check the open proposals' }).click();
  await expect(dialog).toBeHidden();
  await expect(page.locator('#policy-proposals')).toBeFocused();
});

test('an admin edits, deletes and adds content files, checks them together, and opens one pull request', async ({ page }) => {
  await signIn(page, 'ada');
  await go(page, '/admin/content');
  const tree = page.getByRole('navigation', { name: 'Content files' });
  await page.getByLabel('Search files').fill('launch');
  await tree.getByRole('button', { name: /Launch checklist/ }).click();
  await page.getByRole('button', { name: 'Edit', exact: true }).click();
  const editor = page.getByLabel('Text of content/playbooks/launch-checklist.md');
  await editor.fill((await editor.inputValue()).replace('48 hours', '72 hours'));
  await page.getByRole('button', { name: 'Done editing' }).click();
  await expect(page.getByText('Edited, not proposed yet')).toBeVisible();

  await go(page, '/admin/content?file=content%2Ftemplates%2Fone-pager.md');
  await expect(page.getByRole('heading', { name: 'templates/one-pager.md' })).toBeVisible();
  await expect(page.getByRole('button', { name: 'Delete', exact: true })).toBeEnabled();
  await page.getByRole('button', { name: 'Delete', exact: true }).click();

  await page.getByRole('button', { name: 'New file' }).click();
  await page.getByLabel('Path', { exact: true }).fill('content/playbooks/handoff.md');
  await page.getByRole('button', { name: 'Add to the change' }).click();

  const tray = page.getByRole('region', { name: 'Changes to propose' });
  await expect(tray).toContainText('3 files changed');
  await tray.getByRole('button', { name: 'Check these changes' }).click();
  await expect(tray.getByRole('heading', { name: 'Check result' })).toBeVisible();
  await expect(tray).toContainText('No sandbox changes');
  await tray.getByRole('button', { name: 'Open a pull request' }).click();
  const dialog = page.getByRole('dialog', { name: 'Open a pull request' });
  await expect(dialog).toContainText('Nothing in this change widens what a bot may do.');
  await dialog.getByLabel('Title').fill('Tidy the playbooks');
  await dialog.getByLabel('Why this change').fill('Longer watch after launch, and a handoff playbook.');
  await dialog.getByRole('button', { name: 'Open the pull request' }).click();
  await expect(page.getByRole('heading', { name: 'Pull request #42 is open' })).toBeVisible();
  await expect(tray.getByRole('list', { name: 'Files in this change' })).toBeHidden();
});

test('an auditor sees policy and content read-only, with locked files', async ({ page }) => {
  await signIn(page, 'audrey');
  await go(page, '/admin/policy');
  await expect(page.getByRole('heading', { name: /^What each bot may do/ })).toBeVisible();
  await expect(page.getByText('Only admins can propose changes.', { exact: false })).toBeVisible();
  await expect(page.getByRole('button', { name: 'Check this change' })).toHaveCount(0);
  await expect(page.getByRole('button', { name: 'Open a pull request' })).toHaveCount(0);

  await go(page, '/admin/content');
  const tree = page.getByRole('navigation', { name: 'Content files' });
  await expect(tree.getByText('Only admins can open personal files and restricted context.')).toHaveCount(3);
  await expect(tree.getByRole('button', { name: /Roadmap/ })).toHaveCount(0);
  await expect(page.getByRole('button', { name: 'New file' })).toHaveCount(0);
  await tree.getByRole('button', { name: /House rules/ }).click();
  await expect(page.getByRole('heading', { name: 'master/AGENTS.md' })).toBeVisible();
  await expect(page.getByRole('button', { name: 'Edit', exact: true })).toHaveCount(0);
  await expect(page.getByRole('table', { name: 'Bots in the harness' })).toBeVisible();
});

test('the palette jumps to policy and content', async ({ page }) => {
  await signIn(page, 'ada');
  await page.keyboard.press('Control+k');
  await page.getByLabel('Ask, or type a page or draft name').fill('Content');
  await page.getByRole('option', { name: /Content and harness/ }).click();
  await expect(page.getByRole('heading', { level: 1, name: 'Content and harness' })).toBeVisible();
});

test('the MCP servers on the policy page: every state in words, the same for an auditor, read-only', async ({ page }) => {
  await signIn(page, 'ada');
  await go(page, '/admin/policy');
  const section = page.getByRole('region', { name: /^MCP servers/ });
  await expect(section.getByText('On the built-in connectors: Confluence.')).toBeVisible();
  const tracker = section.getByRole('article', { name: 'acme-tracker' });
  await expect(tracker.getByText('Tools changed')).toBeVisible();
  await expect(tracker.getByRole('note')).toContainText('jira.update_issue');
  await expect(tracker.getByText("Changed at the server: can't be used")).toBeVisible();
  await expect(tracker.getByText('Every write still waits for the PM to approve it before it is sent.')).toBeVisible();
  await expect(tracker.getByText('Each PM signs in with their own account')).toBeVisible();
  const chat = section.getByRole('article', { name: 'chat-selfhosted' });
  await expect(chat.getByText('Check failed').first()).toBeVisible();
  await expect(chat.getByText(/connection refused/)).toBeVisible();
  await expect(chat.getByText("Gone from the server: can't be used")).toBeVisible();
  await expect(section.getByRole('article', { name: 'pmos-docs' }).getByText('Check passed')).toBeVisible();
  // Read-only: no buttons; a change goes through a content proposal.
  await expect(section.getByRole('button')).toHaveCount(0);
  await section.getByRole('link', { name: 'Propose it on the Content page' }).click();
  await expect(page).toHaveURL(/#\/admin\/content$/);

  await signIn(page, 'audrey');
  await go(page, '/admin/policy');
  const seen = page.getByRole('region', { name: /^MCP servers/ });
  await expect(seen.getByRole('article')).toHaveCount(3);
  await expect(seen.getByRole('article', { name: 'acme-tracker' }).getByText('mcp.tracker.example.test', { exact: true })).toBeVisible();
  await expect(seen.getByRole('link', { name: 'Propose it on the Content page' })).toHaveCount(0);

  await signIn(page, 'ada', '?mock_mcp=none');
  await go(page, '/admin/policy');
  await expect(page.getByRole('region', { name: /^MCP servers/ }).getByText('No MCP servers are in use. Jira, Confluence, Slack and the docs all go through the built-in connectors.')).toBeVisible();
});
