-- The Authority's own tables, in Agent Governance's own database. Every primary key starts
-- with (tenant, env).

-- What its admins did: one signed record per action, numbered from 1 with no gaps.
CREATE TABLE authority_records (
    tenant   TEXT NOT NULL,
    env      TEXT NOT NULL,
    seq      BIGINT NOT NULL CHECK (seq >= 1),
    hash     TEXT NOT NULL,
    at       BIGINT NOT NULL,
    envelope JSONB NOT NULL,
    PRIMARY KEY (tenant, env, seq)
);

-- The registry: people, agents, gates, enrolled keys, and one-time codes waiting to be used
-- (kind 'code-agent' or 'code-gate'; only a code's digest is here). A row changes only in
-- the transaction that appends the record standing for the change, except a code's count
-- of wrong guesses and its removal once presented, which nobody decided.
CREATE TABLE authority_registry (
    tenant TEXT NOT NULL,
    env    TEXT NOT NULL,
    kind   TEXT NOT NULL,
    name   TEXT NOT NULL,
    -- For an agent: the person who answers for it.
    owner  TEXT,
    doc    JSONB NOT NULL,
    PRIMARY KEY (tenant, env, kind, name)
);

CREATE INDEX authority_registry_owner ON authority_registry (tenant, env, owner)
    WHERE owner IS NOT NULL;
