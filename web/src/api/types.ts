// Types match docs/web-api.md exactly.

export type HintName = 'sources' | 'ask' | 'approve';
/**
 * The guided start's marks, kept in `hints_seen` beside the hints (docs/web-api.md, "Me"):
 * started, its three steps, skipped, and the one "Approved!" moment.
 */
export type TourMark = 'tour_started' | 'tour_asked' | 'tour_opened' | 'tour_decided' | 'tour_skipped' | 'tour_cheered';
/** Anything `POST /api/me/hints` can mark seen. */
export type SeenName = HintName | TourMark;

export interface Me {
  handle: string;
  /** What to call the person: the display name, or the handle when there is none. Plain text. */
  name: string;
  /** The display name from the user database, untrusted text (render it plainly). An older server leaves it out. */
  display_name?: string;
  roles: string[];
  csrf_token: string;
  first_visit: boolean;
  hints_seen: SeenName[];
  nudges: boolean;
  /** "Show Nudge": whether to draw the mascot. An older server leaves it out: treat that as true. */
  mascot?: boolean;
  /** What the person's roles allow, kebab-case. An older server leaves it out: treat that as none. */
  permissions?: string[];
  /** True when the person holds the reviewer role: show Reviews. An older server leaves it out: treat that as false. */
  reviewer?: boolean;
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
  /** What the reviewers assigned to the PM recommend, oldest first, one per reviewer. An older server leaves it out. */
  recommendations?: Recommendation[];
}

// ---- Reviews (docs/web-api.md, "Reviews") ----

/** `approve` ("recommends approving") or `changes` ("suggests changes"). */
export type Verdict = 'approve' | 'changes';

/** A reviewer's advice on the owner's draft. Names and notes are untrusted plain text. */
export interface Recommendation {
  reviewer: string;
  /** Their display name, or their handle. Untrusted: plain text only. */
  reviewer_name: string;
  verdict: Verdict;
  /** Untrusted plain text, or null: none given, or removed with a Slack draft's text (`note_removed`). */
  note: string | null;
  note_removed: boolean;
  created_at: string;
  /** The draft changed since the advice was given. */
  stale: boolean;
}

/** The reviewer's own live recommendation on a draft. */
export interface MyRecommendation {
  verdict: Verdict;
  note: string | null;
  note_removed: boolean;
  revision: number;
  created_at: string;
  stale: boolean;
}

/**
 * One line of the reviewer's inbox. A restricted draft (`restricted` anything but `false`) is
 * metadata only: no title, no `args_hash`, no `edited`, no recommendation.
 */
export interface ReviewSummary {
  id: string;
  owner: string;
  /** Untrusted: plain text only. */
  owner_name: string;
  kind: DraftKind;
  status: DraftStatus;
  created_at: string;
  expires_at: string | null;
  restricted: boolean;
  /** The version of the draft's text, 1 as drafted. Null from an older tool server. */
  revision: number | null;
  title?: string | null;
  edited?: boolean;
  /** 64 lowercase hex: send it back with `revision`. */
  args_hash?: string;
  my_recommendation?: MyRecommendation | null;
}

/** One draft as a reviewer reads it. The text, target and sources are absent when restricted. */
export interface ReviewDraft extends ReviewSummary {
  body?: string;
  target?: DraftTarget;
  sources?: Source[];
}

export interface ReviewList {
  drafts: ReviewSummary[];
  total: number;
  truncated: boolean;
  /** The handles whose drafts the reviewer reviews now. */
  pms: string[];
}

export interface RecommendInput {
  verdict: Verdict;
  note?: string;
  revision: number;
  args_hash: string;
}

export interface Recommended {
  recommendation: MyRecommendation;
  draft: ReviewDraft;
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
  /**
   * True when PM OS couldn't check the person's access just now and left out every item from
   * Slack (docs/web-api.md, "The daily brief and follow-ups"). A missing field reads as false.
   */
  slack_withheld?: boolean;
  /** The server's sentence for `slack_withheld`; the app shows its own copy. */
  note?: string;
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
  own_draft: 'yes',
  restricted_draft: 'yes',
  not_found: 'yes',
  method_not_allowed: 'yes',
  not_pending: 'yes',
  stale: 'yes',
  expired: 'yes',
  draft_changed: 'yes',
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
  users_unavailable: 'yes',
  entra_needs_host: 'yes',
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
  assertion_required: 'yes',
  host_only: 'yes',
  self_change: 'yes',
  exists: 'yes',
  taken: 'yes',
  unchanged: 'yes',
  last_admin: 'yes',
  dual_role: 'yes',
  evals_account: 'yes',
  not_active: 'yes',
  linked: 'yes',
  owns_bots: 'yes',
  not_reviewer: 'yes',
  not_pm: 'yes',
  deactivation_limit: 'yes',
  sign_in_failed: 'yes',
  code_failed: 'yes',
  sign_in_expired: 'yes',
  setup_failed: 'yes',
  setup_expired: 'yes',
  wrong_step: 'yes',
  weak_password: 'yes',
  reauth_failed: 'yes',
  no_password: 'yes',
  sign_in_unconfirmed: 'yes',
  too_many_attempts: 'yes',
  sign_in_busy: 'yes',
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

/** `users` in `GET /api/admin/overview`: the user database's flags. Handles and counts only. */
export interface OverviewUsers {
  active: number;
  deactivated: number;
  sign_in_unconfirmed: string[];
  dual_role: string[];
  pilot_exception: { ends: string; ending_soon: boolean; lapsed: boolean } | null;
  role_conflicts: string[];
  entra_needs_host: string[];
  no_slack: string[];
  identity_conflicts: number | null;
  identity_conflict_users: string[] | null;
  resolve_miss_warning: string[];
  resolve_miss_warning_threshold: number;
  deactivations_last_hour: number;
  deactivation_limit: number;
  deactivation_limit_reached: boolean;
  deactivation_grace_hours: number;
  bots_without_active_owner: string[];
}

export interface AdminOverview {
  since: string;
  pending_drafts: number;
  /** Pending drafts of deactivated people, held for the grace period. Missing or null from an older tool server. */
  held_drafts?: number | null;
  /** The user database's flags. Missing or null from an older tool server. */
  users?: OverviewUsers | null;
  approved: number | null;
  rejected: number | null;
  denied: number | null;
  /** All three may be missing or null from a tool server older than retention. */
  retention_days?: number | null;
  oldest_event_at?: string | null;
  last_purge?: { at: string; deleted: number } | null;
}

// ---- Admin: users and roles (docs/web-api.md, "Admin: users and roles") ----

/** The roles a user can hold, in the order the server sorts them. */
export const USER_ROLES = ['admin', 'pm', 'reviewer', 'auditor'] as const;
export type UserRole = (typeof USER_ROLES)[number];

/** The kinds of identity an admin can unlink. */
export type IdentityKind = 'entra' | 'atlassian' | 'github';

/** One person in the user database. The three address fields are in the admin view only (absent, not null, for an auditor). */
export interface DirectoryUser {
  handle: string;
  /** Untrusted text (often from Slack): render as plain text only. `""` when none. */
  display_name: string;
  status: 'active' | 'deactivated';
  roles: string[];
  created_at: string;
  deactivated_at: string | null;
  grace_ends_at: string | null;
  dual_role: boolean;
  role_conflict: boolean;
  has_sign_in_name: boolean;
  sign_in_name_confirmed: boolean;
  sign_in_unconfirmed: boolean;
  entra: 'none' | 'linked' | 'needs_host';
  entra_needs_host: boolean;
  slack_linked: boolean;
  entra_linked: boolean;
  atlassian_linked: boolean;
  github_linked: boolean;
  /** Whether each account's token is linked now. Null when it couldn't be read (writes' answers only). */
  accounts: TeamAccounts | null;
  host_only: boolean;
  bot: string;
  team_bots: string[];
  reviews: string[];
  reviewed_by: string[];
  sign_in_name?: string | null;
  atlassian_email?: string | null;
  github_login?: string | null;
  /** Password sign-in (docs/web-api.md, "Password sign-in"): booleans only. Absent from an older server. */
  password_set?: boolean;
  second_step_enrolled?: boolean;
  setup_code_pending?: boolean;
}

export interface PilotException {
  ends: string;
  ends_at: string;
  ending_soon: boolean;
  lapsed: boolean;
}

export interface UserList {
  version: number;
  view: 'admin' | 'auditor';
  users: DirectoryUser[];
  deactivations: { last_hour: number; limit: number; remaining: number; grace_hours: number };
  pilot_exception: PilotException | null;
  linking: boolean;
}

/** Every write's answer: the person as they are now (admin view) and the directory's new version. */
export interface Written {
  version: number;
  user: DirectoryUser;
  /** Only on a deactivation: plain text for the admin. */
  note?: string;
}

export interface NewUser {
  handle: string;
  display_name?: string;
  sign_in_name: string;
  atlassian_email: string;
  github_login?: string | null;
  slack_id?: string | null;
  roles: string[];
}

export interface UserFields {
  sign_in_name?: string;
  atlassian_email?: string;
  github_login?: string;
  display_name?: string;
}

export class ApiError extends Error {
  readonly status: number;
  readonly code: ErrorCode | string;
  /** The rest of the error body, for the codes that carry more (`paths`, `base`, `errors`, `warnings`). */
  readonly details: Readonly<Record<string, unknown>>;
  /** Seconds from `Retry-After`, when the server sent one. */
  readonly retryAfter: number | null;
  constructor(status: number, code: string, message: string, details: Record<string, unknown> = {}, retryAfter: number | null = null) {
    super(message);
    this.name = 'ApiError';
    this.status = status;
    this.code = code;
    this.details = details;
    this.retryAfter = retryAfter;
  }
}

/** A way to sign in that the server has turned on. `password` is email-and-password sign-in with an authenticator code (docs/web-api.md, "Password sign-in"). */
export type AuthProvider = 'entra' | 'password' | 'dev';

// ---- Password sign-in (docs/web-api.md, "Password sign-in"). The screens aren't built yet. ----

/** Why a new password was refused (`422 weak_password`'s `reason`). */
export type WeakPasswordReason = 'too_short' | 'too_long' | 'matches_name' | 'common';

/** `POST /api/auth/password/sign-in`: the password was right; the second step is next. */
export interface PasswordSignInStarted {
  next: 'second_step';
  expires_at: string;
}

/** `POST /api/auth/password/second-step`: signed in. */
export interface PasswordSignedIn {
  recovery_code_used: boolean;
  recovery_codes_left: number;
}

/** `POST /api/auth/password/setup/begin`. */
export interface PasswordSetupStarted {
  handle: string;
  kind: 'setup' | 'reset';
  expires_at: string;
  password: { min_length: number; max_length: number };
}

/** `POST /api/auth/password/setup/authenticator`: shown once; the app draws `otpauth_uri` as a QR code. */
export interface PasswordEnrolment {
  otpauth_uri: string;
  secret: string;
  account: string;
  issuer: string;
  algorithm: 'SHA1';
  digits: 6;
  period: 30;
}

/** `POST /api/auth/password/setup/confirm` and `POST /api/auth/password/recovery-codes`: shown once. */
export interface PasswordRecoveryCodes {
  kind?: 'setup' | 'reset';
  recovery_codes: string[];
}

/** `GET /api/auth/password/status`. */
export interface PasswordStatus {
  enabled: boolean;
  password_set: boolean;
  second_step_enrolled: boolean;
  recovery_codes_left: number;
}

/** `POST /api/admin/users/{handle}/password-setup-code`: shown once to the admin. */
export interface PasswordSetupCode {
  setup_code: string;
  handle: string;
  kind: 'setup' | 'reset';
  expires_at: string;
}

export interface Api {
  /** Public: which sign-in methods the server offers. Never needs a session. */
  providers(): Promise<AuthProvider[]>;
  devLogin(handle: string): Promise<void>;
  logout(): Promise<void>;
  me(): Promise<Me>;
  setNudges(nudges: boolean): Promise<Me>;
  setMascot(mascot: boolean): Promise<Me>;
  markHintSeen(seen: SeenName): Promise<Me>;
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

  listReviews(): Promise<ReviewList>;
  getReview(id: string): Promise<ReviewDraft>;
  recommend(id: string, input: RecommendInput): Promise<Recommended>;

  adminOverview(): Promise<AdminOverview>;
  adminAudit(query: AuditQuery): Promise<AuditPage>;
  adminAuditExport(query: AuditQuery): Promise<AuditExport>;
  adminTeam(): Promise<TeamList>;
  adminConnectors(): Promise<Connector[]>;

  adminUsers(): Promise<UserList>;
  adminInvite(input: NewUser): Promise<Written>;
  adminSetRoles(handle: string, roles: string[], expectedVersion: number): Promise<Written>;
  adminSetFields(handle: string, fields: UserFields, expectedVersion: number): Promise<Written>;
  adminConfirmSignIn(handle: string, signInName: string, expectedVersion?: number): Promise<Written>;
  adminDeactivate(handle: string, expectedVersion?: number): Promise<Written>;
  adminReactivate(handle: string, expectedVersion?: number): Promise<Written>;
  adminUnlink(handle: string, kind: IdentityKind): Promise<Written>;
  adminAssignReviewer(reviewer: string, pms: string[], expectedVersion?: number): Promise<Written>;

  adminPolicy(): Promise<PolicyView>;
  adminPolicyValidate(spec: SpecPatch): Promise<Validation>;
  adminPolicyPropose(input: { spec: SpecPatch; title: string; reason: string }): Promise<ProposalResult>;
  adminPolicyProposals(): Promise<ProposalList>;
  adminContent(): Promise<ContentView>;
  adminContentFile(path: string): Promise<ContentFileText>;
  adminContentValidate(changes: ContentChange[]): Promise<Validation>;
  adminContentPropose(input: { changes: ContentChange[]; title: string; reason: string }): Promise<ProposalResult>;
  adminContentProposals(): Promise<ProposalList>;
  adminHarness(): Promise<Harness>;
}

export type * from './policyTypes';
import type {
  ContentChange,
  ContentFileText,
  ContentView,
  Harness,
  PolicyView,
  ProposalList,
  ProposalResult,
  SpecPatch,
  Validation,
} from './policyTypes';
