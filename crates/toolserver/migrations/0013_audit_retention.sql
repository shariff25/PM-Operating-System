-- Audit retention (docs/audit-retention.md): events older than 90 days are deleted, with no
-- archive, by one controlled job, and that job records its own run in the log.
--
-- The guarantee that nothing newer than 90 days can be deleted is kept by the database, not by
-- the tool server's code. The pieces, and why each is there:
--
-- 1. The append-only triggers stay (0001 for UPDATE and DELETE, 0004 for TRUNCATE). Their
--    function is replaced below with one that has exactly one way through: a DELETE of a row
--    older than 90 days, while the purge function is running. UPDATE and TRUNCATE are refused
--    for everyone, always.
--
-- 2. The trigger re-checks each row's age itself (`OLD.at < now() - interval '90 days'`), with
--    the 90 days written into the trigger, not passed in. So whatever a caller does (calls the
--    purge function with a short retention, forges the setting in 3, writes its own function),
--    a row younger than 90 days cannot be deleted: the trigger raises and the whole statement,
--    and its transaction, fails. `now()` is the transaction's start time, the same instant the
--    purge function computes its cutoff from, so the two checks agree.
--
-- 3. A transaction-local setting, `pmos.audit_purge`, that only the purge function sets (and
--    clears again before it returns). A plain `DELETE FROM audit_events`, even of old rows, is
--    refused without it, so deleting goes through the function and is therefore recorded (5).
--    Anyone can `SET` a custom setting, so this alone proves nothing; that is why 2 and 4 exist.
--
-- 4. The trigger also checks the call stack (`PG_CONTEXT`): the DELETE must come from inside
--    `purge_audit_events(interval,integer)`. Defence in depth: it stops a hand-typed
--    `SET pmos.audit_purge = 'on'; DELETE …` from deleting even old rows unrecorded. It is not
--    the age guarantee (2 is), since a role that can create functions could name one the same.
--
-- 5. `purge_audit_events(retention, batch_size)`, SECURITY DEFINER:
--    a. refuses (raises) a retention shorter than 90 days, a hard floor written into the
--       function; the tool server also refuses to start with less (PMOS_AUDIT_RETENTION_DAYS);
--    b. deletes only rows with `at` before its cutoff, the older of `now() - retention` and
--       `now() - 90 days` (so a retention in months, which can be shorter than 90 days in
--       February, never moves the cutoff later), oldest first, at most `batch_size` (1 to
--       100000) per call: the job calls again while `more` is true;
--    c. inserts one audit event for its own run in the same transaction (tool `audit.purge`,
--       client `system`, the count deleted and the cutoff; no content), so a purge that isn't
--       recorded didn't happen;
--    d. takes a transaction-scoped advisory lock first, and returns `ran = false` without
--       deleting or recording if another purge holds it, and picks rows `FOR UPDATE SKIP
--       LOCKED`: two replicas running at once neither double-delete nor deadlock;
--    e. runs with a fixed search_path (this schema, then pg_temp last), as SECURITY DEFINER
--       functions should, so a caller's objects can't stand in for `audit_events`.
--
-- 6. EXECUTE on the function is revoked from PUBLIC. In deploy/ (postgres-init.sh) each
--    service has its own database and login role, `pmos_toolserver` owns this database and
--    runs these migrations, and no other role can connect to it (`REVOKE ALL ON DATABASE …
--    FROM PUBLIC`), so the tool server's role is the only one that can call it.
--    What the database cannot stop: the role that OWNS audit_events can drop or disable the
--    triggers (DDL). That role is the tool server's own, which runs these migrations; separating
--    a migration owner from the runtime role would close that, and is not done here
--    (docs/audit-retention.md, "Limits").
--
-- A `role_changes` table (docs/user-database-spec.md) is never touched: the function names
-- `audit_events` only.

-- The purge records its run as client `system`: the tool server itself, not a person's client.
ALTER TABLE audit_events DROP CONSTRAINT IF EXISTS audit_events_client_check;
ALTER TABLE audit_events ADD CONSTRAINT audit_events_client_check
    CHECK (client IN ('mcp', 'orchestrator', 'bot', 'web', 'console', 'system'));

-- The overview's "last purge": the newest `audit.purge` event, without scanning the log.
CREATE INDEX audit_events_purges ON audit_events (at DESC, id DESC) WHERE tool = 'audit.purge';

CREATE OR REPLACE FUNCTION audit_events_append_only() RETURNS trigger AS $$
DECLARE
    stack text;
BEGIN
    -- The one exception: a row-level DELETE (never UPDATE, never TRUNCATE) ...
    IF TG_OP = 'DELETE' AND TG_LEVEL = 'ROW' THEN
        -- ... of a row older than 90 days, checked here whatever the caller asked for (2) ...
        IF OLD.at < now() - interval '90 days'
           -- ... while the purge function says it is purging (3) ...
           AND current_setting('pmos.audit_purge', true) = 'on' THEN
            -- ... and the DELETE runs inside the purge function (4).
            GET DIAGNOSTICS stack = PG_CONTEXT;
            IF position(E'\nPL/pgSQL function purge_audit_events(interval,integer) line ' IN stack) > 0 THEN
                RETURN OLD;
            END IF;
        END IF;
    END IF;
    RAISE EXCEPTION 'audit_events is append-only' USING ERRCODE = 'insufficient_privilege';
END
$$ LANGUAGE plpgsql;

CREATE FUNCTION purge_audit_events(retention interval, batch_size integer)
RETURNS TABLE (deleted bigint, cutoff timestamptz, more boolean, ran boolean)
LANGUAGE plpgsql SECURITY DEFINER AS $$
DECLARE
    -- The hard floor (5a). The trigger has its own copy (2).
    min_retention CONSTANT interval := interval '90 days';
    v_cutoff timestamptz;
    v_deleted bigint;
    v_more boolean;
BEGIN
    IF retention IS NULL OR retention < min_retention THEN
        RAISE EXCEPTION 'audit retention must be at least 90 days'
            USING ERRCODE = 'invalid_parameter_value';
    END IF;
    IF batch_size IS NULL OR batch_size < 1 OR batch_size > 100000 THEN
        RAISE EXCEPTION 'batch_size must be from 1 to 100000'
            USING ERRCODE = 'invalid_parameter_value';
    END IF;
    -- One purge at a time per schema (5d).
    IF NOT pg_try_advisory_xact_lock(hashtext('pmos.audit_purge'), hashtext(current_schema())) THEN
        RETURN QUERY SELECT 0::bigint, NULL::timestamptz, false, false;
        RETURN;
    END IF;
    v_cutoff := least(now() - retention, now() - min_retention);
    PERFORM set_config('pmos.audit_purge', 'on', true);
    WITH doomed AS (
        SELECT id FROM audit_events
        WHERE at < v_cutoff
        ORDER BY at, id
        LIMIT batch_size
        FOR UPDATE SKIP LOCKED
    )
    DELETE FROM audit_events a USING doomed d WHERE a.id = d.id;
    GET DIAGNOSTICS v_deleted = ROW_COUNT;
    PERFORM set_config('pmos.audit_purge', 'off', true);
    v_more := EXISTS (SELECT 1 FROM audit_events WHERE at < v_cutoff);
    -- Its own run, in this transaction (5c): metadata only.
    INSERT INTO audit_events (at, run_id, user_handle, skill, tool, outcome, sources, client, detail)
    VALUES (
        now(), 'retention', 'pmos-retention', NULL, 'audit.purge', 'ok', '{}', 'system',
        format('purged %s events older than %s', v_deleted,
               to_char(v_cutoff AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"'))
    );
    RETURN QUERY SELECT v_deleted, v_cutoff, v_more, true;
END
$$;

-- 5e: this schema first, pg_temp last. Written with the schema the migration runs in (the
-- tests give each case its own schema).
DO $$
BEGIN
    EXECUTE format(
        'ALTER FUNCTION purge_audit_events(interval, integer) SET search_path = %I, pg_temp',
        current_schema()
    );
END
$$;

-- 6: only the owner (the tool server's role) may call it.
REVOKE ALL ON FUNCTION purge_audit_events(interval, integer) FROM PUBLIC;
