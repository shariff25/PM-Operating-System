import type { Role } from "../permissions";

export interface Session {
  handle: string;
  display_name: string;
  roles: Role[];
  must_change_password: boolean;
  /** Working level shown in the shell, e.g. "Senior". Optional. */
  level?: string;
  /** CSRF token to echo in `X-CSRF-Token` on every non-GET request. */
  csrf_token: string;
}

export interface Bot {
  id: string;
  name: string;
  description: string;
}

export type TaskStatus = "queued" | "running" | "needs_approval" | "done" | "failed" | "cancelled";

export const SETTLED: readonly TaskStatus[] = ["done", "failed", "cancelled"];

export interface Task {
  id: string;
  bot: string;
  prompt: string;
  status: TaskStatus;
  requested_by: string;
  source: "web" | "slack" | "routine" | "cli" | "eval";
  created_at: string;
  finished_at?: string | null;
  attempts: number;
  /** Markdown-free plain text; bullet lines start with "- ". */
  result?: string | null;
  failure_reason?: string | null;
}

export interface TaskEvent {
  id: string;
  at: string;
  kind: string;
  message: string;
}

export type DraftStatus = "pending" | "approved" | "rejected" | "expired" | "failed";

export interface Recommendation {
  by: string;
  recommendation: "approve" | "reject";
  reason?: string;
  at: string;
}

export interface Draft {
  id: string;
  task_id: string;
  kind: string;
  title: string;
  preview: string;
  /** Goes somewhere outside the company: needs an explicit confirmation. */
  external: boolean;
  owner: string;
  drafted_by: string;
  status: DraftStatus;
  created_at: string;
  expires_at: string;
  recommendations: Recommendation[];
}

export interface DecisionRequest {
  decision: "approve" | "reject";
  reason?: string;
  confirm_external?: boolean;
}

export interface RecommendationRequest {
  recommendation: "approve" | "reject";
  reason?: string;
}

export interface Routine {
  id: string;
  owner: string;
  title: string;
  /** Human text, e.g. "Every weekday at 17:30, Europe/London". */
  when: string;
  next_run_at?: string | null;
  next_run_text?: string | null;
  last_run_at?: string | null;
  enabled: boolean;
}

export interface RoutineDraft {
  title: string;
  /** Plain-words confirmation sentence shown before creating. */
  summary: string;
  when: string;
  bot: string;
  text: string;
}

export interface LoginRequest {
  handle: string;
  password: string;
}

export interface ChangePasswordRequest {
  current_password: string;
  new_password: string;
}

/** One method per backend endpoint. `real` and `mock` both implement this. */
export interface Api {
  // crates/web
  login(req: LoginRequest): Promise<Session>; // POST /auth/login
  logout(): Promise<void>; // POST /auth/logout
  changePassword(req: ChangePasswordRequest): Promise<Session>; // POST /auth/change-password
  me(): Promise<Session>; // GET /auth/me
  // bots runtime (/v1)
  listBots(): Promise<Bot[]>; // GET /v1/bots
  createTask(req: { bot: string; prompt: string }): Promise<Task>; // POST /v1/tasks (not built yet)
  listTasks(opts?: { limit?: number; requested_by?: string }): Promise<Task[]>; // GET /v1/tasks
  getTask(id: string): Promise<Task>; // GET /v1/tasks/{id}
  getTaskEvents(id: string): Promise<TaskEvent[]>; // GET /v1/tasks/{id}/events
  getTaskDrafts(id: string): Promise<Draft[]>; // GET /v1/tasks/{id}/drafts
  cancelTask(id: string): Promise<Task>; // POST /v1/tasks/{id}/cancel
  listRoutines(opts?: { owner?: string }): Promise<Routine[]>; // GET /v1/routines
  setRoutineEnabled(id: string, enabled: boolean): Promise<Routine>; // PATCH /v1/routines/{id}
  deleteRoutine(id: string): Promise<void>; // DELETE /v1/routines/{id}
  parseRoutine(text: string): Promise<RoutineDraft>; // POST /v1/routines/parse (web client not built yet)
  createRoutine(draft: RoutineDraft): Promise<Routine>; // POST /v1/routines (web client not built yet)
  // tool server (/internal), reached through the crates/web proxy
  listApprovals(opts?: { user?: string; status?: DraftStatus }): Promise<Draft[]>; // GET /internal/approvals
  decideApproval(id: string, req: DecisionRequest): Promise<Draft>; // POST /internal/approvals/{id}/decision
  recommendApproval(id: string, req: RecommendationRequest): Promise<Draft>; // POST /internal/approvals/{id}/recommendation (not built yet)
}
