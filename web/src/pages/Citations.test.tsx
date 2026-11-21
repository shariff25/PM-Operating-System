import { screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';
import type { Api, Draft } from '../api';
import { copy } from '../copy';
import { renderApp } from '../test/util';

const open = (id: string, extra = {}) => renderApp({ hash: `/d/${id}`, ...extra });
const sources = () => screen.getByRole('region', { name: copy.draft.sourcesHeading });
const checklist = () => screen.getByRole('region', { name: copy.draft.checklistHeading });
const markers = (root: HTMLElement) => [...root.querySelectorAll<HTMLElement>('button.cite')];

const phone = () => {
  window.matchMedia = vi.fn().mockImplementation((query: string) => ({
    matches: query.includes('max-width'),
    media: query,
    addEventListener: () => {},
    removeEventListener: () => {},
  })) as unknown as typeof window.matchMedia;
};
afterEach(() => {
  delete (window as { matchMedia?: unknown }).matchMedia;
});

/** A draft body and sources of the test's own, in place of what the sample data holds. */
const withDraft = (patch: Partial<Draft>) => (a: Api) => {
  const orig = a.getDraft.bind(a);
  a.getDraft = async (id) => ({ ...(await orig(id)), ...patch });
  return a;
};

describe('source markers in the document', () => {
  it('shows each marker as a numbered button named after its source, and never the raw [[sN]]', async () => {
    open('apr_prd');
    const doc = await screen.findByRole('article', { name: copy.draft.formattedLabel });
    expect(doc).not.toHaveTextContent('[[s');
    const all = markers(doc);
    expect(all.length).toBe(6);
    const second = within(doc).getAllByRole('button', { name: 'Source 2: PAY-212: Payment failure handling' });
    expect(second.length).toBe(2);
    expect(second[0]).toHaveTextContent('2');
    for (const b of all) expect(b.tagName).toBe('BUTTON');
    // a marker in a heading-free paragraph sits in the text node of its sentence
    expect(within(doc).getByRole('heading', { level: 2, name: 'Problem' })).toBeInTheDocument();
  });

  it('groups markers that sit together, in the order of the cards', async () => {
    open('apr_prd');
    const doc = await screen.findByRole('article', { name: copy.draft.formattedLabel });
    const groups = [...doc.querySelectorAll('sup.cite-group')];
    const two = groups.find((g) => g.querySelectorAll('button').length === 2)!;
    expect([...two.querySelectorAll('button')].map((b) => b.textContent)).toEqual(['1', '4']);
  });

  it('renders nothing for a marker whose source is not listed, and leaves code as written', async () => {
    open('apr_prd', {
      wrap: withDraft({
        body: 'Known[[s1]] and unknown[[s9]] and `[[s1]]` in code.\n\n```\n[[s1]]\n```\n',
        sources: [{ id: 's1', system: 'docs', title: 'TCB grace period', url: null, excerpt: null, cited: true }],
      }),
    });
    const doc = await screen.findByRole('article', { name: copy.draft.formattedLabel });
    expect(markers(doc)).toHaveLength(1);
    expect(doc).not.toHaveTextContent('[[s9]]');
    expect(doc.querySelectorAll('code')).toHaveLength(2);
    expect([...doc.querySelectorAll('code')].every((c) => c.textContent?.includes('[[s1]]'))).toBe(true);
  });

  it('pressing a marker selects its card, moves focus to it and says how many places cite it', async () => {
    const user = userEvent.setup();
    open('apr_prd');
    const doc = await screen.findByRole('article', { name: copy.draft.formattedLabel });
    const scroll = vi.fn();
    Element.prototype.scrollIntoView = scroll;
    await user.click(within(doc).getAllByRole('button', { name: /^Source 1: ?/ })[0]!);
    const card = within(sources()).getByRole('button', { name: /^Source 1: ?/ });
    expect(card).toHaveAttribute('aria-pressed', 'true');
    expect(card).toHaveFocus();
    expect(scroll).toHaveBeenCalled();
    expect(document.getElementById('src-s1')).toHaveClass('source-item-flash');
    expect(sources()).toHaveTextContent(copy.draft.citedPlaces(2));
    // and the same markers light up in the text
    expect(markers(doc).filter((b) => b.classList.contains('cite-on')).map((b) => b.dataset.cite)).toEqual(['s1', 's1']);
  });

  it('works from the keyboard', async () => {
    const user = userEvent.setup();
    open('apr_prd');
    const doc = await screen.findByRole('article', { name: copy.draft.formattedLabel });
    const first = markers(doc)[0]!;
    first.focus();
    await user.keyboard('{Enter}');
    expect(within(sources()).getByRole('button', { name: /^Source 2: ?/ })).toHaveAttribute('aria-pressed', 'true');
  });

  it('shows the source title on hover and focus, and hides it again', async () => {
    const user = userEvent.setup();
    open('apr_prd');
    const doc = await screen.findByRole('article', { name: copy.draft.formattedLabel });
    const first = markers(doc)[0]!;
    await user.hover(first);
    expect(document.querySelector('.cite-tip')).toHaveTextContent('PAY-212: Payment failure handling');
    await user.unhover(first);
    expect(document.querySelector('.cite-tip')).toBeNull();
    first.focus();
    expect(await screen.findByText('PAY-212: Payment failure handling', { selector: '.cite-tip' })).toBeInTheDocument();
    await user.keyboard('{Escape}');
    expect(document.querySelector('.cite-tip')).toBeNull();
  });

  it('pressing a source card lights up its markers in the text, and pressing it again clears them', async () => {
    const user = userEvent.setup();
    open('apr_prd');
    const doc = await screen.findByRole('article', { name: copy.draft.formattedLabel });
    const card = within(sources()).getByRole('button', { name: /^Source 4: ?/ });
    await user.click(card);
    expect(markers(doc).filter((b) => b.classList.contains('cite-on'))).toHaveLength(2);
    expect(sources()).toHaveTextContent(copy.draft.citedPlaces(2));
    await user.click(card);
    expect(markers(doc).some((b) => b.classList.contains('cite-on'))).toBe(false);
  });

  it('on a phone, pressing a marker opens the Sources tab, and the card can send you back to the text', async () => {
    phone();
    const user = userEvent.setup();
    Element.prototype.scrollIntoView = vi.fn();
    open('apr_prd');
    const doc = await screen.findByRole('article', { name: copy.draft.formattedLabel });
    await user.click(markers(doc)[0]!);
    const tab = await screen.findByRole('tab', { name: new RegExp(`^${copy.panel.sources}`) });
    expect(tab).toHaveAttribute('aria-selected', 'true');
    expect(within(sources()).getByRole('button', { name: /^Source 2: ?/ })).toHaveFocus();
    await user.click(screen.getByRole('button', { name: copy.draft.showInDraft }));
    expect(await screen.findByRole('article', { name: copy.draft.formattedLabel })).toBeInTheDocument();
    expect(document.querySelector('.sheet button.cite-on')).toHaveFocus();
  });
});

describe('source cards', () => {
  it('keep their order, and mark the ones the text does not use as read, not cited', async () => {
    open('apr_prd');
    await screen.findByRole('article', { name: copy.draft.formattedLabel });
    const cards = within(sources()).getAllByRole('button').filter((b) => b.classList.contains('source'));
    expect(cards.map((c) => c.querySelector('.source-num')?.textContent?.replace(/^Source \d+: /, ''))).toEqual(['1', '2', '3', '4', '5']);
    const notCited = cards.filter((c) => c.textContent?.includes(copy.draft.notCited));
    expect(notCited.map((c) => c.querySelector('.source-title')?.textContent)).toEqual(['Confluence page 12345', 'Churn analysis Q3.xlsx']);
    expect(document.getElementById('src-s1')).toHaveClass('source-item-cited');
    expect(document.getElementById('src-s3')).toHaveClass('source-item-uncited');
  });

  it('say nothing about "not cited" when the draft cites nothing at all', async () => {
    open('apr_jira');
    await screen.findByRole('article', { name: copy.draft.formattedLabel });
    expect(sources()).not.toHaveTextContent(copy.draft.notCited);
  });
});

describe('before you approve: the source check', () => {
  it('says how many sources the text cites', async () => {
    open('apr_prd');
    await screen.findByRole('article', { name: copy.draft.formattedLabel });
    expect(checklist()).toHaveTextContent(copy.draft.checklist.cited(3, 5));
  });

  it('warns softly when there are sources and no marker at all', async () => {
    open('apr_jira');
    await screen.findByRole('article', { name: copy.draft.formattedLabel });
    const item = within(checklist()).getByText(copy.draft.checklist.noneCited).closest('li')!;
    expect(item).toHaveClass('check-warn');
  });

  it('says source links were removed once the PM edits, and shows no markers', async () => {
    const user = userEvent.setup();
    open('apr_prd');
    await user.click(await screen.findByRole('button', { name: copy.draft.edit }));
    await user.type(screen.getByLabelText(copy.draft.titleLabel), ' v2');
    await user.click(screen.getByRole('button', { name: copy.draft.save }));
    const doc = await screen.findByRole('article', { name: copy.draft.formattedLabel });
    expect(markers(doc)).toHaveLength(0);
    expect(checklist()).toHaveTextContent(copy.draft.checklist.editedNoLinks);
    expect(checklist()).not.toHaveTextContent(copy.draft.checklist.noneCited);
    expect(checklist()).not.toHaveTextContent(copy.draft.checklist.cited(3, 5));
    expect(sources()).not.toHaveTextContent(copy.draft.notCited);
  });

  it('does not warn on a practice draft, which never has links', async () => {
    const user = userEvent.setup();
    // Nothing offered besides Nudge, so the first visit stays on Today (no "Pick your team" step).
    renderApp({ noDrafts: true, catalog: 'nudge_only' });
    await user.click(await screen.findByRole('link', { name: /Practice: why PMs should take the stairs/ }));
    expect(await screen.findByText(copy.draft.practiceLabel + '.')).toBeInTheDocument();
    expect(checklist()).not.toHaveTextContent(copy.draft.checklist.noneCited);
  });
});

describe('markers are removed wherever text is plain', () => {
  it('the edit box shows the text without markers and is not "unsaved" until it changes', async () => {
    const user = userEvent.setup();
    open('apr_prd');
    await user.click(await screen.findByRole('button', { name: copy.draft.edit }));
    const box = screen.getByLabelText(copy.draft.bodyLabel) as HTMLTextAreaElement;
    expect(box.value).not.toContain('[[s');
    expect(box.value).toContain('lose access the moment a payment fails');
    expect(screen.getByRole('button', { name: copy.draft.save })).toBeDisabled();
    await user.click(screen.getByRole('button', { name: copy.draft.discard }));
    expect(screen.getByText(copy.draft.saved)).toBeInTheDocument();
  });

  it('the saved text sent to the server has no markers', async () => {
    const user = userEvent.setup();
    const sent: string[] = [];
    open('apr_prd', {
      wrap: (a: Api) => {
        const orig = a.saveDraft.bind(a);
        a.saveDraft = (id, input) => {
          sent.push(input.body);
          return orig(id, input);
        };
        return a;
      },
    });
    await user.click(await screen.findByRole('button', { name: copy.draft.edit }));
    await user.type(screen.getByLabelText(copy.draft.titleLabel), '!');
    await user.click(screen.getByRole('button', { name: copy.draft.save }));
    await screen.findByRole('article', { name: copy.draft.formattedLabel });
    expect(sent).toHaveLength(1);
    expect(sent[0]).not.toContain('[[s');
  });

  it('the suggestion diff is made on the text without markers', async () => {
    const user = userEvent.setup();
    open('apr_prd');
    await user.type(await screen.findByLabelText(copy.draft.askLabel), 'shorter');
    await user.click(screen.getByRole('button', { name: copy.draft.askButton }));
    const region = await screen.findByRole('region', { name: copy.draft.suggestionHeading });
    expect(region).not.toHaveTextContent('[[s');
    expect(region.querySelectorAll('button.cite')).toHaveLength(0);
    // unchanged paragraphs are not shown as changed just because they lost a marker
    expect(within(region).getAllByText(copy.draft.diffRemoved).length).toBeGreaterThan(0);
    expect(region.querySelectorAll('.dd-same').length).toBeGreaterThan(0);
  });

  it('titles in the list, the heading, the palette and the approve dialog never show one', async () => {
    const user = userEvent.setup();
    const dirty = (a: Api) => {
      const list = a.listDrafts.bind(a);
      a.listDrafts = async () => {
        const l = await list();
        return { ...l, drafts: l.drafts.map((d) => ({ ...d, title: `${d.title}[[s1]]` })) };
      };
      const get = a.getDraft.bind(a);
      a.getDraft = async (id) => ({ ...(await get(id)), title: 'PRD: TCB grace period setting[[s1]]' });
      return a;
    };
    renderApp({ hash: '/drafts', wrap: dirty });
    await screen.findAllByText('PRD: TCB grace period setting');
    expect(document.body).not.toHaveTextContent('[[s');
    window.location.hash = '/d/apr_prd';
    await screen.findByRole('heading', { level: 1, name: 'PRD: TCB grace period setting' });
    await user.click(screen.getByRole('button', { name: copy.approve.button('confluence_page') }));
    const dialog = await screen.findByRole('dialog', { name: copy.approve.confirmHeading });
    expect(dialog).not.toHaveTextContent('[[s');
    expect(dialog).toHaveTextContent('PRD: TCB grace period setting');
  });

  it('a thread answer never shows one', async () => {
    renderApp({
      hash: '/a/tsk_seed_week',
      wrap: (a: Api) => {
        const orig = a.getTask.bind(a);
        a.getTask = async (id) => ({ ...(await orig(id)), result: 'Billing moved[[s1]] this week[[s2]][[s3]].' });
        return a;
      },
    });
    expect(await screen.findByText(/Billing moved this week\./)).toBeInTheDocument();
    expect(document.body).not.toHaveTextContent('[[s');
  });

  it('link text in the approve dialog has no marker either', async () => {
    const user = userEvent.setup();
    open('apr_prd', {
      wrap: withDraft({ body: 'See [the roadmap[[s1]]](https://example.com/wiki/roadmap).' }),
    });
    await user.click(await screen.findByRole('button', { name: copy.approve.button('confluence_page') }));
    const dialog = await screen.findByRole('dialog', { name: copy.approve.confirmHeading });
    expect(dialog).toHaveTextContent('Link text: the roadmap');
    expect(dialog).not.toHaveTextContent('[[s');
    await waitFor(() => expect(dialog).toBeInTheDocument());
  });
});
