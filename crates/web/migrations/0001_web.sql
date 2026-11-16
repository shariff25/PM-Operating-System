-- pmos-web: sessions, per-PM settings and the follow-up tracker. Every PM's rows are keyed by
-- `handle`, and every query filters on it.

-- Sessions are stored by the SHA-256 of the cookie value only; the cookie itself is never
-- written down. Idle and absolute expiry are enforced in the queries that read them.
CREATE TABLE web_sessions (
    id_hash    bytea PRIMARY KEY,
    handle     text NOT NULL,
    csrf_token text NOT NULL,
    created_at timestamptz NOT NULL,
    last_seen  timestamptz NOT NULL
);
CREATE INDEX web_sessions_handle ON web_sessions (handle);
CREATE INDEX web_sessions_last_seen ON web_sessions (last_seen);
CREATE INDEX web_sessions_created_at ON web_sessions (created_at);

CREATE TABLE web_user_settings (
    handle         text PRIMARY KEY,
    hints_seen     text[] NOT NULL DEFAULT '{}',
    nudges         boolean NOT NULL DEFAULT true,
    opened_a_draft boolean NOT NULL DEFAULT false,
    updated_at     timestamptz NOT NULL
);

-- A PM's saved edit of a draft; deleted when the draft is decided, and purged when stale.
CREATE TABLE web_draft_edits (
    handle     text NOT NULL,
    draft_id   text NOT NULL,
    title      text,
    body       text NOT NULL,
    updated_at timestamptz NOT NULL,
    PRIMARY KEY (handle, draft_id)
);
CREATE INDEX web_draft_edits_updated_at ON web_draft_edits (updated_at);

CREATE TABLE web_suggestions (
    handle      text NOT NULL,
    draft_id    text NOT NULL,
    task_id     text,
    instruction text NOT NULL,
    status      text NOT NULL,
    body        text,
    error       text,
    updated_at  timestamptz NOT NULL,
    PRIMARY KEY (handle, draft_id)
);
CREATE INDEX web_suggestions_updated_at ON web_suggestions (updated_at);

-- The practice draft: at most one per PM. Lives only here, never upstream.
CREATE TABLE web_practice_drafts (
    handle     text PRIMARY KEY,
    id         text NOT NULL,
    status     text NOT NULL,
    created_at text NOT NULL,
    decided_at text
);

-- Drafts the tool server said post to a channel shared outside the company.
CREATE TABLE web_draft_flags (
    handle     text NOT NULL,
    draft_id   text NOT NULL,
    created_at timestamptz NOT NULL,
    PRIMARY KEY (handle, draft_id)
);

-- Tasks this backend made to revise drafts; hidden from the PM's task list.
CREATE TABLE web_revision_tasks (
    handle     text NOT NULL,
    task_id    text NOT NULL,
    created_at timestamptz NOT NULL,
    PRIMARY KEY (handle, task_id)
);

-- Follow-ups: one-line summaries only, never message text. Slack-derived rows expire after
-- 24 hours, others after a week; expired rows are purged.
CREATE TABLE web_followups (
    handle    text NOT NULL,
    id        text NOT NULL,
    kind      text NOT NULL CHECK (kind IN ('waiting_on_you', 'you_wait_on')),
    system    text NOT NULL,
    who       text NOT NULL,
    summary   text NOT NULL,
    url       text NOT NULL,
    created   timestamptz NOT NULL,
    expires   timestamptz NOT NULL,
    dismissed boolean NOT NULL DEFAULT false,
    PRIMARY KEY (handle, id)
);
CREATE INDEX web_followups_expires ON web_followups (expires);

-- The day's "what changed" items for a PM.
CREATE TABLE web_brief_days (
    handle     text PRIMARY KEY,
    date       date NOT NULL,
    changed    jsonb NOT NULL,
    updated_at timestamptz NOT NULL
);
