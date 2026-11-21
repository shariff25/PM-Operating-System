// Sample data for the Admin area: an audit log, a team, connectors and the overview counts.
// Fake names and ids only. Used by the sample-data client (tests and the dev server).
import { MOCK_CATALOG } from './mockBots';
import { createMockUsers } from './mockUsers';
import type { MockUsers, MockUsersOptions } from './mockUsers';
import { ApiError } from './types';
import type { AdminBot, AdminBots, AdminHeldDraft, AdminOverview, AuditEvent, AuditExport, AuditPage, AuditQuery, Connector, TeamList, TeamMember } from './types';

export type MockRole = 'pm' | 'admin' | 'auditor' | 'reviewer';

export const MOCK_PROFILES: Record<MockRole, { handle: string; name: string; roles: string[] }> = {
  pm: { handle: 'ana', name: 'Ana Silva', roles: ['pm'] },
  admin: { handle: 'ada', name: 'Ada Okafor', roles: ['admin'] },
  auditor: { handle: 'audrey', name: 'Audrey Nolan', roles: ['auditor'] },
  // Reviews Ana's and Dana's drafts, as in the sample user database.
  reviewer: { handle: 'ben', name: 'Ben Carter', roles: ['pm', 'reviewer'] },
};

const PM_PERMISSIONS = [
  'ask-bot',
  'view-own-tasks',
  'manage-own-routines',
  'decide-own-approval',
  'view-own-traces',
  'link-own-accounts',
];

const ADMIN_PERMISSIONS = [
  'ask-bot',
  'view-own-tasks',
  'view-others-tasks',
  'manage-own-routines',
  'decide-own-approval',
  'view-others-approvals',
  'recommend-on-approval',
  'view-own-traces',
  'view-traces',
  'view-audit-log',
  'export-audit',
  'edit-bots-and-skills',
  'view-policies',
  'edit-policies',
  'view-content',
  'edit-content',
  'approve-policy-change',
  'link-own-accounts',
  'manage-connectors',
  'manage-members',
  'trigger-reindex',
];

export const MOCK_PERMISSIONS: Record<MockRole, string[]> = {
  pm: PM_PERMISSIONS,
  admin: ADMIN_PERMISSIONS,
  auditor: ['view-audit-log', 'export-audit', 'view-policies', 'view-content'],
  // The Reviewer row: a PM's, plus reading and recommending on others' drafts.
  reviewer: [
    'ask-bot',
    'view-own-tasks',
    'manage-own-routines',
    'decide-own-approval',
    'view-others-approvals',
    'recommend-on-approval',
    'view-own-traces',
    'link-own-accounts',
  ],
};

/** The role a development handle signs in as. Any other handle is a plain PM. */
export function roleOfHandle(handle: string): MockRole | null {
  const h = handle.trim().toLowerCase();
  if (h === 'ada' || h === 'admin') return 'admin';
  if (h === 'audrey' || h === 'auditor') return 'auditor';
  if (h === 'ben' || h === 'reviewer') return 'reviewer';
  return null;
}

export interface MockAdminOptions extends MockUsersOptions {
  /** The tool server does not answer: no linked accounts, no counts, a connector down. */
  toolServerDown?: boolean;
  /** The most rows one export holds. The real server's cap is 10,000. */
  exportCap?: number;
  /** How many sample events there are. */
  eventCount?: number;
}

const MIN = 60_000;

interface Template {
  actor: string;
  client: string;
  action: string;
  target: string | null;
  outcome: string;
  detail: string | null;
  sources: number;
  titled: number;
  skill: string | null;
  bot: string | null;
  /** The marketplace bot the event ran as; absent is Nudge or no bot. */
  persona?: string;
}

const T: Template[] = [
  { actor: 'ana', client: 'mcp', action: 'jira.search', target: null, outcome: 'ok', detail: null, sources: 8, titled: 8, skill: 'brief', bot: 'pm-ana' },
  { actor: 'ana', client: 'mcp', action: 'confluence.create_draft', target: 'apr_prd', outcome: 'pending', detail: null, sources: 5, titled: 4, skill: 'prd', bot: 'pm-ana', persona: 'chief' },
  { actor: 'ana', client: 'web', action: 'jira.create_issue', target: 'https://acme.atlassian.net/browse/PAY-301', outcome: 'approved', detail: 'edited before approval', sources: 3, titled: 2, skill: null, bot: 'pm-ana', persona: 'chief' },
  { actor: 'ben', client: 'bot', action: 'slack.post_reply', target: null, outcome: 'denied', detail: null, sources: 0, titled: 0, skill: 'reply', bot: null },
  { actor: 'ben', client: 'mcp', action: 'slack.read_thread', target: null, outcome: 'ok', detail: null, sources: 14, titled: 0, skill: 'reply', bot: 'pm-ben' },
  { actor: 'dana', client: 'web', action: 'confluence.create_page', target: 'https://acme.atlassian.net/wiki/spaces/PM/pages/48211', outcome: 'approved', detail: null, sources: 6, titled: 6, skill: null, bot: 'pm-dana' },
  { actor: 'dana', client: 'web', action: 'jira.create_issue', target: 'apr_8d21', outcome: 'rejected', detail: 'reason: wrong_target', sources: 2, titled: 2, skill: null, bot: 'pm-dana' },
  { actor: 'ana', client: 'orchestrator', action: 'memory.write', target: 'memory:billing-notes', outcome: 'ok', detail: null, sources: 0, titled: 0, skill: null, bot: 'pm-ana' },
  { actor: 'ada', client: 'web', action: 'admin.audit', target: null, outcome: 'ok', detail: null, sources: 0, titled: 0, skill: null, bot: null },
  { actor: 'ravi', client: 'bot', action: 'slack.post_message', target: 'apr_c04f', outcome: 'expired', detail: 'undecided after 24 h: text removed', sources: 3, titled: 1, skill: 'announce', bot: 'pm-ravi' },
  { actor: 'ravi', client: 'mcp', action: 'context.read', target: null, outcome: 'ok', detail: null, sources: 4, titled: 4, skill: 'prd', bot: 'pm-ravi' },
  { actor: 'ana', client: 'web', action: 'credentials.unlink', target: 'github', outcome: 'ok', detail: null, sources: 0, titled: 0, skill: null, bot: null },
  { actor: 'priya', client: 'mcp', action: 'jira.update_issue', target: 'apr_77be', outcome: 'failed', detail: null, sources: 1, titled: 1, skill: 'triage', bot: 'pm-priya' },
  { actor: 'priya', client: 'web', action: 'slack.post_reply', target: 'apr_51aa', outcome: 'granted', detail: 'approved in the inbox; writing', sources: 2, titled: 0, skill: 'reply', bot: 'pm-priya' },
  { actor: 'audrey', client: 'web', action: 'admin.audit_export', target: null, outcome: 'ok', detail: null, sources: 0, titled: 0, skill: null, bot: null },
  { actor: 'dana', client: 'mcp', action: 'docs.search', target: null, outcome: 'error', detail: null, sources: 0, titled: 0, skill: 'brief', bot: 'pm-dana', persona: 'echo' },
  { actor: 'ben', client: 'web', action: 'admin.overview', target: null, outcome: 'denied', detail: null, sources: 0, titled: 0, skill: null, bot: null },
  { actor: 'ana', client: 'web', action: 'slack.post_reply', target: 'apr_e19c', outcome: 'started', detail: 'approved in the inbox; writing', sources: 1, titled: 0, skill: 'reply', bot: 'pm-ana' },
];

function buildEvents(now: number, count: number): AuditEvent[] {
  const out: AuditEvent[] = [];
  let t = now - 4 * MIN;
  for (let i = 0; i < count; i++) {
    const tpl = T[(i * 7 + (i >> 2)) % T.length]!;
    const ms = (i * 37) % 997;
    out.push({
      id: `ev_${9000 - i}`,
      at: new Date(t - ms).toISOString(),
      actor: tpl.actor,
      client: tpl.client,
      action: tpl.action,
      system: tpl.action.split('.')[0]!,
      target: tpl.target,
      outcome: tpl.outcome,
      detail: tpl.detail,
      sources: tpl.sources,
      titled_sources: tpl.titled,
      skill: tpl.skill,
      bot: tpl.bot,
      persona: tpl.persona ?? null,
      task_id: tpl.action.startsWith('admin.') ? 'admin' : tpl.client === 'mcp' ? `run_${(7000 + i * 13).toString(16)}` : `tsk_01JD${(4100 + i * 29).toString(36).toUpperCase()}`,
    });
    // Gaps of 3 to 95 minutes, so a week holds a hundred or so events.
    t -= (3 + ((i * 53) % 93)) * MIN;
  }
  return out;
}

const MEMBERS: TeamMember[] = [
  { handle: 'ana', name: 'Ana Silva', roles: ['pm'], has_email: true, bot: 'pm-ana', accounts: { atlassian: true, slack: true, github: false } },
  { handle: 'ben', name: 'ben', roles: ['pm', 'reviewer'], has_email: true, bot: 'pm-ben', accounts: { atlassian: true, slack: true, github: true } },
  { handle: 'dana', name: 'dana', roles: ['pm'], has_email: true, bot: 'pm-dana', accounts: { atlassian: true, slack: false, github: false } },
  { handle: 'priya', name: 'priya', roles: ['reviewer'], has_email: true, bot: 'pm-priya', accounts: { atlassian: false, slack: true, github: false } },
  { handle: 'ravi', name: 'ravi', roles: ['pm', 'skill-author'], has_email: false, bot: 'pm-ravi', accounts: { atlassian: false, slack: false, github: false } },
  { handle: 'ada', name: 'ada', roles: ['admin'], has_email: true, bot: 'pm-ada', accounts: { atlassian: true, slack: true, github: true } },
  { handle: 'audrey', name: 'audrey', roles: ['auditor'], has_email: false, bot: 'pm-audrey', accounts: { atlassian: false, slack: false, github: false } },
];

const CSV_COLUMNS = ['at', 'actor', 'client', 'action', 'system', 'target', 'outcome', 'detail', 'sources', 'titled_sources', 'skill', 'bot', 'task_id', 'id', 'persona'] as const;

/** Every cell quoted, quotes doubled, and a leading ' on a cell a spreadsheet would run as a formula. */
export function csvCell(v: string | number | null): string {
  let s = v === null ? '' : String(v);
  if (/^[=+\-@\t\r]/.test(s)) s = `'${s}`;
  return `"${s.replace(/"/g, '""')}"`;
}

export function toCsv(events: AuditEvent[]): string {
  const lines = [CSV_COLUMNS.map((c) => csvCell(c)).join(',')];
  for (const e of events) lines.push(CSV_COLUMNS.map((c) => csvCell(e[c] ?? null)).join(','));
  return lines.join('\r\n') + '\r\n';
}

const PAGE = 50;

const cursorOf = (index: number) => `c1.${btoa(String(index)).replace(/=+$/, '')}`;
function indexOfCursor(cursor: string): number {
  const m = /^c1\.([A-Za-z0-9+/]+)$/.exec(cursor);
  const n = m ? Number(atob(m[1]!)) : NaN;
  if (!Number.isInteger(n) || n < 0) throw new ApiError(400, 'bad_request', 'That cursor is not one I know.');
  return n;
}

/** Who has each bot in the sample team, and what else the admin view shows of it. */
const BOT_USAGE: Record<string, { handles: string[]; drafts: number; clashes?: string[]; lint?: AdminBot['lint'] }> = {
  chief: { handles: ['ana', 'ben', 'dana', 'ravi'], drafts: 23 },
  sprout: { handles: ['ana', 'priya'], drafts: 6 },
  echo: { handles: ['dana'], drafts: 3, clashes: ['echo'], lint: { status: 'warning', errors: 0, warnings: 1 } },
  scout: { handles: ['ana', 'ravi'], drafts: 2 },
};

export interface MockAdmin {
  overview(): AdminOverview;
  bots(canEdit: boolean): AdminBots;
  /** Release a held draft: false when there is none by that id. */
  release(id: string): boolean;
  audit(q: AuditQuery): AuditPage;
  auditExport(q: AuditQuery): AuditExport;
  team(): TeamList;
  connectors(): Connector[];
  users: MockUsers;
}

export function createMockAdmin(now: number, options: MockAdminOptions, pendingDrafts: () => number): MockAdmin {
  const events = buildEvents(now, options.eventCount ?? 128);
  const cap = options.exportCap ?? 10_000;
  const down = options.toolServerDown ?? false;
  const users = createMockUsers(now, options);

  const filtered = (q: AuditQuery) => {
    if (q.from && q.to && Date.parse(q.from) > Date.parse(q.to)) {
      throw new ApiError(400, 'bad_request', 'The start date is after the end date.');
    }
    return events.filter(
      (e) =>
        (!q.actor || e.actor === q.actor) &&
        (!q.action || e.action === q.action) &&
        (!q.outcome || e.outcome === q.outcome) &&
        (!q.persona || e.persona === q.persona) &&
        (!q.from || Date.parse(e.at) >= Date.parse(q.from)) &&
        (!q.to || Date.parse(e.at) < Date.parse(q.to)),
    );
  };

  // Drafts on hold: the withdrawn Scout's pending drafts, and one of a bot deleted from the catalog.
  const held: Record<string, AdminHeldDraft[]> = {
    scout: [
      { id: 'apr_held', user: 'ana', kind: 'confluence_page', tool: 'confluence.create_draft', created_at: new Date(now - 5 * 60 * MIN).toISOString() },
      { id: 'apr_held_ravi', user: 'ravi', kind: 'jira_issue', tool: 'jira.create_issue', created_at: new Date(now - 30 * 60 * MIN).toISOString() },
    ],
    atlas: [{ id: 'apr_held_atlas', user: 'ravi', kind: 'slack_reply', tool: 'slack.post_reply', created_at: new Date(now - 3 * 24 * 60 * MIN).toISOString() }],
  };

  return {
    users,
    bots(canEdit) {
      if (down) throw new ApiError(503, 'unavailable', 'The tool server is not answering.');
      const bots: AdminBot[] = MOCK_CATALOG.map((c) => {
        const u = BOT_USAGE[c.key];
        const state = !c.offered ? 'withdrawn' : c.preview ? 'preview' : 'offered';
        return {
          key: c.key,
          display_name: c.display_name,
          mascot: c.mascot,
          state,
          offered: c.offered,
          always_on: c.always_on,
          preview: c.preview ?? false,
          users: c.always_on ? null : (u?.handles.length ?? 0),
          handles: c.always_on ? null : [...(u?.handles ?? [])],
          needs: c.needs.map(({ source, label, met }) => ({ source, label, met })),
          drafts_30d: c.always_on ? 41 : (u?.drafts ?? 0),
          name_clashes: [...(u?.clashes ?? [])],
          held: structuredClone(held[c.key] ?? []),
          path: `content/personas/${c.key}/PERSONA.md`,
          lint: u?.lint ?? { status: 'ok', errors: 0, warnings: 0 },
        };
      });
      const unknown = [{ key: 'atlas', users: 1, handles: ['ravi'], drafts_30d: 0, held: structuredClone(held.atlas ?? []) }];
      const total = Object.values(held).reduce((n, l) => n + l.length, 0);
      return { bots, unknown, held_total: total, can_release: canEdit, can_edit_content: canEdit };
    },
    release(id) {
      for (const list of Object.values(held)) {
        const i = list.findIndex((h) => h.id === id);
        if (i >= 0) {
          list.splice(i, 1);
          return true;
        }
      }
      return false;
    },
    overview() {
      const since = now - 7 * 24 * 60 * MIN;
      const count = (outcome: string) => events.filter((e) => e.outcome === outcome && Date.parse(e.at) >= since).length;
      return {
        since: new Date(since).toISOString(),
        pending_drafts: pendingDrafts() + 1,
        approved: down ? null : count('approved'),
        rejected: down ? null : count('rejected'),
        denied: down ? null : count('denied'),
        retention_days: 90,
        oldest_event_at: events.length ? events.reduce((m, e) => (e.at < m ? e.at : m), events[0]!.at) : null,
        last_purge: { at: new Date(now - 9 * 60 * MIN).toISOString(), deleted: 1200 },
        held_drafts: down ? null : 1,
        users: down ? null : users.overview(),
      };
    },
    audit(q) {
      const all = filtered(q);
      const limit = q.limit ?? PAGE;
      if (!Number.isInteger(limit) || limit < 1 || limit > 100) throw new ApiError(400, 'bad_request', 'The page size must be between 1 and 100.');
      const start = q.before ? indexOfCursor(q.before) : 0;
      const page = all.slice(start, start + limit);
      const next = start + limit < all.length ? cursorOf(start + limit) : null;
      return { events: structuredClone(page), next_cursor: next };
    },
    auditExport(q) {
      const all = filtered(q);
      const rows = all.slice(0, cap);
      return {
        blob: new Blob([toCsv(rows)], { type: 'text/csv;charset=utf-8' }),
        filename: 'pmos-audit.csv',
        truncated: all.length > cap,
        rowCount: rows.length,
      };
    },
    team() {
      return {
        members: structuredClone(MEMBERS).map((m) => (down ? { ...m, accounts: null } : m)),
        accounts_available: !down,
        linking: down ? null : true,
      };
    },
    connectors() {
      const checked = new Date(now - 20_000).toISOString();
      return [
        { name: 'confluence', mode: 'live', health: 'ok', error_class: null, checked_at: checked },
        down
          ? { name: 'docs', mode: 'live', health: 'down', error_class: 'timeout', checked_at: checked }
          : { name: 'docs', mode: 'live', health: 'ok', error_class: null, checked_at: checked },
        { name: 'jira', mode: 'live', health: 'ok', error_class: null, checked_at: checked },
        { name: 'slack', mode: 'live', health: 'ok', error_class: null, checked_at: checked },
      ];
    },
  };
}
