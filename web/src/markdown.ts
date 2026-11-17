import DOMPurify from 'dompurify';
import { Marked } from 'marked';
import type { Renderer, Tokens } from 'marked';
import { MARKER, stripFromLine } from './citations';
import { copy } from './copy';

/** One link destination found in a text. */
export interface LinkRef {
  text: string;
  href: string;
  host: string;
}

export interface Rendered {
  html: string;
  /** Raw pieces that were left out of the formatted view (HTML, comments, hidden characters). */
  stripped: string[];
  links: LinkRef[];
  /** For each source id, how many places in the text cite it. Only sources that were listed count. */
  cites: Record<string, number>;
}

/** What the renderer needs to know about a source to draw its marker. */
export interface CiteSource {
  id: string;
  title: string;
}

export interface RenderOptions {
  /**
   * The draft's sources, in the order of their cards. A marker becomes a numbered link to the card
   * at that position. Without it, or for an id that is not listed, a marker renders nothing.
   */
  sources?: readonly CiteSource[];
}

// Text from a bot that read pages other people can write to. Only plain formatting survives.
const ALLOWED_TAGS = [
  'h1', 'h2', 'h3', 'h4', 'h5', 'h6', 'p', 'br', 'hr', 'ul', 'ol', 'li', 'em', 'strong', 'del',
  'code', 'pre', 'blockquote', 'table', 'thead', 'tbody', 'tr', 'th', 'td', 'a', 'small',
];

DOMPurify.addHook('afterSanitizeAttributes', (node) => {
  if (node.tagName !== 'A') return;
  const href = node.getAttribute('href') ?? '';
  if (!/^https?:\/\//i.test(href)) {
    node.removeAttribute('href');
  } else {
    node.setAttribute('target', '_blank');
    node.setAttribute('rel', 'noopener noreferrer');
  }
});

const INVISIBLE_SOURCE =
  '[\\u00AD\\u200B\\u200E\\u200F\\u202A-\\u202E\\u2060-\\u2064\\u2066-\\u2069\\uFEFF\\u{E0000}-\\u{E007F}]+';

const INVISIBLE = new RegExp(INVISIBLE_SOURCE, 'gu');

const esc = (s: string) =>
  s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;').replace(/'/g, '&#39;');

const bare = (s: string) => s.trim().toLowerCase().replace(/^https?:\/\//, '').replace(/\/+$/, '');

const GROUP = /(?:\[\[s\d{1,3}\]\])+/g;

/**
 * Turn the markers in the sanitised HTML into numbered links. Only text nodes are touched, and
 * never one inside code, so a marker written as code stays as written. The markers are built here
 * as elements after the sanitiser has run, so nothing in the bot's text can pose as one.
 */
function placeMarkers(html: string, sources: readonly CiteSource[], cites: Record<string, number>): string {
  if (!html.includes('[[s')) return html;
  const root = document.createElement('div');
  root.innerHTML = html;
  const position = new Map(sources.map((s, i) => [s.id, i] as const));
  const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
  const nodes: Text[] = [];
  for (let n = walker.nextNode(); n; n = walker.nextNode()) {
    const t = n as Text;
    if (t.data.includes('[[s') && !t.parentElement?.closest('code, pre')) nodes.push(t);
  }
  // A marker inside link text would be a link in a link, so it is placed just after the link.
  const lastAfter = new Map<Element, Node>();

  for (const node of nodes) {
    const text = node.data;
    const anchor = node.parentElement?.closest('a') ?? null;
    const out = document.createDocumentFragment();
    let at = 0;
    for (const run of text.matchAll(GROUP)) {
      out.append(text.slice(at, run.index));
      at = run.index + run[0].length;
      const ids = [...new Set([...run[0].matchAll(MARKER)].map((m) => `s${m[1]}`))].filter((id) => position.has(id));
      if (ids.length === 0) continue;
      ids.sort((a, b) => position.get(a)! - position.get(b)!);
      const group = document.createElement('sup');
      group.className = 'cite-group';
      for (const id of ids) {
        const source = sources[position.get(id)!]!;
        const n = position.get(id)! + 1;
        const b = document.createElement('button');
        b.type = 'button';
        b.className = 'cite';
        b.dataset.cite = id;
        b.dataset.title = source.title;
        b.setAttribute('aria-label', copy.draft.citeLabel(n, source.title));
        b.textContent = String(n);
        group.append(b);
        cites[id] = (cites[id] ?? 0) + 1;
      }
      if (anchor) {
        const prev = lastAfter.get(anchor) ?? anchor;
        prev.parentNode?.insertBefore(group, prev.nextSibling);
        lastAfter.set(anchor, group);
      } else {
        out.append(group);
      }
    }
    out.append(text.slice(at));
    node.replaceWith(out);
  }
  return root.innerHTML;
}

/** Render markdown to sanitised HTML with the allowed formatting only. */
export function renderMarkdown(text: string, options: RenderOptions = {}): Rendered {
  const stripped: string[] = [];
  const links: LinkRef[] = [];
  const cites: Record<string, number> = {};

  const source = text.replace(INVISIBLE, (run) => {
    stripped.push(
      'Hidden characters: ' +
        [...run].map((c) => `U+${c.codePointAt(0)!.toString(16).toUpperCase().padStart(4, '0')}`).join(' '),
    );
    return '';
  });

  const marked = new Marked({
    gfm: true,
    breaks: false,
    renderer: {
      html(token: Tokens.HTML | Tokens.Tag) {
        const raw = token.text.trim();
        // A lone closing tag has no content of its own, so it is not worth a warning.
        if (raw && !/^<\/[a-z][a-z0-9-]*\s*>$/i.test(raw)) stripped.push(raw);
        return '';
      },
      image(token: Tokens.Image) {
        return esc(`[image: ${token.text || 'no description'}] (${token.href})`);
      },
      checkbox(token: { checked: boolean }) {
        return token.checked ? '[x] ' : '[ ] ';
      },
      link(this: Renderer, token: Tokens.Link) {
        const inner = this.parser.parseInline(token.tokens);
        let url: URL;
        try {
          url = new URL(token.href);
        } catch {
          return `${inner} (${esc(token.href)})`;
        }
        if (url.protocol !== 'https:' && url.protocol !== 'http:') {
          return `${inner} (${esc(token.href)})`;
        }
        const label = stripFromLine(token.text);
        links.push({ text: label, href: url.href, host: url.host });
        const same = bare(label) === bare(url.href) || bare(label) === bare(token.href);
        return (
          `<a href="${esc(url.href)}" target="_blank" rel="noopener noreferrer">${inner}</a>` +
          (same ? '' : ` <small>(${esc(url.host)})</small>`)
        );
      },
    },
  });

  const html = marked.parse(source, { async: false });
  const clean = DOMPurify.sanitize(html, {
    ALLOWED_TAGS,
    ALLOWED_ATTR: ['href'],
    ALLOW_DATA_ATTR: false,
    ALLOW_ARIA_ATTR: false,
    ALLOWED_URI_REGEXP: /^https?:/i,
    FORBID_ATTR: ['style', 'class', 'id', 'name'],
    FORBID_TAGS: [
      'style', 'form', 'input', 'button', 'textarea', 'select', 'img', 'video', 'audio', 'svg',
      'math', 'details', 'iframe', 'object', 'embed', 'link', 'meta',
    ],
    SANITIZE_NAMED_PROPS: true,
  });
  return { html: placeMarkers(clean, options.sources ?? [], cites), stripped, links, cites };
}
