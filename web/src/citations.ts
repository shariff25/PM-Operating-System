import type { Api, Draft, DraftSummary, ReviewSummary, Suggestion } from './api/types';

// Source markers. The server puts `[[s3]]` after the text a source supports (docs/web-api.md,
// "Citations"). The app turns each into a small numbered link in the formatted draft, and
// removes them everywhere text is shown plain.

/** A marker: `[[s` + 1 to 3 digits + `]]`. The digits are the source's id without the `s`. */
export const MARKER = /\[\[s(\d{1,3})\]\]/g;

const FENCE_OPEN = /^[ \t]{0,3}(`{3,}|~{3,})/;
// An inline code span: a run of backticks, the code, the same run again.
const INLINE_CODE = /(`+)(?!`)[\s\S]*?(?<!`)\1(?!`)/g;

/** Remove the markers from a stretch of text that holds no fenced code. Inline code is left alone. */
function stripOutsideInlineCode(text: string): string {
  let out = '';
  let at = 0;
  for (const m of text.matchAll(INLINE_CODE)) {
    out += text.slice(at, m.index).replace(MARKER, '');
    out += m[0];
    at = m.index + m[0].length;
  }
  return out + text.slice(at).replace(MARKER, '');
}

/**
 * The text with its source markers removed, for every place text is shown plain: the edit box,
 * a list preview, a thread answer. A marker inside inline code or a fenced block is code, not a
 * source, and stays.
 */
export function stripMarkers(text: string): string {
  if (!text.includes('[[s')) return text;
  const out: string[] = [];
  let pending: string[] = [];
  let fence: RegExp | null = null;
  const flush = () => {
    if (pending.length > 0) out.push(stripOutsideInlineCode(pending.join('\n')));
    pending = [];
  };
  for (const line of text.split('\n')) {
    if (fence) {
      out.push(line);
      if (fence.test(line)) fence = null;
      continue;
    }
    const open = FENCE_OPEN.exec(line);
    if (open) {
      flush();
      out.push(line);
      const f = open[1]!;
      fence = new RegExp(`^[ \\t]{0,3}\\${f[0]}{${f.length},}[ \\t]*$`);
      continue;
    }
    pending.push(line);
  }
  flush();
  return out.join('\n');
}

/** A title or one line of plain text: the markers go, and the space they leave behind is closed up. */
export function stripFromLine(text: string): string {
  if (!text.includes('[[s')) return text;
  return text.replace(MARKER, '').replace(/ {2,}/g, ' ').trim();
}

const cleanSummary = <T extends DraftSummary>(d: T): T => ({ ...d, title: stripFromLine(d.title) });
const cleanReview = <T extends ReviewSummary>(r: T): T => (typeof r.title === 'string' ? { ...r, title: stripFromLine(r.title) } : r);
const cleanSuggestion = (s: Suggestion | null): Suggestion | null =>
  s && s.body !== null ? { ...s, body: stripMarkers(s.body) } : s;

/**
 * Titles are never cited, and a suggestion has no markers either: the server removes them, and
 * this makes sure that a marker which slips through is still never shown. Only a draft's `body`
 * keeps its markers, because the document is the one place they mean something. The client is
 * changed in place, as `withCleanTasks` does.
 */
export function withCleanDrafts(api: Api): Api {
  const { listDrafts, getDraft, saveDraft, acceptSuggestion, rejectSuggestion, approve, reject, createPractice } = api;
  const { requestChange, getSuggestion, listReviews, getReview, recommend } = api;
  const one = (d: Draft): Draft => cleanSummary(d);
  api.listDrafts = async () => {
    const l = await listDrafts.call(api);
    return { ...l, drafts: l.drafts.map(cleanSummary) };
  };
  api.getDraft = async (id) => one(await getDraft.call(api, id));
  api.saveDraft = async (id, input) => one(await saveDraft.call(api, id, input));
  api.acceptSuggestion = async (id) => one(await acceptSuggestion.call(api, id));
  api.rejectSuggestion = async (id) => one(await rejectSuggestion.call(api, id));
  api.approve = async (id, o) => one(await approve.call(api, id, o));
  api.reject = async (id, r) => one(await reject.call(api, id, r));
  api.createPractice = async () => one(await createPractice.call(api));
  api.requestChange = async (id, text) => cleanSuggestion(await requestChange.call(api, id, text))!;
  api.getSuggestion = async (id) => cleanSuggestion(await getSuggestion.call(api, id));
  // A reviewer reads the same drafts: their titles are never cited either.
  api.listReviews = async () => {
    const l = await listReviews.call(api);
    return { ...l, drafts: l.drafts.map(cleanReview) };
  };
  api.getReview = async (id) => cleanReview(await getReview.call(api, id));
  api.recommend = async (id, input) => {
    const r = await recommend.call(api, id, input);
    return { ...r, draft: cleanReview(r.draft) };
  };
  return api;
}
