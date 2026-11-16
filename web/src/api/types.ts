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

export interface TaskEvent {
  at: string;
  kind: string;
  text: string;
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
  | 'network'
  | 'unknown';

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

export interface Api {
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
}
