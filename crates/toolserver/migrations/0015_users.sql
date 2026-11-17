-- The user database (docs/user-database-spec.md §1): who exists, their status, roles and keys.
-- Slice U1 ("Store"): nothing reads these tables for a decision yet; content/team.yaml stays
-- what the services read until slices U3-U7 cut them over.
--
-- The guarantees are the database's own, not the application's:
--
-- 1. `users_invariants()`, a DEFERRED constraint trigger on users, user_roles, user_identities
--    and pilot_exceptions, runs at commit. Its first statement takes a transaction-scoped
--    advisory lock, so two transactions that change users are checked one after the other,
--    whatever the application does. It refuses any isolation level but READ COMMITTED, so
--    every check after the lock reads a fresh snapshot that includes whatever committed while
--    it waited. Then:
--    a. the last admin: users in the table but no `active` user holding `admin` is refused;
--    b. Admin and Auditor on one active person is refused unless a pilot exception is open
--       and not past `expires_at`. The check is on the people this transaction touched (for a
--       change to pilot_exceptions, everyone), so a lapsed exception blocks changes to the
--       person holding both, but never the changes that resolve it: revoking either role,
--       deactivating them, or `off --revoke-auditor`;
--    c. the `evals` account holds `pm` only and never a row in user_identities.
-- 2. `role_changes` is append-only by its own trigger, `role_changes_append_only()`, which
--    knows nothing of the audit purge's `pmos.audit_purge` setting. Its one exception: a row
--    DELETE, inside `purge_role_changes(integer)`, with `pmos.role_changes_retention` on, of a
--    row whose target is a user deactivated more than 90 days ago (never `*`).
-- 3. Users are never deleted (no erasure in the MVP); TRUNCATE is refused on every table here.
-- 4. Every function has a fixed search_path (this schema, then pg_temp), so a session's
--    search_path can't point a trigger at other tables or change the lock's key.
--
-- What the database can't stop: the role that owns these tables can drop or disable the
-- triggers (DDL). That role is the tool server's own, as for audit_events (migration 0013).
--
-- Not here, by design: reviewer_assignments and approval_recommendations (§8) come with slice
-- U6, which depends on the B1 fix's approval revision; credentials.suspended_at comes with U3.

-- Host commands record their audit events as client `host` (§10).
ALTER TABLE audit_events DROP CONSTRAINT IF EXISTS audit_events_client_check;
ALTER TABLE audit_events ADD CONSTRAINT audit_events_client_check
    CHECK (client IN ('mcp', 'orchestrator', 'bot', 'web', 'console', 'system', 'host'));

CREATE TABLE users (
    handle        text PRIMARY KEY
                  CHECK (handle ~ '^[a-z0-9][a-z0-9._-]{0,63}$' AND handle NOT LIKE 'bots.%'
                         -- Actor names in role_changes, never a person.
                         AND handle NOT IN ('host', 'import', 'system')),
    -- Untrusted (often from Slack): cleaned on write, rendered as plain text (§10).
    display_name  text NOT NULL DEFAULT ''
                  CHECK (length(display_name) <= 100 AND display_name !~ '[[:cntrl:]<>]'),
    -- The Entra sign-in name (UPN) a first link must match (§7). Not the Atlassian email.
    sign_in_name  text CHECK (sign_in_name = lower(sign_in_name) AND length(sign_in_name) <= 254),
    -- Set when an admin (web) or the host confirmed sign_in_name; NULL for a value that came
    -- through the Slack bot client (§4). An unconfirmed name never links.
    sign_in_name_confirmed_at timestamptz,
    sign_in_name_confirmed_by text,
    -- What an Atlassian link must match until the first link; after it, the account id in
    -- user_identities decides (§6).
    atlassian_email text CHECK (atlassian_email = lower(atlassian_email)
                                AND length(atlassian_email) <= 254),
    -- What a GitHub link must match until the first link (a login); then the numeric id.
    github_login  text CHECK (github_login ~ '^[A-Za-z0-9-]{1,39}$'),
    status        text NOT NULL CHECK (status IN ('active', 'deactivated')),
    created_at    timestamptz NOT NULL,
    created_by    text NOT NULL,           -- a handle, or 'import' / 'host'
    created_client text NOT NULL CHECK (created_client IN ('web', 'bot', 'host', 'import')),
    deactivated_at timestamptz,
    deactivated_by text,
    CHECK ((sign_in_name_confirmed_at IS NULL) = (sign_in_name_confirmed_by IS NULL)),
    -- The retention rule for role_changes reads deactivated_at: it is set exactly while the
    -- user is deactivated.
    CHECK ((status = 'deactivated') = (deactivated_at IS NOT NULL)),
    CHECK ((deactivated_at IS NULL) = (deactivated_by IS NULL))
);
-- Unique across every user, active or not, so a deactivated person's address is never
-- silently re-used for a first link.
CREATE UNIQUE INDEX users_sign_in_name ON users (sign_in_name) WHERE sign_in_name IS NOT NULL;
CREATE UNIQUE INDEX users_atlassian_email ON users (atlassian_email)
    WHERE atlassian_email IS NOT NULL;
CREATE UNIQUE INDEX users_github_login ON users (lower(github_login))
    WHERE github_login IS NOT NULL;

CREATE TABLE user_roles (
    handle     text NOT NULL REFERENCES users ON DELETE RESTRICT,
    role       text NOT NULL CHECK (role IN ('admin', 'pm', 'reviewer', 'auditor')),
    granted_at timestamptz NOT NULL,
    granted_by text NOT NULL,
    PRIMARY KEY (handle, role)
);
CREATE INDEX user_roles_role ON user_roles (role);

-- Keys that point at a person and were proven by a sign-in or an OAuth link (or set on the host).
CREATE TABLE user_identities (
    handle    text NOT NULL REFERENCES users ON DELETE RESTRICT,
    kind      text NOT NULL CHECK (kind IN ('slack', 'entra', 'atlassian', 'github')),
    issuer    text NOT NULL DEFAULT '',   -- entra: tid; slack: workspace id; else ''
    subject   text NOT NULL CHECK (subject <> ''),
                                          -- slack member id; entra oid; Atlassian account id;
                                          -- GitHub numeric user id
    linked_at timestamptz NOT NULL,
    linked_by text NOT NULL,
    linked_client text NOT NULL CHECK (linked_client IN ('web', 'bot', 'host', 'import', 'link')),
    -- entra only: true when the host set or confirmed it (§7: required for Admin and Auditor).
    host_confirmed boolean NOT NULL DEFAULT false,
    UNIQUE (kind, issuer, subject),       -- one person per key
    UNIQUE (handle, kind)                 -- one key of each kind per person
);

-- Authoritative history of every change above, written in the same transaction.
CREATE TABLE role_changes (
    id        bigserial PRIMARY KEY,
    at        timestamptz NOT NULL,
    actor     text NOT NULL,           -- a handle, or 'host' / 'import'
    client    text NOT NULL CHECK (client IN ('web', 'bot', 'host', 'import', 'system')),
    host_user text CHECK (host_user ~ '^[a-z_][a-z0-9_.-]{0,31}$'),  -- the OS user (§5)
    target    text NOT NULL,           -- a handle, or '*' for import and pilot exceptions
    change    text NOT NULL CHECK (change IN ('create', 'grant', 'revoke', 'deactivate',
              'reactivate', 'link', 'unlink', 'import', 'assign', 'unassign', 'set_field',
              'confirm_sign_in_name', 'suspend_credentials', 'delete_credentials',
              'exception_on', 'exception_off')),
    role      text,
    detail    text,                    -- fixed formats only (§10), never free text
    CHECK (client <> 'host' OR host_user IS NOT NULL)
);
CREATE INDEX role_changes_target ON role_changes (target, at DESC);
CREATE INDEX role_changes_recent ON role_changes (change, at DESC);

-- Owner Q3: Admin and Auditor on one person, for the pilot only (§6). Host only.
CREATE TABLE pilot_exceptions (
    id          bigserial PRIMARY KEY,
    name        text NOT NULL CHECK (name = 'admin_and_auditor'),
    enabled_at  timestamptz NOT NULL,
    enabled_by  text NOT NULL,         -- host_user
    expires_at  timestamptz NOT NULL CHECK (expires_at <= enabled_at + interval '90 days'),
    ended_at    timestamptz,
    ended_by    text,
    CHECK (expires_at > enabled_at),
    CHECK ((ended_at IS NULL) = (ended_by IS NULL))
);
CREATE UNIQUE INDEX one_open_exception ON pilot_exceptions (name) WHERE ended_at IS NULL;

-- Bumped by users_invariants() for every committed change to users, user_roles,
-- user_identities or pilot_exceptions (and reviewer_assignments, with U6): the ETag of the
-- directory (§2, slice U2).
CREATE TABLE directory_version (
    one boolean PRIMARY KEY DEFAULT true CHECK (one),
    v   bigint NOT NULL
);
INSERT INTO directory_version (one, v) VALUES (true, 1);

-- 1. The invariants, checked at commit under the users lock.
CREATE FUNCTION users_invariants() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    touched text[];
    exception_open boolean;
    offenders text;
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

    -- The directory's version (the ETag, §2), bumped here, after the lock, so concurrent
    -- writers don't queue on its row before they reach the checks above.
    UPDATE directory_version SET v = v + 1 WHERE one;
    RETURN NULL;
END
$$;

CREATE CONSTRAINT TRIGGER users_invariants
    AFTER INSERT OR UPDATE OR DELETE ON users
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION users_invariants();
CREATE CONSTRAINT TRIGGER users_invariants
    AFTER INSERT OR UPDATE OR DELETE ON user_roles
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION users_invariants();
CREATE CONSTRAINT TRIGGER users_invariants
    AFTER INSERT OR UPDATE OR DELETE ON user_identities
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION users_invariants();
CREATE CONSTRAINT TRIGGER users_invariants
    AFTER INSERT OR UPDATE OR DELETE ON pilot_exceptions
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION users_invariants();

-- 3. Never deleted, never truncated. (TRUNCATE skips row triggers, so it gets its own.)
CREATE FUNCTION users_keep_rows() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_TABLE_NAME = 'users' THEN
        RAISE EXCEPTION 'users are deactivated, never deleted' USING ERRCODE = 'insufficient_privilege';
    END IF;
    RAISE EXCEPTION '% can''t be truncated', TG_TABLE_NAME USING ERRCODE = 'insufficient_privilege';
END
$$;
CREATE TRIGGER users_no_delete BEFORE DELETE ON users
    FOR EACH ROW EXECUTE FUNCTION users_keep_rows();
CREATE TRIGGER users_no_truncate BEFORE TRUNCATE ON users
    FOR EACH STATEMENT EXECUTE FUNCTION users_keep_rows();
CREATE TRIGGER user_roles_no_truncate BEFORE TRUNCATE ON user_roles
    FOR EACH STATEMENT EXECUTE FUNCTION users_keep_rows();
CREATE TRIGGER user_identities_no_truncate BEFORE TRUNCATE ON user_identities
    FOR EACH STATEMENT EXECUTE FUNCTION users_keep_rows();
CREATE TRIGGER pilot_exceptions_no_truncate BEFORE TRUNCATE ON pilot_exceptions
    FOR EACH STATEMENT EXECUTE FUNCTION users_keep_rows();

-- 2. role_changes: append-only, with its own narrow retention rule (owner Q5, §10).
CREATE FUNCTION role_changes_append_only() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    stack text;
BEGIN
    -- The one exception: a row DELETE (never UPDATE, never TRUNCATE) ...
    IF TG_OP = 'DELETE' AND TG_LEVEL = 'ROW'
       -- ... while the retention function says it is purging ...
       AND current_setting('pmos.role_changes_retention', true) = 'on'
       -- ... of a person's row (never an import or pilot exception row) ...
       AND OLD.target <> '*'
       -- ... whose person was deactivated more than 90 days ago, checked here, per row ...
       AND EXISTS (SELECT 1 FROM users u
                   WHERE u.handle = OLD.target AND u.status = 'deactivated'
                     AND u.deactivated_at < now() - interval '90 days') THEN
        -- ... and the DELETE runs inside the retention function.
        GET DIAGNOSTICS stack = PG_CONTEXT;
        IF position(E'\nPL/pgSQL function purge_role_changes(integer) line ' IN stack) > 0 THEN
            RETURN OLD;
        END IF;
    END IF;
    RAISE EXCEPTION 'role_changes is append-only' USING ERRCODE = 'insufficient_privilege';
END
$$;
CREATE TRIGGER role_changes_append_only
    BEFORE UPDATE OR DELETE ON role_changes
    FOR EACH ROW EXECUTE FUNCTION role_changes_append_only();
CREATE TRIGGER role_changes_no_truncate
    BEFORE TRUNCATE ON role_changes
    FOR EACH STATEMENT EXECUTE FUNCTION role_changes_append_only();

-- The daily retention job's one way to delete role history: the rows of people deactivated
-- more than 90 days ago, at most batch_size per call, recorded in the audit log in the same
-- transaction (client `system`, a count only).
CREATE FUNCTION purge_role_changes(batch_size integer)
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
        SELECT c.id FROM role_changes c JOIN users u ON u.handle = c.target
        WHERE c.target <> '*' AND u.status = 'deactivated'
          AND u.deactivated_at < now() - interval '90 days'
        ORDER BY c.id
        LIMIT batch_size
        FOR UPDATE OF c SKIP LOCKED
    )
    DELETE FROM role_changes c USING doomed d WHERE c.id = d.id;
    GET DIAGNOSTICS v_deleted = ROW_COUNT;
    PERFORM set_config('pmos.role_changes_retention', 'off', true);
    v_more := EXISTS (SELECT 1 FROM role_changes c JOIN users u ON u.handle = c.target
                      WHERE c.target <> '*' AND u.status = 'deactivated'
                        AND u.deactivated_at < now() - interval '90 days');
    INSERT INTO audit_events (at, run_id, user_handle, skill, tool, outcome, sources, client, detail)
    VALUES (now(), 'retention', 'pmos-retention', NULL, 'users.role_changes_purge', 'ok', '{}',
            'system', format('purged %s role changes', v_deleted));
    RETURN QUERY SELECT v_deleted, v_more;
END
$$;
REVOKE ALL ON FUNCTION purge_role_changes(integer) FROM PUBLIC;

-- 4. A fixed search_path for every function above, written with the schema the migration
--    runs in (the tests give each case its own schema).
DO $$
BEGIN
    EXECUTE format('ALTER FUNCTION users_invariants() SET search_path = %I, pg_temp', current_schema());
    EXECUTE format('ALTER FUNCTION users_keep_rows() SET search_path = %I, pg_temp', current_schema());
    EXECUTE format('ALTER FUNCTION role_changes_append_only() SET search_path = %I, pg_temp', current_schema());
    EXECUTE format('ALTER FUNCTION purge_role_changes(integer) SET search_path = %I, pg_temp', current_schema());
END
$$;
