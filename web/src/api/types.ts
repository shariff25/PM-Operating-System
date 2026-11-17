// Types match docs/web-api.md exactly.

export type HintName = 'sources' | 'ask' | 'approve';

export interface Me {
  handle: string;
  name: string;
  roles: string[];
  csrf_token: string;
  first_visit: boolean;
  hints_seen: HintName[];
  nudges: boolean;
  /** What the person's roles allow, kebab-case. An older server leaves it out: treat that as none. */
  permissions?: string[];
}

export type DraftKind = 'confluence_page' | 'jira_issue' | 'slack_reply' | 'slack_message';
export type DraftStatus = 'pending' | 'approved' | 'rejected' | 'expired' | 'failed';

export interface DraftSummary {
  id: string;
  kind: DraftKind;
  title: string;
  bot: string;
  status: DraftStatus;
  created_at: string;
  expires_at: string | null;
  task_id: string | null;
  practice: boolean;
}

export interface Source {
  id: string;
  system: string;
  title: string;
  url: string | null;
  excerpt: string | null;
  /** True when the draft's text has a marker for this source: the draft relies on it. */
  cited: boolean;
}

export interface DraftTarget {
  system: string;
  label: string;
  url: string | null;
}

export interface DraftAudience {
  members: number | null;
  external: boolean | null;
}

export interface Draft extends DraftSummary {
  body: string;
  edited: boolean;
  target: DraftTarget;
  sources: Source[];
  result_url: string | null;
  external: boolean;
  /** When the PM last saved an edit here, else null. */
  edited_at: string | null;
  /** The run that drafted it read restricted sources (or predates that record, which counts the same). */
  restricted_sources: boolean;
  /**
   * For a Slack draft, who can see the channel. Null for Confluence and Jira. `members` and
   * `external` are null until PM OS has checked the channel, which it does when an approve is tried.
   */
  audience: DraftAudience | null;
  /** The line PM OS adds to what it writes so readers can tell. Null for a practice draft. */
  marker: string | null;
  /** Opaque. Sent back with approve so the server can refuse a draft that changed after it was shown. */
  version?: string;
}

export type SuggestionStatus = 'working' | 'ready' | 'failed';

export interface Suggestion {
  status: SuggestionStatus;
  instruction: string;
  body: string | null;
  error: string | null;
}

export const REJECT_REASONS = [
  'wrong_facts',
  'wrong_target',
  'tone',
  'incomplete',
  'should_not_draft',
  'other',
] as const;
export type RejectReason = (typeof REJECT_REASONS)[number];

export type TaskStatus = 'queued' | 'working' | 'needs_approval' | 'done' | 'failed' | 'cancelled';

/** A human-readable step. The backend sends milestones only, in order, each text once. */
export interface TaskEvent {
  at: string;
  kind: 'milestone';
  text: string;
}

export interface TaskProgress {
  done: number;
  total: number;
}

export interface Task {
  id: string;
  bot: string;
  text: string;
  status: TaskStatus;
  result: string | null;
  error: string | null;
  draft_ids: string[];
  created_at: string;
  updated_at: string;
  /** From the bot's latest checklist, or null when it has none. */
  progress: TaskProgress | null;
  /** True while the task runs again after the runtime retried it. Optional. */
  retrying?: boolean;
  /** Absent in the list answer. */
  events?: TaskEvent[];
}

export interface BriefChange {
  system: string;
  title: string;
  summary: string;
  url: string;
}

export interface FollowUp {
  id: string;
  system: string;
  who: string;
  summary: string;
  url: string;
  age_days: number;
}

export interface Brief {
  date: string;
  changed: BriefChange[];
  waiting_on_you: FollowUp[];
  you_wait_on: FollowUp[];
  /**
   * When the brief was last delivered, or null when none ever has. An older server leaves it out,
   * which the app reads the same way as null.
   */
  delivered_at?: string | null;
  /** What to say about it: never delivered, delivered within the last day, or older. A missing field reads as `never`. */
  status?: 'never' | 'today' | 'stale';
}

/**
 * Every error code in the table in docs/web-api.md ("Error codes"), with what the table says about
 * whether anything changed:
 * - `yes`: refused before anything was written, so the app may say nothing changed.
 * - `decision`: the same, but only on a decision on a draft. A task may have been made.
 * - `unknown`: it may have happened, so the app must not say either way.
 * A unit test reads the table and fails if this list and the table drift apart.
 */
export const SERVER_ERROR_CODES = {
  bad_request: 'yes',
  signed_out: 'yes',
  bad_token: 'yes',
  bad_origin: 'yes',
  bad_csrf: 'yes',
  not_a_member: 'yes',
  not_allowed: 'yes',
  forbidden: 'yes',
  not_found: 'yes',
  method_not_allowed: 'yes',
  not_pending: 'yes',
  stale: 'yes',
  expired: 'yes',
  needs_external_confirm: 'yes',
  audience_blocked: 'yes',
  not_approvable: 'yes',
  change_in_progress: 'yes',
  no_suggestion: 'yes',
  not_practice: 'yes',
  task_refused: 'yes',
  bad_content_type: 'yes',
  bad_edit: 'yes',
  bad_brief: 'yes',
  not_built: 'yes',
  upstream_unavailable: 'decision',
  unknown_outcome: 'unknown',
  audience_unavailable: 'yes',
  unavailable: 'yes',
  team_unavailable: 'yes',
  github_not_linked: 'yes',
  stale_base: 'yes',
  invalid: 'yes',
  policy_mismatch: 'yes',
  rate_limited: 'yes',
  not_configured: 'yes',
  audit_unavailable: 'yes',
  github_refused: 'yes',
  github_unavailable: 'yes',
  github_rate_limited: 'yes',
} as const satisfies Record<string, 'yes' | 'decision' | 'unknown'>;

/** Codes the server sends. */
export type ServerErrorCode = keyof typeof SERVER_ERROR_CODES;

/** Codes the server sends, plus the two the client makes up: no answer at all, and one it cannot read. */
export type ErrorCode = ServerErrorCode | 'network' | 'unknown';

export function isServerErrorCode(code: string): code is ServerErrorCode {
  return Object.hasOwn(SERVER_ERROR_CODES, code);
}

// ---- Admin and auditor (docs/web-api.md, "Admin and auditor") ----

export const AUDIT_OUTCOMES = [
  'ok',
  'denied',
  'pending',
  'approved',
  'rejected',
  'granted',
  'started',
  'error',
  'failed',
  'expired',
] as const;
export type AuditOutcome = (typeof AUDIT_OUTCOMES)[number];

/** One audit event. Every field is always present; metadata only, never draft or Slack text. */
export interface AuditEvent {
  id: string;
  at: string;
  actor: string;
  client: string;
  action: string;
  system: string;
  target: string | null;
  outcome: AuditOutcome | string;
  detail: string | null;
  sources: number;
  titled_sources: number;
  skill: string | null;
  bot: string | null;
  task_id: string;
}

/** The query the audit routes take. `before` is the opaque cursor from the page before. */
export interface AuditQuery {
  actor?: string;
  action?: string;
  outcome?: string;
  /** RFC 3339, events at or after it. */
  from?: string;
  /** RFC 3339, events strictly before it. */
  to?: string;
  limit?: number;
  before?: string;
}

export interface AuditPage {
  events: AuditEvent[];
  next_cursor: string | null;
}

export interface AuditExport {
  blob: Blob;
  filename: string;
  /** True when more events matched than the export holds. */
  truncated: boolean;
  rowCount: number;
}

export interface TeamAccounts {
  atlassian: boolean;
  slack: boolean;
  github: boolean;
}

export interface TeamMember {
  handle: string;
  name: string;
  roles: string[];
  has_email: boolean;
  bot: string;
  /** Null when the tool server could not be asked. */
  accounts: TeamAccounts | null;
}

export interface TeamList {
  members: TeamMember[];
  accounts_available: boolean;
  /** Whether account linking is on. Null when the tool server could not be asked. */
  linking: boolean | null;
}

export interface Connector {
  name: string;
  mode: 'live' | 'fake' | string;
  health: 'ok' | 'down' | string;
  error_class: string | null;
  checked_at: string;
}

/** `GET /api/drafts`: `truncated` only when the tool server left pending drafts out; then `total_pending` says how many wait. */
export interface DraftList {
  drafts: DraftSummary[];
  truncated: boolean;
  total_pending: number | null;
}

export interface AdminOverview {
  since: string;
  pending_drafts: number;
  approved: number | null;
  rejected: number | null;
  denied: number | null;
  /** All three may be missing or null from a tool server older than retention. */
  retention_days?: number | null;
  oldest_event_at?: string | null;
  last_purge?: { at: string; deleted: number } | null;
}

export class ApiError extends Error {
  readonly status: number;
  readonly code: ErrorCode | string;
  constructor(status: number, code: string, message: string) {
    super(message);
    this.name = 'ApiError';
    this.status = status;
    this.code = code;
  }
}

/** A way to sign in that the server has turned on. */
export type AuthProvider = 'entra' | 'dev';

export interface Api {
  /** Public: which sign-in methods the server offers. Never needs a session. */
  providers(): Promise<AuthProvider[]>;
  devLogin(handle: string): Promise<void>;
  logout(): Promise<void>;
  me(): Promise<Me>;
  setNudges(nudges: boolean): Promise<Me>;
  markHintSeen(seen: HintName): Promise<Me>;
  resetHints(): Promise<Me>;

  listDrafts(): Promise<DraftList>;
  getDraft(id: string): Promise<Draft>;
  saveDraft(id: string, input: { title?: string; body: string }): Promise<Draft>;
  requestChange(id: string, instruction: string): Promise<Suggestion>;
  getSuggestion(id: string): Promise<Suggestion | null>;
  acceptSuggestion(id: string): Promise<Draft>;
  rejectSuggestion(id: string): Promise<Draft>;
  approve(id: string, opts?: { confirmExternal?: boolean; version?: string }): Promise<Draft>;
  reject(id: string, reason?: RejectReason): Promise<Draft>;
  createPractice(): Promise<Draft>;
  deleteDraft(id: string): Promise<void>;

  createTask(text: string): Promise<Task>;
  getTask(id: string): Promise<Task>;
  listTasks(): Promise<Task[]>;

  brief(): Promise<Brief>;
  dismissFollowup(id: string): Promise<void>;
  draftFollowup(id: string): Promise<Task>;

  adminOverview(): Promise<AdminOverview>;
  adminAudit(query: AuditQuery): Promise<AuditPage>;
  adminAuditExport(query: AuditQuery): Promise<AuditExport>;
  adminTeam(): Promise<TeamList>;
  adminConnectors(): Promise<Connector[]>;
}
