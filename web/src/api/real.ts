import { ApiError, NetworkError } from "./errors";
import type { Api, Session } from "./types";

export interface RealApiOptions {
  /** Prefix for every path. Default: "" (same origin). */
  baseUrl?: string;
  fetch?: typeof fetch;
  /** Called on any 401 except from /auth/login. The app returns to the sign-in screen. */
  onUnauthorized?: () => void;
}

const CSRF_HEADER = "X-CSRF-Token";

function messageFor(status: number): string {
  if (status === 401) return "Your session has ended. Sign in again.";
  if (status === 403) return "You do not have permission to do that.";
  if (status === 404) return "That was not found.";
  if (status === 429) return "Too many attempts. Wait a moment and try again.";
  if (status >= 500) return "The server had a problem. Try again in a moment.";
  return "The request could not be completed.";
}

async function readError(res: Response): Promise<ApiError> {
  let code = "http_" + res.status;
  let message = messageFor(res.status);
  try {
    const body: unknown = await res.json();
    if (body && typeof body === "object") {
      const b = body as Record<string, unknown>;
      const e = b["error"];
      if (typeof e === "string") {
        message = typeof b["message"] === "string" ? b["message"] : e;
        code = e;
      } else if (e && typeof e === "object") {
        const eo = e as Record<string, unknown>;
        if (typeof eo["code"] === "string") code = eo["code"];
        if (typeof eo["message"] === "string") message = eo["message"];
      } else if (typeof b["message"] === "string") {
        message = b["message"];
      }
    }
  } catch {
    // Not JSON: keep the generic message.
  }
  return new ApiError(res.status, code, message);
}

export function createRealApi(opts: RealApiOptions = {}): Api {
  const base = opts.baseUrl ?? "";
  const doFetch = opts.fetch ?? ((...a: Parameters<typeof fetch>) => fetch(...a));
  let csrf = "";

  async function request<T>(method: string, path: string, body?: unknown): Promise<T> {
    const headers: Record<string, string> = { Accept: "application/json" };
    if (body !== undefined) headers["Content-Type"] = "application/json";
    if (method !== "GET" && csrf) headers[CSRF_HEADER] = csrf;
    let res: Response;
    try {
      res = await doFetch(base + path, {
        method,
        credentials: "include",
        headers,
        body: body === undefined ? undefined : JSON.stringify(body),
      });
    } catch {
      throw new NetworkError();
    }
    if (!res.ok) {
      const err = await readError(res);
      if (err.status === 401 && path !== "/auth/login") {
        csrf = "";
        opts.onUnauthorized?.();
      }
      throw err;
    }
    if (res.status === 204) return undefined as T;
    const text = await res.text();
    return (text ? JSON.parse(text) : undefined) as T;
  }

  const q = (params: Record<string, string | number | undefined>): string => {
    const u = new URLSearchParams();
    for (const [k, v] of Object.entries(params)) if (v !== undefined) u.set(k, String(v));
    const s = u.toString();
    return s ? "?" + s : "";
  };
  const id = encodeURIComponent;

  const remember = (s: Session): Session => {
    csrf = s.csrf_token;
    return s;
  };

  return {
    login: async (req) => remember(await request<Session>("POST", "/auth/login", req)),
    logout: async () => {
      try {
        await request<void>("POST", "/auth/logout");
      } finally {
        csrf = "";
      }
    },
    changePassword: async (req) =>
      remember(await request<Session>("POST", "/auth/change-password", req)),
    me: async () => remember(await request<Session>("GET", "/auth/me")),
    listBots: () => request("GET", "/v1/bots"),
    createTask: (req) => request("POST", "/v1/tasks", req),
    listTasks: (o) => request("GET", "/v1/tasks" + q({ limit: o?.limit, requested_by: o?.requested_by })),
    getTask: (t) => request("GET", `/v1/tasks/${id(t)}`),
    getTaskEvents: (t) => request("GET", `/v1/tasks/${id(t)}/events`),
    getTaskDrafts: (t) => request("GET", `/v1/tasks/${id(t)}/drafts`),
    cancelTask: (t) => request("POST", `/v1/tasks/${id(t)}/cancel`),
    listRoutines: (o) => request("GET", "/v1/routines" + q({ owner: o?.owner })),
    setRoutineEnabled: (r, enabled) => request("PATCH", `/v1/routines/${id(r)}`, { enabled }),
    deleteRoutine: (r) => request("DELETE", `/v1/routines/${id(r)}`),
    parseRoutine: (text) => request("POST", "/v1/routines/parse", { text }),
    createRoutine: (draft) => request("POST", "/v1/routines", draft),
    listApprovals: (o) => request("GET", "/internal/approvals" + q({ user: o?.user, status: o?.status })),
    decideApproval: (a, req) => request("POST", `/internal/approvals/${id(a)}/decision`, req),
    recommendApproval: (a, req) => request("POST", `/internal/approvals/${id(a)}/recommendation`, req),
  };
}
