-- PM OS tool server: initial schema.

-- Context library index. Rebuilt from the content repo (the source of truth) on start-up and
-- on POST /internal/reindex. Embeddings arrive in a later migration, once the gateway serves
-- the embedding model chosen in the Phase 0 bake-off.
CREATE TABLE context_docs (
    path          text PRIMARY KEY,
    domain        text NOT NULL,
    title         text NOT NULL,
    owner         text NOT NULL,
    last_reviewed date NOT NULL,
    sensitivity   text NOT NULL CHECK (sensitivity IN ('public', 'internal', 'restricted')),
    body          text NOT NULL,
    tsv           tsvector GENERATED ALWAYS AS (
                      setweight(to_tsvector('english', title), 'A') ||
                      setweight(to_tsvector('english', body), 'B')
                  ) STORED,
    indexed_at    timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX context_docs_tsv ON context_docs USING gin (tsv);

-- Per-user memory (MEM-1).
CREATE TABLE memory_notes (
    user_handle text NOT NULL,
    key         text NOT NULL,
    text        text NOT NULL,
    updated_at  timestamptz NOT NULL,
    PRIMARY KEY (user_handle, key)
);

-- Sessions. Only the SHA-256 of each token is stored.
CREATE TABLE sessions (
    token_hash  text PRIMARY KEY,
    user_handle text NOT NULL,
    skill       text NOT NULL,
    run_id      text NOT NULL,
    created_at  timestamptz NOT NULL,
    expires_at  timestamptz NOT NULL
);
CREATE INDEX sessions_expires_at ON sessions (expires_at);

-- Single-use approvals for one exact write call (OUT-4). session_key is the token hash.
CREATE TABLE grants (
    id          bigserial PRIMARY KEY,
    session_key text NOT NULL,
    tool        text NOT NULL,
    args_hash   text NOT NULL,
    expires_at  timestamptz NOT NULL
);
CREATE INDEX grants_match ON grants (session_key, tool, args_hash);

-- The approval inbox.
CREATE TABLE approvals (
    id          text PRIMARY KEY,
    user_handle text NOT NULL,
    skill       text NOT NULL,
    run_id      text NOT NULL,
    tool        text NOT NULL,
    args        jsonb NOT NULL,
    args_hash   text NOT NULL,
    status      text NOT NULL CHECK (status IN ('pending', 'approved', 'rejected', 'failed')),
    created_at  timestamptz NOT NULL,
    decided_at  timestamptz,
    decided_by  text,
    result      jsonb
);
CREATE INDEX approvals_user_created ON approvals (user_handle, created_at DESC);

-- Audit log (ADM-2). Metadata only, never message bodies (SLK-8). Append-only.
CREATE TABLE audit_events (
    id          bigserial PRIMARY KEY,
    at          timestamptz NOT NULL,
    run_id      text NOT NULL,
    user_handle text NOT NULL,
    skill       text NOT NULL,
    tool        text NOT NULL,
    outcome     text NOT NULL,
    sources     text[] NOT NULL DEFAULT '{}',
    args_hash   text,
    output_ref  text,
    detail      text
);
CREATE INDEX audit_events_run ON audit_events (run_id);
CREATE INDEX audit_events_user_at ON audit_events (user_handle, at DESC);

CREATE FUNCTION audit_events_append_only() RETURNS trigger AS $$
BEGIN
    RAISE EXCEPTION 'audit_events is append-only';
END
$$ LANGUAGE plpgsql;

CREATE TRIGGER audit_events_append_only
    BEFORE UPDATE OR DELETE ON audit_events
    FOR EACH ROW EXECUTE FUNCTION audit_events_append_only();
