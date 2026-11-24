-- Slice U5 of the user database (docs/user-database-spec.md §8): which PMs each Reviewer is
-- assigned to, set by an admin in the web app (PUT /internal/admin/reviewers/{reviewer}).
-- U1 left this table to U6; U5 builds the assignments (U6 builds the recommendations, which
-- read them). 0016 is left free for the B1 fix, which is in flight.
--
-- What the database keeps:
-- 1. A row names two users (never deleted: users are deactivated), and never one person as
--    their own reviewer.
-- 2. Every change bumps the directory's version (the ETag the admin screens send back as
--    `expected_version`), under the users lock and in a READ COMMITTED transaction, as
--    users_invariants() does for the users tables (migration 0015).
-- 3. TRUNCATE is refused, as on the other users tables.
--
-- Who may be assigned (an active reviewer, active PMs) is the application's rule, checked
-- when an assignment is made; an assignment whose reviewer later loses the role, or either
-- person is deactivated, is kept and grants nothing.

-- A fix to U3 found here: a person's own first Atlassian or GitHub link (the OAuth callback,
-- `Op::LinkAccount`) records its role change through client `link`, which migration 0015's
-- check didn't allow, so in Postgres every first link of those accounts was refused. The
-- identities table already allows `link`; role_changes now does too.
ALTER TABLE role_changes DROP CONSTRAINT IF EXISTS role_changes_client_check;
ALTER TABLE role_changes ADD CONSTRAINT role_changes_client_check
    CHECK (client IN ('web', 'bot', 'host', 'import', 'system', 'link'));

CREATE TABLE reviewer_assignments (
    reviewer    text NOT NULL REFERENCES users ON DELETE RESTRICT,
    pm          text NOT NULL REFERENCES users ON DELETE RESTRICT,
    assigned_by text NOT NULL,
    assigned_at timestamptz NOT NULL,
    PRIMARY KEY (reviewer, pm),
    CHECK (reviewer <> pm)
);
CREATE INDEX reviewer_assignments_pm ON reviewer_assignments (pm);

CREATE FUNCTION reviewer_assignments_version() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    PERFORM pg_advisory_xact_lock(hashtext('pmos.users'), hashtext(current_schema()));
    IF current_setting('transaction_isolation') <> 'read committed' THEN
        RAISE EXCEPTION 'users_isolation: change reviewer assignments in a READ COMMITTED transaction (it is %)',
            current_setting('transaction_isolation')
            USING ERRCODE = 'check_violation';
    END IF;
    UPDATE directory_version SET v = v + 1 WHERE one;
    RETURN NULL;
END
$$;

CREATE CONSTRAINT TRIGGER reviewer_assignments_version
    AFTER INSERT OR UPDATE OR DELETE ON reviewer_assignments
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION reviewer_assignments_version();
CREATE TRIGGER reviewer_assignments_no_truncate BEFORE TRUNCATE ON reviewer_assignments
    FOR EACH STATEMENT EXECUTE FUNCTION users_keep_rows();

DO $$
BEGIN
    EXECUTE format('ALTER FUNCTION reviewer_assignments_version() SET search_path = %I, pg_temp',
                   current_schema());
END
$$;
