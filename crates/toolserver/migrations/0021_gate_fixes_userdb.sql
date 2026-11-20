-- Fixes from the independent review of b469d5b (docs/user-database-spec.md §12, "Review of
-- b469d5b"): UM5 (the two append-only exceptions) and L8 (addresses are never reused).
--
-- A note on numbering (L13): migration 0016 was never used and never will be. 0018's and
-- 0020's header comments still say it is "left free for the B1 fix"; B1 landed earlier, as
-- 0012_approval_draft.sql. Those comments are not edited, because changing an applied
-- migration changes its checksum and stops existing databases from starting.
--
-- 1. UM5. The audit and role-history purges each have one way through their table's
--    append-only trigger: a DELETE made from inside the purge function, which the trigger
--    recognises in the call stack (PG_CONTEXT). Before this migration the trigger looked for
--    the function's bare name, `purge_audit_events(interval,integer)` /
--    `purge_role_changes(integer)`, and `audit_events_append_only()` ran with the caller's
--    search_path. A role that can create functions could put a same-named function in
--    another schema first on its search_path; PL/pgSQL prints a frame's name unqualified
--    when the function is visible on the search_path it was compiled under, so that frame
--    read the same as the real one. (The age check inside the trigger still held: nothing
--    younger than 90 days could go. What it opened was an unrecorded delete of old rows.)
--
--    Now the purge functions and both triggers run with search_path `pg_catalog, pg_temp`:
--    the schema PM OS's tables live in is not on it, so every table is named with its schema
--    (written in below, per schema: the tests give each case its own), and the real purge
--    function is not visible on its own search_path, so its frame always prints with its
--    schema: `<schema>.purge_audit_events(interval,integer)`. The triggers look for exactly
--    that, with the schema of the table they fire on. A function of the same name in any
--    other schema prints as `<other>.purge_…` or bare, and never matches. The advisory lock
--    keeps its key, `(hashtext('pmos.audit_purge'), hashtext(<schema>))`, with the schema
--    written in instead of `current_schema()` (which is `pg_catalog` on the new path).
--
-- 2. L8. Handles and addresses are never reused (§1): an admin's or the host's change of a
--    user's sign-in name, Atlassian email or GitHub login no longer frees the old value for
--    someone else. `retired_addresses` keeps every value a user had (append-only), and a
--    trigger on users refuses a value another user had, as the unique indexes refuse one
--    another user has. The same user may have an old value of their own back.

DO $migration$
DECLARE
    s text := current_schema();
BEGIN
    -- 1a. The audit purge (0013 §5), unchanged in what it does.
    EXECUTE format($f$
CREATE OR REPLACE FUNCTION %1$I.purge_audit_events(retention interval, batch_size integer)
RETURNS TABLE (deleted bigint, cutoff timestamptz, more boolean, ran boolean)
LANGUAGE plpgsql SECURITY DEFINER AS $$
DECLARE
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
    IF NOT pg_try_advisory_xact_lock(hashtext('pmos.audit_purge'), hashtext(%2$L)) THEN
        RETURN QUERY SELECT 0::bigint, NULL::timestamptz, false, false;
        RETURN;
    END IF;
    v_cutoff := least(now() - retention, now() - min_retention);
    PERFORM set_config('pmos.audit_purge', 'on', true);
    WITH doomed AS (
        SELECT id FROM %1$I.audit_events
        WHERE at < v_cutoff
        ORDER BY at, id
        LIMIT batch_size
        FOR UPDATE SKIP LOCKED
    )
    DELETE FROM %1$I.audit_events a USING doomed d WHERE a.id = d.id;
    GET DIAGNOSTICS v_deleted = ROW_COUNT;
    PERFORM set_config('pmos.audit_purge', 'off', true);
    v_more := EXISTS (SELECT 1 FROM %1$I.audit_events WHERE at < v_cutoff);
    INSERT INTO %1$I.audit_events (at, run_id, user_handle, skill, tool, outcome, sources, client, detail)
    VALUES (
        now(), 'retention', 'pmos-retention', NULL, 'audit.purge', 'ok', '{}', 'system',
        format('purged %%s events older than %%s', v_deleted,
               to_char(v_cutoff AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"'))
    );
    RETURN QUERY SELECT v_deleted, v_cutoff, v_more, true;
END
$$
$f$, s, s);

    -- 1b. Its trigger: the frame it accepts is the purge function with its schema.
    EXECUTE format($f$
CREATE OR REPLACE FUNCTION %1$I.audit_events_append_only() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    stack text;
BEGIN
    IF TG_OP = 'DELETE' AND TG_LEVEL = 'ROW' THEN
        IF OLD.at < now() - interval '90 days'
           AND current_setting('pmos.audit_purge', true) = 'on' THEN
            GET DIAGNOSTICS stack = PG_CONTEXT;
            IF position(E'\nPL/pgSQL function ' || quote_ident(TG_TABLE_SCHEMA)
                        || '.purge_audit_events(interval,integer) line ' IN stack) > 0 THEN
                RETURN OLD;
            END IF;
        END IF;
    END IF;
    RAISE EXCEPTION 'audit_events is append-only' USING ERRCODE = 'insufficient_privilege';
END
$$
$f$, s);

    -- 1c. The role-history purge (0015 §2), unchanged in what it does.
    EXECUTE format($f$
CREATE OR REPLACE FUNCTION %1$I.purge_role_changes(batch_size integer)
RETURNS TABLE (deleted bigint, more boolean)
LANGUAGE plpgsql SECURITY DEFINER AS $$
DECLARE
    v_deleted bigint;
    v_more boolean;
BEGIN
    IF batch_size IS NULL OR batch_size < 1 OR batch_size > 100000 THEN
        RAISE EXCEPTION 'batch_size must be from 1 to 100000' USING ERRCODE = 'invalid_parameter_value';
    END IF;
    PERFORM set_config('pmos.role_changes_retention', 'on', true);
    WITH doomed AS (
        SELECT c.id FROM %1$I.role_changes c JOIN %1$I.users u ON u.handle = c.target
        WHERE c.target <> '*' AND u.status = 'deactivated'
          AND u.deactivated_at < now() - interval '90 days'
        ORDER BY c.id
        LIMIT batch_size
        FOR UPDATE OF c SKIP LOCKED
    )
    DELETE FROM %1$I.role_changes c USING doomed d WHERE c.id = d.id;
    GET DIAGNOSTICS v_deleted = ROW_COUNT;
    PERFORM set_config('pmos.role_changes_retention', 'off', true);
    v_more := EXISTS (SELECT 1 FROM %1$I.role_changes c JOIN %1$I.users u ON u.handle = c.target
                      WHERE c.target <> '*' AND u.status = 'deactivated'
                        AND u.deactivated_at < now() - interval '90 days');
    INSERT INTO %1$I.audit_events (at, run_id, user_handle, skill, tool, outcome, sources, client, detail)
    VALUES (now(), 'retention', 'pmos-retention', NULL, 'users.role_changes_purge', 'ok', '{}',
            'system', format('purged %%s role changes', v_deleted));
    RETURN QUERY SELECT v_deleted, v_more;
END
$$
$f$, s);

    -- 1d. Its trigger, likewise.
    EXECUTE format($f$
CREATE OR REPLACE FUNCTION %1$I.role_changes_append_only() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    stack text;
BEGIN
    IF TG_OP = 'DELETE' AND TG_LEVEL = 'ROW'
       AND current_setting('pmos.role_changes_retention', true) = 'on'
       AND OLD.target <> '*'
       AND EXISTS (SELECT 1 FROM %1$I.users u
                   WHERE u.handle = OLD.target AND u.status = 'deactivated'
                     AND u.deactivated_at < now() - interval '90 days') THEN
        GET DIAGNOSTICS stack = PG_CONTEXT;
        IF position(E'\nPL/pgSQL function ' || quote_ident(TG_TABLE_SCHEMA)
                    || '.purge_role_changes(integer) line ' IN stack) > 0 THEN
            RETURN OLD;
        END IF;
    END IF;
    RAISE EXCEPTION 'role_changes is append-only' USING ERRCODE = 'insufficient_privilege';
END
$$
$f$, s);

    EXECUTE format('ALTER FUNCTION %I.purge_audit_events(interval, integer) SET search_path = pg_catalog, pg_temp', s);
    EXECUTE format('ALTER FUNCTION %I.audit_events_append_only() SET search_path = pg_catalog, pg_temp', s);
    EXECUTE format('ALTER FUNCTION %I.purge_role_changes(integer) SET search_path = pg_catalog, pg_temp', s);
    EXECUTE format('ALTER FUNCTION %I.role_changes_append_only() SET search_path = pg_catalog, pg_temp', s);
END
$migration$;

REVOKE ALL ON FUNCTION purge_audit_events(interval, integer) FROM PUBLIC;
REVOKE ALL ON FUNCTION purge_role_changes(integer) FROM PUBLIC;

-- 2. L8: every address a user had, kept for good.
CREATE TABLE retired_addresses (
    kind       text NOT NULL CHECK (kind IN ('sign_in_name', 'atlassian_email', 'github_login')),
    -- As the unique indexes compare it: sign-in names and emails are stored lower-case; a
    -- GitHub login is kept lower-cased.
    address    text NOT NULL CHECK (address = lower(address) AND address <> ''),
    handle     text NOT NULL REFERENCES users ON DELETE RESTRICT,
    retired_at timestamptz NOT NULL,
    PRIMARY KEY (kind, address)
);

CREATE FUNCTION users_addresses_never_reused() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    k text;
    old_v text;
    new_v text;
    holder text;
BEGIN
    FOREACH k IN ARRAY ARRAY['sign_in_name', 'atlassian_email', 'github_login'] LOOP
        new_v := CASE k WHEN 'sign_in_name' THEN NEW.sign_in_name
                        WHEN 'atlassian_email' THEN NEW.atlassian_email
                        ELSE lower(NEW.github_login) END;
        IF TG_OP = 'UPDATE' THEN
            old_v := CASE k WHEN 'sign_in_name' THEN OLD.sign_in_name
                            WHEN 'atlassian_email' THEN OLD.atlassian_email
                            ELSE lower(OLD.github_login) END;
            IF old_v IS NOT NULL AND old_v IS DISTINCT FROM new_v THEN
                INSERT INTO retired_addresses (kind, address, handle, retired_at)
                VALUES (k, old_v, OLD.handle, now())
                ON CONFLICT (kind, address) DO NOTHING;
            END IF;
        END IF;
        IF new_v IS NOT NULL THEN
            SELECT r.handle INTO holder FROM retired_addresses r
             WHERE r.kind = k AND r.address = new_v;
            IF holder IS NOT NULL AND holder <> NEW.handle THEN
                RAISE EXCEPTION 'taken: that % was another user''s (addresses are never reused)',
                    replace(k, '_', ' ')
                    USING ERRCODE = 'check_violation';
            END IF;
        END IF;
    END LOOP;
    RETURN NEW;
END
$$;
CREATE TRIGGER users_addresses_never_reused
    BEFORE INSERT OR UPDATE OF sign_in_name, atlassian_email, github_login ON users
    FOR EACH ROW EXECUTE FUNCTION users_addresses_never_reused();

CREATE FUNCTION retired_addresses_keep() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'retired_addresses is append-only' USING ERRCODE = 'insufficient_privilege';
END
$$;
CREATE TRIGGER retired_addresses_append_only
    BEFORE UPDATE OR DELETE ON retired_addresses
    FOR EACH ROW EXECUTE FUNCTION retired_addresses_keep();
CREATE TRIGGER retired_addresses_no_truncate
    BEFORE TRUNCATE ON retired_addresses
    FOR EACH STATEMENT EXECUTE FUNCTION retired_addresses_keep();

DO $$
BEGIN
    EXECUTE format('ALTER FUNCTION users_addresses_never_reused() SET search_path = %I, pg_temp', current_schema());
    EXECUTE format('ALTER FUNCTION retired_addresses_keep() SET search_path = %I, pg_temp', current_schema());
END
$$;
