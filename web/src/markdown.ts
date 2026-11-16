import DOMPurify from 'dompurify';
import { Marked } from 'marked';
import type { Renderer, Tokens } from 'marked';

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

/** Render markdown to sanitised HTML with the allowed formatting only. */
export function renderMarkdown(text: string): Rendered {
  const stripped: string[] = [];
  const links: LinkRef[] = [];

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
        links.push({ text: token.text, href: url.href, host: url.host });
        const same = bare(token.text) === bare(url.href) || bare(token.text) === bare(token.href);
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
  return { html: clean, stripped, links };
}
