-- pmos-web: the PM OS user database (local accounts) and server-side web sessions.
-- The user table is the source of truth for people and roles; team.yaml follows it.

CREATE TABLE web_users (
    id                   text PRIMARY KEY CHECK (length(id) = 36),
    handle               text NOT NULL UNIQUE CHECK (handle ~ '^[a-z0-9][a-z0-9._-]{0,63}$'),
    display_name         text NOT NULL CHECK (char_length(display_name) BETWEEN 1 AND 100),
    -- A new role needs a migration here as well as in pmos_contract::Role.
    roles                text[] NOT NULL
                         CHECK (cardinality(roles) >= 1
                                AND roles <@ ARRAY['admin', 'pm', 'reviewer', 'auditor']::text[]),
    status               text NOT NULL CHECK (status IN ('active', 'disabled')),
    -- An Argon2id PHC string. Never a plain password.
    password_hash        text NOT NULL CHECK (password_hash LIKE '$argon2id$%'),
    failed_attempts      integer NOT NULL DEFAULT 0 CHECK (failed_attempts >= 0),
    locked_until         timestamptz,
    must_change_password boolean NOT NULL,
    created_at           timestamptz NOT NULL,
    updated_at           timestamptz NOT NULL,
    password_changed_at  timestamptz NOT NULL,
    last_login_at        timestamptz
);

-- Keyed by the SHA-256 of the session id: the id itself is never stored.
CREATE TABLE web_sessions (
    id_hash      text PRIMARY KEY CHECK (id_hash ~ '^[0-9a-f]{64}$'),
    user_id      text NOT NULL REFERENCES web_users (id) ON DELETE CASCADE,
    provider     text NOT NULL,
    created_at   timestamptz NOT NULL,
    last_seen_at timestamptz NOT NULL,
    expires_at   timestamptz NOT NULL
);

CREATE INDEX web_sessions_user ON web_sessions (user_id);
CREATE INDEX web_sessions_expires ON web_sessions (expires_at);
