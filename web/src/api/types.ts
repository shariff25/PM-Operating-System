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

export interface Draft extends DraftSummary {
  body: string;
  edited: boolean;
  target: DraftTarget;
  sources: Source[];
  result_url: string | null;
  external: boolean;
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
  /** Optional summary of the same fact. */
  status?: 'never' | 'today' | 'stale';
}

/** Error codes named in the contract; the server may add others. */
export type ErrorCode =
  | 'signed_out'
  | 'bad_csrf'
  | 'not_a_member'
  | 'not_found'
  | 'not_pending'
  | 'change_in_progress'
  | 'needs_external_confirm'
  | 'audience_blocked'
  | 'expired'
  | 'stale'
  | 'not_practice'
  | 'not_built'
  | 'forbidden'
  | 'bad_request'
  | 'upstream_unavailable'
  /** The server could not tell whether a write went through. */
  | 'unknown_outcome'
  | 'network'
  | 'unknown';

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

export interface AdminOverview {
  since: string;
  pending_drafts: number;
  approved: number | null;
  rejected: number | null;
  denied: number | null;
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

  listDrafts(): Promise<DraftSummary[]>;
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
