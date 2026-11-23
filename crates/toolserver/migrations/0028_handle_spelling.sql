-- Finding U3 of the review at c78b5fb: handles that spell the same.
--
-- A personal bot's name and the model gateway's per-person caps write a handle in its spelled
-- form (`.` and `_` as `-`, a run of hyphens as one, no hyphen at either end:
-- `spelled_handle` in crates/contract/src/onboarding.rs), so `ana.k`, `ana_k` and `ana-k` would
-- share one bot name and one person's model slots. Since the review at 6bb806f the
-- application refuses to create such a handle (rules.rs, `Builder::spelling_clash`), but the
-- database did not, so raw SQL could still add one. Now `users_invariants()` refuses it too:
--
-- d. a user inserted (or a handle changed) so that its spelled form is another user's, active
--    or deactivated, is refused `exists`. Only the inserted or renamed row is checked, so users
--    that already clash (made before the application's check) are left as they are and can
--    still be changed, deactivated and reactivated; the user list warns about them
--    (`Directory::spelling_clashes`).
--
-- The function is otherwise migration 0015's, unchanged; migration 0015 itself is not edited.

-- The spelled form, as `pmos_contract::onboarding::spelled_handle` writes it.
CREATE FUNCTION spelled_handle(handle text) RETURNS text
    LANGUAGE sql IMMUTABLE STRICT PARALLEL SAFE AS $$
    SELECT trim(BOTH '-' FROM regexp_replace(translate(handle, '._', '--'), '-+', '-', 'g'))
$$;

CREATE OR REPLACE FUNCTION users_invariants() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    touched text[];
    exception_open boolean;
    offenders text;
    clash text;
BEGIN
    -- First: the lock. Re-entrant, so the application may (and does) take it first too.
    PERFORM pg_advisory_xact_lock(hashtext('pmos.users'), hashtext(current_schema()));
    -- Under REPEATABLE READ or SERIALIZABLE the checks below would read the snapshot taken
    -- at the transaction's start, blind to a change that committed while this one waited
    -- for the lock.
    IF current_setting('transaction_isolation') <> 'read committed' THEN
        RAISE EXCEPTION 'users_isolation: change users in a READ COMMITTED transaction (it is %)',
            current_setting('transaction_isolation')
            USING ERRCODE = 'check_violation';
    END IF;

    -- a. The last admin.
    IF EXISTS (SELECT 1 FROM users)
       AND NOT EXISTS (SELECT 1 FROM users u JOIN user_roles r ON r.handle = u.handle
                       WHERE u.status = 'active' AND r.role = 'admin') THEN
        RAISE EXCEPTION 'last_admin: PM OS needs at least one active admin'
            USING ERRCODE = 'check_violation';
    END IF;

    -- Who this change touched: the row's person, or everyone for a pilot exception change.
    IF TG_TABLE_NAME = 'pilot_exceptions' THEN
        touched := ARRAY(SELECT handle FROM users);
    ELSE
        touched := array_remove(ARRAY[OLD.handle, NEW.handle], NULL);
    END IF;

    -- b. Admin and Auditor together, only while the pilot exception is open.
    exception_open := EXISTS (SELECT 1 FROM pilot_exceptions
                              WHERE name = 'admin_and_auditor' AND ended_at IS NULL
                                AND expires_at > now());
    IF NOT exception_open THEN
        SELECT string_agg(u.handle, ', ' ORDER BY u.handle) INTO offenders
        FROM users u
        WHERE u.handle = ANY (touched) AND u.status = 'active'
          AND EXISTS (SELECT 1 FROM user_roles r WHERE r.handle = u.handle AND r.role = 'admin')
          AND EXISTS (SELECT 1 FROM user_roles r WHERE r.handle = u.handle AND r.role = 'auditor');
        IF offenders IS NOT NULL THEN
            RAISE EXCEPTION 'dual_role: % would hold admin and auditor with no open pilot exception',
                offenders
                USING ERRCODE = 'check_violation';
        END IF;
    END IF;

    -- c. The evals account: pm only, no identities.
    IF 'evals' = ANY (touched) THEN
        IF EXISTS (SELECT 1 FROM user_roles WHERE handle = 'evals' AND role <> 'pm') THEN
            RAISE EXCEPTION 'evals_account: the evals account may hold pm only'
                USING ERRCODE = 'check_violation';
        END IF;
        IF EXISTS (SELECT 1 FROM user_identities WHERE handle = 'evals') THEN
            RAISE EXCEPTION 'evals_account: the evals account never has a Slack, Entra, Atlassian or GitHub key'
                USING ERRCODE = 'check_violation';
        END IF;
    END IF;

    -- d. A new handle (or a changed one) spelled as another user's (U3). Read after the lock,
    -- so two transactions adding `ana.k` and `ana_k` at once can't both commit. Nested, not
    -- one condition: PL/pgSQL may evaluate every part of an AND, and NEW of another table has
    -- no `handle` to compare (pilot_exceptions) or isn't a rename (user_roles).
    IF TG_TABLE_NAME = 'users' AND TG_OP IN ('INSERT', 'UPDATE') THEN
        IF TG_OP = 'INSERT' OR NEW.handle IS DISTINCT FROM OLD.handle THEN
            SELECT string_agg(u.handle, ', ' ORDER BY u.handle) INTO clash
            FROM users u
            WHERE u.handle <> NEW.handle
              AND spelled_handle(u.handle) = spelled_handle(NEW.handle)
              -- The row as committed now: a later statement may have renamed it again.
              AND EXISTS (SELECT 1 FROM users n WHERE n.handle = NEW.handle);
            IF clash IS NOT NULL THEN
                RAISE EXCEPTION 'exists: `%` is spelled `%` in a bot''s name and by the model gateway, as `%` already is: pick a handle that differs by more than `.`, `_` and `-`',
                    NEW.handle, spelled_handle(NEW.handle), clash
                    USING ERRCODE = 'check_violation';
            END IF;
        END IF;
    END IF;

    -- The directory's version (the ETag, §2), bumped here, after the lock, so concurrent
    -- writers don't queue on its row before they reach the checks above.
    UPDATE directory_version SET v = v + 1 WHERE one;
    RETURN NULL;
END
$$;

-- A fixed search_path for both functions, as migration 0015 gives every function there:
-- CREATE OR REPLACE drops the one users_invariants() had, so it is set again here.
DO $$
BEGIN
    EXECUTE format('ALTER FUNCTION users_invariants() SET search_path = %I, pg_temp', current_schema());
    EXECUTE format('ALTER FUNCTION spelled_handle(text) SET search_path = %I, pg_temp', current_schema());
END
$$;
