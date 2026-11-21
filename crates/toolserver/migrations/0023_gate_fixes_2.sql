-- Fixes from the independent re-check of eb24d28 (docs/password-sign-in.md, "Re-check of
-- eb24d28"; docs/entra-sign-in.md, the same section). The bot marketplace's tables take 0024.
--
-- 1. H1 and M2: the work one setup code (and one setup token) can cause is bounded, and
--    guesses against a code are counted against the code itself.
--    - password_pending.password_sets / enrolments: password sets and new authenticator
--      secrets made with one setup token (at most 3 and 5: `password::limits`). Past either,
--      the token is spent and the person starts again with the code.
--    - password_setup_codes.begins / password_sets: setup tokens and password sets made with
--      one code across its renewals (at most 5 and 6). Past either, the code is spent: an admin
--      issues a new one.
--    - password_setup_codes.wrong_guesses / voided_at: wrong codes tried against a person's
--      live code. At 20, the code is void (revoked, and voided_at says why), the admin overview
--      lists it, and a new code starts from zero.
-- 2. M3: an address the person (anyone) fully signed in from is looked up by its key alone.
-- 3. LF: a reset notice's dismissal is a role change (who, when), like the reset itself.
-- 4. M4: the ID tokens already used for a first Entra link, kept until they expire, so a
--    restart doesn't reopen a replay. Only a SHA-256 of the token's (oid, iat) is kept.
-- 5. LG: the two purges' way through their append-only triggers no longer reads a function
--    name from PG_CONTEXT (a crafted, quoted name containing a newline could print the same
--    text). The purge function writes a mark for its own transaction into purge_marks, a table
--    only its owner can write (EXECUTE on the purge functions is revoked from PUBLIC; nothing
--    on purge_marks is granted to anyone), and the trigger looks for that mark. A function of
--    any name, anywhere, that doesn't write the mark can't delete; a role that can write the
--    mark owns the tables and could drop the trigger anyway.

-- 1.
ALTER TABLE password_pending
    ADD COLUMN password_sets integer NOT NULL DEFAULT 0 CHECK (password_sets >= 0),
    ADD COLUMN enrolments integer NOT NULL DEFAULT 0 CHECK (enrolments >= 0);

ALTER TABLE password_setup_codes
    ADD COLUMN begins integer NOT NULL DEFAULT 0 CHECK (begins >= 0),
    ADD COLUMN password_sets integer NOT NULL DEFAULT 0 CHECK (password_sets >= 0),
    ADD COLUMN wrong_guesses integer NOT NULL DEFAULT 0 CHECK (wrong_guesses >= 0),
    ADD COLUMN voided_at timestamptz,
    -- A void code is revoked at the same moment.
    ADD CONSTRAINT password_setup_codes_void_is_revoked
        CHECK (voided_at IS NULL OR revoked_at = voided_at);
CREATE INDEX password_setup_codes_voided ON password_setup_codes (voided_at)
    WHERE voided_at IS NOT NULL;

-- 2.
CREATE INDEX password_source_counters_key ON password_source_counters (source_key);

-- 3.
ALTER TABLE role_changes DROP CONSTRAINT IF EXISTS role_changes_change_check;
ALTER TABLE role_changes ADD CONSTRAINT role_changes_change_check
    CHECK (change IN ('create', 'grant', 'revoke', 'deactivate', 'reactivate', 'link', 'unlink',
                      'import', 'assign', 'unassign', 'set_field', 'confirm_sign_in_name',
                      'suspend_credentials', 'delete_credentials', 'exception_on',
                      'exception_off', 'issue_password_code', 'set_password',
                      'dismiss_reset_notice'));
-- (The overview reads them by `role_changes_recent (change, at DESC)`, from 0015.)

-- 4.
CREATE TABLE entra_used_tokens (
    token_key  text PRIMARY KEY CHECK (token_key ~ '^[0-9a-f]{64}$'),
    used_at    timestamptz NOT NULL,
    expires_at timestamptz NOT NULL CHECK (expires_at <= used_at + interval '1 day')
);
CREATE INDEX entra_used_tokens_expires ON entra_used_tokens (expires_at);

-- 5.
CREATE TABLE purge_marks (
    txid bigint NOT NULL,
    kind text NOT NULL CHECK (kind IN ('audit', 'role_changes')),
    PRIMARY KEY (txid, kind)
);
REVOKE ALL ON purge_marks FROM PUBLIC;

DO $migration$
DECLARE
    s text := current_schema();
BEGIN
    -- 5a. The audit purge (0013 §5, 0021 §1a), unchanged in what it does; it marks its own
    -- transaction while it deletes.
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
    INSERT INTO %1$I.purge_marks (txid, kind) VALUES (txid_current(), 'audit')
        ON CONFLICT DO NOTHING;
    WITH doomed AS (
        SELECT id FROM %1$I.audit_events
        WHERE at < v_cutoff
        ORDER BY at, id
        LIMIT batch_size
        FOR UPDATE SKIP LOCKED
    )
    DELETE FROM %1$I.audit_events a USING doomed d WHERE a.id = d.id;
    GET DIAGNOSTICS v_deleted = ROW_COUNT;
    DELETE FROM %1$I.purge_marks WHERE txid = txid_current() AND kind = 'audit';
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

    -- 5b. Its trigger: a DELETE of an old row goes only inside a marked transaction.
    EXECUTE format($f$
CREATE OR REPLACE FUNCTION %1$I.audit_events_append_only() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' AND TG_LEVEL = 'ROW' THEN
        IF OLD.at < now() - interval '90 days'
           AND current_setting('pmos.audit_purge', true) = 'on'
           AND EXISTS (SELECT 1 FROM %1$I.purge_marks
                       WHERE txid = txid_current() AND kind = 'audit') THEN
            RETURN OLD;
        END IF;
    END IF;
    RAISE EXCEPTION 'audit_events is append-only' USING ERRCODE = 'insufficient_privilege';
END
$$
$f$, s);

    -- 5c. The role-history purge (0015 §2, 0021 §1c), unchanged in what it does.
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
    INSERT INTO %1$I.purge_marks (txid, kind) VALUES (txid_current(), 'role_changes')
        ON CONFLICT DO NOTHING;
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
    DELETE FROM %1$I.purge_marks WHERE txid = txid_current() AND kind = 'role_changes';
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

    -- 5d. Its trigger, likewise.
    EXECUTE format($f$
CREATE OR REPLACE FUNCTION %1$I.role_changes_append_only() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' AND TG_LEVEL = 'ROW'
       AND current_setting('pmos.role_changes_retention', true) = 'on'
       AND OLD.target <> '*'
       AND EXISTS (SELECT 1 FROM %1$I.users u
                   WHERE u.handle = OLD.target AND u.status = 'deactivated'
                     AND u.deactivated_at < now() - interval '90 days')
       AND EXISTS (SELECT 1 FROM %1$I.purge_marks
                   WHERE txid = txid_current() AND kind = 'role_changes') THEN
        RETURN OLD;
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
