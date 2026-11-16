import DOMPurify from 'dompurify';
import { marked } from 'marked';

DOMPurify.addHook('afterSanitizeAttributes', (node) => {
  if (node.tagName === 'A') {
    node.setAttribute('target', '_blank');
    node.setAttribute('rel', 'noopener noreferrer');
  }
});

/** Render markdown to sanitised HTML. */
export function renderMarkdown(text: string): string {
  const html = marked.parse(text, { async: false, gfm: true, breaks: false });
  return DOMPurify.sanitize(html);
}
