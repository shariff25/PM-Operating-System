import { copy } from '../copy';
import { ApiError } from './types';
import type {
  AdminOverview,
  Api,
  AuditPage,
  AuditQuery,
  AuthProvider,
  Brief,
  Connector,
  Draft,
  DraftSummary,
  HintName,
  Me,
  RejectReason,
  Suggestion,
  TeamList,
  Task,
} from './types';

/** The query string for an audit route. Empty values are left out, as the server counts them as absent. */
export function auditQueryString(q: AuditQuery): string {
  const p = new URLSearchParams();
  for (const key of ['actor', 'action', 'outcome', 'from', 'to', 'before'] as const) {
    const v = q[key];
    if (v) p.set(key, v);
  }
  if (q.limit !== undefined) p.set('limit', String(q.limit));
  const s = p.toString();
  return s ? `?${s}` : '';
}

function filenameOf(disposition: string | null): string {
  const m = /filename="?([^";]+)"?/i.exec(disposition ?? '');
  return m?.[1] ?? 'pmos-audit.csv';
}

export interface RealOptions {
  /** Called on any 401: the app goes back to sign-in. */
  onSignedOut?: () => void;
  baseUrl?: string;
  fetchImpl?: typeof fetch;
}

/** An id placed in a path. Dots alone would be read as a path step, so they become an id no server has. */
function seg(id: string): string {
  return /^\.+$/.test(id) ? '%00' : encodeURIComponent(id);
}

export function createRealClient(opts: RealOptions = {}): Api {
  const base = opts.baseUrl ?? '/api';
  let csrf: string | null = null;

  /** Send a request; resolve with the response when it is a success, throw an ApiError otherwise. */
  async function send(method: string, path: string, body?: unknown): Promise<Response> {
    const headers: Record<string, string> = {};
    if (body !== undefined) headers['Content-Type'] = 'application/json';
    if (method !== 'GET' && csrf) headers['X-Pmos-Csrf'] = csrf;

    let res: Response;
    try {
      res = await (opts.fetchImpl ?? fetch)(`${base}${path}`, {
        method,
        headers,
        credentials: 'include',
        body: body === undefined ? undefined : JSON.stringify(body),
      });
    } catch {
      throw new ApiError(0, 'network', copy.errors.network);
    }

    if (res.ok) return res;

    let code = 'unknown';
    let message: string = copy.errors.unknown;
    try {
      const data = (await res.json()) as { code?: string; error?: string };
      if (data.code) code = data.code;
      if (data.error) message = data.error;
    } catch {
      /* body was not JSON; keep the plain fallback */
    }
    if (res.status === 401) {
      csrf = null;
      opts.onSignedOut?.();
      code = 'signed_out';
    }
    throw new ApiError(res.status, code, message);
  }

  async function call<T>(method: string, path: string, body?: unknown): Promise<T> {
    const res = await send(method, path, body);
    if (res.status === 204) return undefined as T;
    return (await res.json()) as T;
  }

  const post = <T>(path: string, body?: unknown) => call<T>('POST', path, body ?? undefined);

  return {
    providers: async () => (await call<{ providers: AuthProvider[] }>('GET', '/auth/providers')).providers,
    devLogin: (handle) => post<void>('/auth/dev-login', { handle }),
    logout: async () => {
      await post<void>('/auth/logout');
      csrf = null;
    },
    me: async () => {
      const me = await call<Me>('GET', '/me');
      csrf = me.csrf_token;
      return me;
    },
    setNudges: (nudges) => call<Me>('PUT', '/me/settings', { nudges }),
    markHintSeen: (seen: HintName) => post<Me>('/me/hints', { seen }),
    resetHints: () => call<Me>('DELETE', '/me/hints'),

    listDrafts: async () => {
      const r = await call<{ drafts: DraftSummary[]; truncated?: boolean; total_pending?: number }>('GET', '/drafts');
      return { drafts: r.drafts, truncated: r.truncated === true, total_pending: typeof r.total_pending === 'number' ? r.total_pending : null };
    },
    getDraft: (id) => call<Draft>('GET', `/drafts/${seg(id)}`),
    saveDraft: (id, input) => call<Draft>('PUT', `/drafts/${seg(id)}`, input),
    requestChange: async (id, instruction) =>
      (await post<{ suggestion: Suggestion }>(`/drafts/${seg(id)}/changes`, { instruction })).suggestion,
    getSuggestion: async (id) =>
      (await call<{ suggestion: Suggestion | null }>('GET', `/drafts/${seg(id)}/suggestion`)).suggestion,
    acceptSuggestion: (id) => post<Draft>(`/drafts/${seg(id)}/suggestion/accept`),
    rejectSuggestion: (id) => post<Draft>(`/drafts/${seg(id)}/suggestion/reject`),
    approve: (id, o) =>
      post<Draft>(`/drafts/${seg(id)}/approve`, {
        ...(o?.confirmExternal ? { confirm_external: true } : {}),
        ...(o?.version !== undefined ? { version: o.version } : {}),
      }),
    reject: (id, reason?: RejectReason) => post<Draft>(`/drafts/${seg(id)}/reject`, reason ? { reason } : {}),
    createPractice: () => post<Draft>('/drafts/practice'),
    deleteDraft: (id) => call<void>('DELETE', `/drafts/${seg(id)}`),

    createTask: (text) => post<Task>('/tasks', { text }),
    getTask: (id) => call<Task>('GET', `/tasks/${seg(id)}`),
    listTasks: async () => (await call<{ tasks: Task[] }>('GET', '/tasks')).tasks,

    brief: () => call<Brief>('GET', '/brief'),
    dismissFollowup: (id) => post<void>(`/followups/${seg(id)}/dismiss`),
    draftFollowup: (id) => post<Task>(`/followups/${seg(id)}/draft`),

    adminOverview: () => call<AdminOverview>('GET', '/admin/overview'),
    adminAudit: (q) => call<AuditPage>('GET', `/admin/audit${auditQueryString(q)}`),
    adminAuditExport: async (q) => {
      // The export takes the filters only, never a page size or a cursor.
      const filters = { ...q };
      delete filters.limit;
      delete filters.before;
      const res = await send('GET', `/admin/audit/export${auditQueryString(filters)}`);
      const blob = await res.blob();
      const rowCount = Number(res.headers.get('X-Pmos-Row-Count'));
      return {
        blob,
        filename: filenameOf(res.headers.get('Content-Disposition')),
        truncated: res.headers.get('X-Pmos-Truncated') === 'true',
        rowCount: Number.isFinite(rowCount) ? rowCount : 0,
      };
    },
    adminTeam: () => call<TeamList>('GET', '/admin/team'),
    adminConnectors: async () => (await call<{ connectors: Connector[] }>('GET', '/admin/connectors')).connectors,
  };
}
