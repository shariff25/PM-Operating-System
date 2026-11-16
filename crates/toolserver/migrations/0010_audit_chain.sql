-- Unified audit envelope and hash chain (docs/toolserver.md, "Audit log").
--
-- The original columns stay (at, run_id, user_handle, skill, bot, tool, outcome, sources,
-- args_hash, output_ref, detail, client), so existing writers and readers are unaffected.
-- New columns hold the envelope: who (actor), through which channel, doing what (action) to
-- what (target), with what result (outcome, reason_code), from which request (ids and
-- digests), plus the chain fields. No column holds free text a caller controls.
--
-- Rows written before this migration have seq, hash and prev_hash NULL: the chain starts
-- after them and they are protected by the append-only triggers only.

-- Events that aren't tool calls (sign-in, role change, ...) have no run or tool.
ALTER TABLE audit_events ALTER COLUMN run_id DROP NOT NULL;
ALTER TABLE audit_events ALTER COLUMN tool DROP NOT NULL;

ALTER TABLE audit_events
    ADD COLUMN seq             bigint,
    ADD COLUMN schema_version  integer,
    ADD COLUMN source          text,
    ADD COLUMN actor_type      text,
    ADD COLUMN idp_oid         text,
    ADD COLUMN idp_tid         text,
    ADD COLUMN session_id      text,
    ADD COLUMN channel         text,
    ADD COLUMN action          text,
    ADD COLUMN target_type     text,
    ADD COLUMN target_id       text,
    ADD COLUMN reason_code     text,
    ADD COLUMN request_id      text,
    ADD COLUMN source_ip_hash  text,
    ADD COLUMN user_agent_hash text,
    ADD COLUMN before_hash     text,
    ADD COLUMN after_hash      text,
    ADD COLUMN prev_hash       text,
    ADD COLUMN hash            text;

ALTER TABLE audit_events ADD CONSTRAINT audit_events_channel_check
    CHECK (channel IS NULL OR channel IN ('web', 'slack', 'api', 'mcp', 'system'));
ALTER TABLE audit_events ADD CONSTRAINT audit_events_actor_type_check
    CHECK (actor_type IS NULL OR actor_type IN ('human', 'bot', 'service', 'system'));
-- A chained row is complete.
ALTER TABLE audit_events ADD CONSTRAINT audit_events_chain_complete
    CHECK (seq IS NULL OR (
        hash IS NOT NULL AND prev_hash IS NOT NULL AND schema_version IS NOT NULL
        AND source IS NOT NULL AND actor_type IS NOT NULL AND channel IS NOT NULL
        AND action IS NOT NULL));

CREATE UNIQUE INDEX audit_events_seq ON audit_events (seq) WHERE seq IS NOT NULL;
CREATE INDEX audit_events_at ON audit_events (at);
CREATE INDEX audit_events_action_at ON audit_events (action, at DESC);

-- The chain head: one row, locked `FOR UPDATE` by every insert, so seq is gapless and two
-- writers can't fork the chain. It may only move forward by one.
CREATE TABLE audit_chain_head (
    id   integer PRIMARY KEY CHECK (id = 1),
    seq  bigint NOT NULL,
    hash text NOT NULL
);
INSERT INTO audit_chain_head (id, seq, hash) VALUES (1, 0, repeat('0', 64));

CREATE FUNCTION audit_chain_head_guard() RETURNS trigger AS $$
BEGIN
    IF TG_OP = 'UPDATE' AND NEW.id = OLD.id AND NEW.seq = OLD.seq + 1 THEN
        RETURN NEW;
    END IF;
    RAISE EXCEPTION 'audit_chain_head is append-only: it only moves forward by one';
END
$$ LANGUAGE plpgsql;

CREATE TRIGGER audit_chain_head_guard
    BEFORE UPDATE OR DELETE ON audit_chain_head
    FOR EACH ROW EXECUTE FUNCTION audit_chain_head_guard();
CREATE TRIGGER audit_chain_head_no_truncate
    BEFORE TRUNCATE ON audit_chain_head
    FOR EACH STATEMENT EXECUTE FUNCTION audit_chain_head_guard();

-- One row per prune: the last pruned seq and its hash, chained to the previous checkpoint.
-- Verification starts from the newest checkpoint.
CREATE TABLE audit_checkpoints (
    through_seq  bigint PRIMARY KEY,
    through_hash text NOT NULL,
    rows_pruned  bigint NOT NULL,
    cutoff       timestamptz NOT NULL,
    pruned_at    timestamptz NOT NULL,
    prev_hash    text NOT NULL,
    hash         text NOT NULL
);

CREATE FUNCTION audit_checkpoints_append_only() RETURNS trigger AS $$
BEGIN
    RAISE EXCEPTION 'audit_checkpoints is append-only';
END
$$ LANGUAGE plpgsql;

CREATE TRIGGER audit_checkpoints_append_only
    BEFORE UPDATE OR DELETE ON audit_checkpoints
    FOR EACH ROW EXECUTE FUNCTION audit_checkpoints_append_only();
CREATE TRIGGER audit_checkpoints_no_truncate
    BEFORE TRUNCATE ON audit_checkpoints
    FOR EACH STATEMENT EXECUTE FUNCTION audit_checkpoints_append_only();

-- audit_events: UPDATE and TRUNCATE stay forbidden (trigger from 0001 and 0004). DELETE is
-- forbidden too, except for a chained row at or below a recorded checkpoint: that is how
-- retention pruning removes whole prefixes of the chain and nothing else.
DROP TRIGGER audit_events_append_only ON audit_events;
CREATE TRIGGER audit_events_append_only
    BEFORE UPDATE ON audit_events
    FOR EACH ROW EXECUTE FUNCTION audit_events_append_only();

CREATE FUNCTION audit_events_guard_delete() RETURNS trigger AS $$
BEGIN
    IF OLD.seq IS NOT NULL
       AND OLD.seq <= COALESCE((SELECT max(through_seq) FROM audit_checkpoints), 0) THEN
        RETURN OLD;
    END IF;
    RAISE EXCEPTION 'audit_events is append-only: only rows covered by a prune checkpoint can be deleted';
END
$$ LANGUAGE plpgsql;

CREATE TRIGGER audit_events_guard_delete
    BEFORE DELETE ON audit_events
    FOR EACH ROW EXECUTE FUNCTION audit_events_guard_delete();
