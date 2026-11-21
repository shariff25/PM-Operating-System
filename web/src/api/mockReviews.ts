// Sample data for Reviews: the drafts a reviewer (Ben) reads, and the advice on them. Fake names
// and text only. Used by the sample-data client (tests and the dev server).
//
// Ben reviews Ana and Dana, as in the sample user database. Ana's drafts are the sample client's
// own drafts, so advice given here shows on Ana's draft page, and an edit Ana saves marks it as
// given on an earlier version. Every state the screens have to handle is here:
// - apr_jira, apr_slack: Ana's, already advised by Ben (changes, approve);
// - apr_prd: Ana's, built from restricted sources, so metadata only;
// - apr_slack_old: Ana's, expired, with a note removed after 24 hours;
// - apr_dana_onboarding: Dana's, nothing from Ben yet;
// - apr_dana_exports: Dana's, which she edits the moment Ben first recommends (409 draft_changed);
// - apr_dana_launch: Dana's Slack message, restricted;
// - apr_ben_own: Ben's own draft (403 own_draft); any other id is not his to read (404).
// With `hostile`, a PM and a reviewer whose names and note try markup.
import { ApiError } from './types';
import type { Draft, MyRecommendation, Recommendation, RecommendInput, Recommended, ReviewDraft, ReviewList, ReviewSummary, Verdict } from './types';

const MIN = 60_000;
const HOUR = 60 * MIN;
const iso = (t: number) => new Date(t).toISOString();

/** A display name that tries everything: markup, an image that runs script, a Slack mention. */
export const HOSTILE_NAME = '<img src=x onerror="window.__pwned=1"> <!channel>';
export const HOSTILE_NOTE = '<img src=x onerror="window.__pwned=1"> <!channel> **not bold** [not a link](https://evil.example/login)';

interface StoredRec {
  reviewer: string;
  reviewer_name: string;
  verdict: Verdict;
  note: string | null;
  note_removed: boolean;
  created_at: string;
  revision: number;
  args_hash: string;
}

/** A draft as the sample data writes it: the rest is filled in by `own`. */
type DraftInit = Omit<Draft, 'edited_at' | 'audience' | 'marker'> & Partial<Pick<Draft, 'edited_at' | 'audience' | 'marker'>>;

interface Owned {
  owner: string;
  owner_name: string;
  draft: Draft;
}

export interface MockReviewsOptions {
  /** The dev server's `?mock_reviews=<code>`: every recommendation is answered with this code. `unknown_outcome` records it first. */
  reviewError?: string;
  hostile?: boolean;
}

export interface MockReviews {
  /** The advice on one of Ana's drafts, as her draft page reads it. */
  recommendationsFor(d: Draft): Recommendation[];
  list(): ReviewList;
  get(id: string): ReviewDraft;
  recommend(id: string, input: RecommendInput): Recommended;
}

/** A fake SHA-256-shaped hash of a draft's arguments: 64 lowercase hex, changing whenever they do. */
export function fakeArgsHash(d: Pick<Draft, 'title' | 'body' | 'target'>): string {
  const text = `${d.title}\n${d.body}\n${d.target.label}`;
  let out = '';
  for (let seed = 0; seed < 8; seed++) {
    let h = (0x811c9dc5 ^ (seed * 0x9e3779b1)) >>> 0;
    for (let i = 0; i < text.length; i++) {
      h ^= text.charCodeAt(i);
      h = Math.imul(h, 0x01000193) >>> 0;
    }
    out += h.toString(16).padStart(8, '0');
  }
  return out;
}

const revisionOf = (d: Draft) => Number(d.version ?? '1');

const STATUS: Record<string, number> = {
  bad_request: 400,
  forbidden: 403,
  own_draft: 403,
  restricted_draft: 403,
  bad_csrf: 403,
  not_found: 404,
  not_pending: 409,
  expired: 409,
  draft_changed: 409,
  invalid: 422,
  upstream_unavailable: 502,
  unknown_outcome: 502,
  assertion_required: 503,
  unavailable: 503,
  users_unavailable: 503,
};

/** The note as the server keeps it: control characters out (a tab becomes a space, line breaks stay), trimmed. */
function cleanNote(raw: string | undefined): string | null {
  if (raw === undefined) return null;
  const out = raw
    .replace(/\r\n?/g, '\n')
    .replace(/\t/g, ' ')
    // eslint-disable-next-line no-control-regex
    .replace(/[\u0000-\u0009\u000b-\u001f\u007f-\u009f‪-‮⁦-⁩]/g, '')
    .trim();
  return out === '' ? null : out;
}

export function createMockReviews(now0: number, anaDrafts: Map<string, Draft>, options: MockReviewsOptions = {}): MockReviews {
  const others = new Map<string, Owned>();
  const own = (owner: string, owner_name: string, init: DraftInit) => {
    const draft = init as Draft;
    draft.version ??= '1';
    draft.edited_at ??= null;
    draft.audience ??= draft.kind.startsWith('slack') ? { members: null, external: null } : null;
    draft.marker ??= null;
    others.set(draft.id, { owner, owner_name, draft });
  };

  own('dana', 'dana', {
    id: 'apr_dana_onboarding',
    kind: 'confluence_page',
    title: 'Onboarding checklist for new workspaces',
    bot: 'pm-dana',
    status: 'pending',
    created_at: iso(now0 - 40 * MIN),
    expires_at: null,
    task_id: null,
    practice: false,
    body: `## Why
New workspace admins miss two setup steps in their first week, and half of them open a ticket about it[[s1]].

## The checklist
- Invite at least one other admin
- Connect billing before the trial ends[[s2]]
- Pick a grace period (see the TCB spec)

## Out of scope
Single sign-on setup stays with the IT guide.
`,
    edited: false,
    target: { system: 'confluence', label: 'Space PM, under “Onboarding”', url: null },
    sources: [
      { id: 's1', system: 'jira', title: 'SUP-88: Admins stuck after signup', url: 'https://example.com/jira/SUP-88', excerpt: null, cited: true },
      { id: 's2', system: 'docs', title: 'Trial and billing rules', url: 'https://example.com/docs/trial', excerpt: null, cited: true },
      { id: 's3', system: 'slack', title: 'Slack thread', url: 'https://example.com/slack/onboarding/p4', excerpt: null, cited: false },
    ],
    result_url: null,
    external: false,
    restricted_sources: false,
  });
  own('dana', 'dana', {
    id: 'apr_dana_exports',
    persona: 'sprout',
    kind: 'jira_issue',
    title: 'Retry failed exports nightly',
    bot: 'pm-dana',
    status: 'pending',
    created_at: iso(now0 - 5 * HOUR),
    expires_at: null,
    task_id: null,
    practice: false,
    body: `**Summary:** Retry failed invoice exports every night

**Description**
Exports that fail because the warehouse is busy are never tried again[[s1]]. Retry them once a night, at most three times.

**Acceptance criteria**
- A failed export is retried the next night
- After three failures the admin gets an email
`,
    edited: false,
    target: { system: 'jira', label: 'Project PAY, a new Story', url: null },
    sources: [{ id: 's1', system: 'jira', title: 'PAY-230: Export failures', url: 'https://example.com/jira/PAY-230', excerpt: null, cited: true }],
    result_url: null,
    external: false,
    restricted_sources: false,
  });
  own('dana', 'dana', {
    id: 'apr_dana_launch',
    kind: 'slack_message',
    title: 'Message to a Slack channel',
    bot: 'pm-dana',
    status: 'pending',
    created_at: iso(now0 - 25 * MIN),
    expires_at: iso(now0 + 23 * HOUR),
    task_id: null,
    practice: false,
    body: 'Numbers from the finance review, for the launch channel.',
    edited: false,
    target: { system: 'slack', label: 'A Slack channel', url: null },
    sources: [],
    result_url: null,
    external: false,
    restricted_sources: true,
  });
  if (options.hostile) {
    own('zed', HOSTILE_NAME, {
      id: 'apr_zed',
      kind: 'jira_issue',
      title: 'Tidy the release checklist',
      bot: 'pm-zed',
      status: 'pending',
      created_at: iso(now0 - 3 * HOUR - 10 * MIN),
      expires_at: null,
      task_id: null,
      practice: false,
      body: 'Move the release checklist into the PM space and drop the steps nobody runs.',
      edited: false,
      target: { system: 'jira', label: 'Project OPS, a new Task', url: null },
      sources: [],
      result_url: null,
      external: false,
      restricted_sources: false,
    });
  }
  /** Dana saves an edit the first time Ben recommends on this one, so his advice lands on a version that is gone. */
  let exportsMoved = false;

  const recs = new Map<string, StoredRec[]>();
  const seed = (id: string, rec: Omit<StoredRec, 'revision' | 'args_hash'>) => {
    const d = anaDrafts.get(id) ?? others.get(id)?.draft;
    if (!d) return;
    recs.set(id, [...(recs.get(id) ?? []), { ...rec, revision: revisionOf(d), args_hash: fakeArgsHash(d) }]);
  };
  seed('apr_jira', {
    reviewer: 'ben',
    reviewer_name: 'Ben Carter',
    verdict: 'changes',
    note: "Say which time zone the end date uses: the admin's or the workspace's.\nOtherwise this reads well.",
    note_removed: false,
    created_at: iso(now0 - 2 * HOUR),
  });
  seed('apr_slack', {
    reviewer: 'ben',
    reviewer_name: 'Ben Carter',
    verdict: 'approve',
    note: 'Thursday matches the launch plan. Good to go.',
    note_removed: false,
    created_at: iso(now0 - 40 * MIN),
  });
  seed('apr_slack_old', {
    reviewer: 'ben',
    reviewer_name: 'Ben Carter',
    verdict: 'approve',
    note: null,
    note_removed: true,
    created_at: iso(now0 - 29 * HOUR),
  });
  if (options.hostile) {
    seed('apr_hostile', {
      reviewer: 'zed',
      reviewer_name: HOSTILE_NAME,
      verdict: 'changes',
      note: HOSTILE_NOTE,
      note_removed: false,
      created_at: iso(now0 - 30 * MIN),
    });
  }

  const PMS = ['ana', 'dana', ...(options.hostile ? ['zed'] : [])];

  const isStale = (r: StoredRec, d: Draft) => r.revision !== revisionOf(d) || r.args_hash !== fakeArgsHash(d);

  /** Every draft Ben may read, with its owner. Ana's practice drafts are hers alone. */
  const all = (): Owned[] => [
    ...[...anaDrafts.values()].filter((d) => !d.practice).map((draft) => ({ owner: 'ana', owner_name: 'Ana Silva', draft })),
    ...others.values(),
  ];

  const expiredNow = (d: Draft) => d.status === 'expired' || (d.status === 'pending' && d.expires_at !== null && Date.parse(d.expires_at) <= Date.now());

  const mine = (d: Draft): MyRecommendation | null => {
    const r = recs.get(d.id)?.find((x) => x.reviewer === 'ben');
    if (!r) return null;
    return { verdict: r.verdict, note: r.note, note_removed: r.note_removed, revision: r.revision, created_at: r.created_at, stale: isStale(r, d) };
  };

  const summary = (o: Owned): ReviewSummary => {
    const d = o.draft;
    const base = {
      id: d.id,
      owner: o.owner,
      owner_name: o.owner_name,
      kind: d.kind,
      status: d.status,
      created_at: d.created_at,
      expires_at: d.expires_at,
      revision: revisionOf(d),
      // Which bot drafted it: metadata, so it is kept for a restricted draft too.
      persona: d.persona ?? null,
      persona_offered: d.persona_offered ?? true,
    };
    if (d.restricted_sources) return { ...base, restricted: true, title: null, my_recommendation: null };
    return { ...base, restricted: false, title: d.title, edited: d.edited, args_hash: fakeArgsHash(d), my_recommendation: mine(d) };
  };

  const read = (o: Owned): ReviewDraft => {
    const d = o.draft;
    if (d.restricted_sources) {
      // Exactly the metadata: nothing about the text.
      const { id, owner, owner_name, kind, status, created_at, expires_at, revision, persona, persona_offered } = summary(o);
      return structuredClone({ id, owner, owner_name, kind, status, created_at, expires_at, restricted: true, revision, persona, persona_offered });
    }
    // A Slack target names a public channel only; the sample channels here have no name to hide.
    return structuredClone({ ...summary(o), body: d.body, target: d.target, sources: d.sources });
  };

  const find = (id: string): Owned => {
    if (id === 'apr_ben_own') throw new ApiError(403, 'own_draft', 'This is your own draft: you decide it under Drafts.');
    const o = all().find((x) => x.draft.id === id);
    if (!o) throw new ApiError(404, 'not_found', 'I could not find that draft.');
    if (expiredNow(o.draft)) throw new ApiError(409, 'expired', 'This Slack draft passed its 24 hours.');
    if (o.draft.status !== 'pending') throw new ApiError(409, 'not_pending', 'This draft was already decided.');
    return o;
  };

  return {
    recommendationsFor(d) {
      if (d.practice) return [];
      return (recs.get(d.id) ?? []).map((r) => ({
        reviewer: r.reviewer,
        reviewer_name: r.reviewer_name,
        verdict: r.verdict,
        note: r.note,
        note_removed: r.note_removed,
        created_at: r.created_at,
        stale: isStale(r, d),
      }));
    },
    list() {
      const pending = all()
        .filter((o) => o.draft.status === 'pending' && !expiredNow(o.draft))
        .sort((a, b) => b.draft.created_at.localeCompare(a.draft.created_at));
      return structuredClone({ drafts: pending.map(summary), total: pending.length, truncated: false, pms: PMS });
    },
    get(id) {
      return read(find(id));
    },
    recommend(id, input) {
      if (input.verdict !== 'approve' && input.verdict !== 'changes') throw new ApiError(400, 'bad_request', 'That is not a verdict I know.');
      if (!Number.isInteger(input.revision) || input.revision < 1 || !/^[0-9a-f]{64}$/.test(input.args_hash)) {
        throw new ApiError(400, 'bad_request', 'A recommendation names the version it was read at.');
      }
      if (input.note !== undefined && [...input.note].length > 1000) throw new ApiError(422, 'invalid', 'A note is at most 1000 characters.');
      const code = options.reviewError;
      if (code && code !== 'unknown_outcome') throw new ApiError(STATUS[code] ?? 400, code, 'Refused by the sample data.');
      const o = find(id);
      const d = o.draft;
      if (d.restricted_sources) throw new ApiError(403, 'restricted_draft', 'This draft uses restricted sources.');
      if (d.id === 'apr_dana_exports' && !exportsMoved) {
        // Dana saves an edit while Ben reads: his version is gone.
        exportsMoved = true;
        d.body = d.body.replace('at most three times', 'at most twice, then stop').replace('After three failures', 'After two failures');
        d.edited = true;
        d.sources = d.sources.map((s) => ({ ...s, cited: false }));
        d.body = d.body.replace(/\[\[s\d+\]\]/g, '');
        d.version = String(revisionOf(d) + 1);
      }
      if (input.revision !== revisionOf(d) || input.args_hash !== fakeArgsHash(d)) {
        throw new ApiError(409, 'draft_changed', 'This draft changed since you read it.');
      }
      const rec: StoredRec = {
        reviewer: 'ben',
        reviewer_name: 'Ben Carter',
        verdict: input.verdict,
        note: cleanNote(input.note),
        note_removed: false,
        created_at: iso(Date.now()),
        revision: revisionOf(d),
        args_hash: fakeArgsHash(d),
      };
      // One live recommendation per reviewer: a new one replaces the old, note and all.
      recs.set(d.id, [...(recs.get(d.id) ?? []).filter((r) => r.reviewer !== 'ben'), rec]);
      if (code === 'unknown_outcome') throw new ApiError(502, 'unknown_outcome', 'The answer was lost.');
      return { recommendation: mine(d)!, draft: read(o) };
    },
  };
}
