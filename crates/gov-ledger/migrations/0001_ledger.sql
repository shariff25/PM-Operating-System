-- The Ledger's own tables, in Agent Governance's own database (spec §9). Every primary key
-- starts with (tenant, env): no row is reachable without both.

CREATE TABLE gate_keys (
    tenant  TEXT NOT NULL,
    env     TEXT NOT NULL,
    kid     TEXT NOT NULL,
    gate    TEXT NOT NULL,
    key     JSONB NOT NULL,
    PRIMARY KEY (tenant, env, kid)
);

-- One key, one gate: a key enrolled under two ids could be disowned under either.
CREATE UNIQUE INDEX gate_keys_one_key ON gate_keys (tenant, env, (key ->> 'key'));

-- The last receipt of each gate's chain. Kept apart from the receipts so that pruning them
-- does not lose where the chain stands.
CREATE TABLE chain_heads (
    tenant  TEXT NOT NULL,
    env     TEXT NOT NULL,
    gate    TEXT NOT NULL,
    seq     BIGINT NOT NULL CHECK (seq >= 1),
    hash    TEXT NOT NULL,
    PRIMARY KEY (tenant, env, gate)
);

CREATE TABLE receipts (
    tenant   TEXT NOT NULL,
    env      TEXT NOT NULL,
    gate     TEXT NOT NULL,
    seq      BIGINT NOT NULL CHECK (seq >= 1),
    hash     TEXT NOT NULL,
    at       BIGINT NOT NULL,
    -- The Ledger's own clock at intake. Pruning goes by this, not by the gate's "at".
    received_at BIGINT NOT NULL,
    envelope JSONB NOT NULL,
    PRIMARY KEY (tenant, env, gate, seq)
);

-- What intake refused. The envelope itself is kept only when its signature held (a fork or a
-- gap signed by a real gate is evidence); otherwise only its hash.
CREATE TABLE refusals (
    tenant        TEXT NOT NULL,
    env           TEXT NOT NULL,
    id            BIGINT GENERATED ALWAYS AS IDENTITY,
    at            BIGINT NOT NULL,
    kid           TEXT NOT NULL,
    code          TEXT NOT NULL,
    envelope_hash TEXT NOT NULL,
    envelope      JSONB,
    PRIMARY KEY (tenant, env, id)
);

CREATE TABLE checkpoints (
    tenant   TEXT NOT NULL,
    env      TEXT NOT NULL,
    seq      BIGINT NOT NULL CHECK (seq >= 1),
    at       BIGINT NOT NULL,
    envelope JSONB NOT NULL,
    PRIMARY KEY (tenant, env, seq)
);

CREATE TABLE pruned (
    tenant      TEXT NOT NULL,
    env         TEXT NOT NULL,
    gate        TEXT NOT NULL,
    through_seq BIGINT NOT NULL,
    envelope    JSONB NOT NULL,
    PRIMARY KEY (tenant, env, gate)
);
