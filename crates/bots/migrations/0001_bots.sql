-- pmos-bots: the durable task queue, its events and traces, the Slack notification feed and
-- routines (D2, D6, D8). Bookkeeping is in `_pmos_bots_migrations`.

CREATE TABLE bots_tasks (
    id           text PRIMARY KEY,
    bot          text NOT NULL,
    requested_by text NOT NULL,
    source       text NOT NULL CHECK (source IN ('slack', 'routine', 'cli', 'eval')),
    priority     text NOT NULL CHECK (priority IN ('interactive', 'scheduled', 'personal')),
    -- Claim order: interactive (0) before scheduled (1) before personal (2).
    priority_rank smallint NOT NULL,
    text         text NOT NULL,
    status       text NOT NULL CHECK (status IN
                     ('queued', 'working', 'needs_approval', 'done', 'failed', 'cancelled')),
    checklist    jsonb NOT NULL DEFAULT '[]',
    result       text,
    error        text,
    approvals    jsonb NOT NULL DEFAULT '[]',
    slack        jsonb,
    routine_id   text,
    attempts     integer NOT NULL DEFAULT 0,
    worker       text,
    lease_until  timestamptz,
    created_at   timestamptz NOT NULL,
    updated_at   timestamptz NOT NULL
);
CREATE INDEX bots_tasks_queue ON bots_tasks (priority_rank, created_at, id) WHERE status = 'queued';
CREATE INDEX bots_tasks_leases ON bots_tasks (lease_until) WHERE status = 'working';
CREATE INDEX bots_tasks_waiting ON bots_tasks (updated_at) WHERE status = 'needs_approval';
CREATE INDEX bots_tasks_created ON bots_tasks (created_at DESC, id DESC);
CREATE INDEX bots_tasks_requested_by ON bots_tasks (requested_by, created_at DESC);

CREATE TABLE bots_task_events (
    id      bigserial PRIMARY KEY,
    task_id text NOT NULL REFERENCES bots_tasks (id) ON DELETE CASCADE,
    at      timestamptz NOT NULL,
    kind    text NOT NULL,
    text    text NOT NULL
);
CREATE INDEX bots_task_events_task ON bots_task_events (task_id, id);

-- The full trace (D6): kept 30 days; Slack tool results are redacted after 24 hours.
CREATE TABLE bots_traces (
    id       bigserial PRIMARY KEY,
    task_id  text NOT NULL,
    at       timestamptz NOT NULL,
    kind     text NOT NULL CHECK (kind IN
                 ('prompt', 'model_reply', 'tool_call', 'tool_result', 'policy_denial')),
    -- The tool the entry is about, from the body, so Slack results can be found again.
    tool     text,
    body     jsonb NOT NULL,
    redacted boolean NOT NULL DEFAULT false
);
CREATE INDEX bots_traces_task ON bots_traces (task_id, id);
CREATE INDEX bots_traces_at ON bots_traces (at);
CREATE INDEX bots_traces_slack ON bots_traces (at)
    WHERE kind = 'tool_result' AND NOT redacted AND (tool LIKE 'slack.%' OR tool LIKE 'slack\_%');

-- The Slack client's feed. `seq` comes from `bots_counters` under a row lock held to commit,
-- so sequence numbers become visible in order and a reader's cursor never skips one.
CREATE TABLE bots_notifications (
    seq          bigint PRIMARY KEY,
    task_id      text NOT NULL,
    bot          text NOT NULL,
    requested_by text NOT NULL,
    slack        jsonb,
    kind         text NOT NULL,
    text         text NOT NULL,
    approvals    jsonb NOT NULL DEFAULT '[]',
    outcomes     jsonb NOT NULL DEFAULT '[]',
    at           timestamptz NOT NULL
);
CREATE INDEX bots_notifications_at ON bots_notifications (at);

CREATE TABLE bots_counters (
    name  text PRIMARY KEY,
    value bigint NOT NULL
);
INSERT INTO bots_counters (name, value) VALUES ('notifications', 0);

CREATE TABLE bots_routines (
    id          text PRIMARY KEY,
    bot         text NOT NULL,
    owner       text NOT NULL,
    enabled     boolean NOT NULL,
    cron        text NOT NULL,
    tz          text NOT NULL,
    prompt      text NOT NULL,
    output      jsonb NOT NULL,
    approval    text NOT NULL CHECK (approval = 'approve'),
    next_run_at timestamptz,
    last_run_at timestamptz,
    created_at  timestamptz NOT NULL,
    updated_at  timestamptz NOT NULL
);
CREATE INDEX bots_routines_due ON bots_routines (next_run_at) WHERE enabled;
CREATE INDEX bots_routines_owner ON bots_routines (owner, created_at);
