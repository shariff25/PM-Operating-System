-- Email-and-password sign-in with a required authenticator-app code (docs/password-sign-in.md).
-- The tool server owns users, so it owns these secrets; pmos-web never stores or sees one.
-- 0016 stays free for the B1 fix.
--
-- What the database keeps:
-- 1. password_credentials: one row per user who has, or is being given, a password credential.
--    The password as an Argon2id PHC string (its parameters inside it, so they can be raised
--    later); the authenticator secret sealed by the vault (AES-256-GCM, bound to the handle);
--    the last accepted authenticator time step (a code is accepted once); the credential's
--    epoch (bumped by every setup, reset and change: a pmos-web session holds the epoch it
--    signed in with and ends when it moves); and the two failure counters behind the growing
--    delay (sign-in, and setup-code guesses).
-- 2. password_recovery_codes: ten single-use codes per user, as SHA-256 hashes only.
-- 3. password_setup_codes: one-time setup (or reset) codes an Admin, or the host, issues; as
--    SHA-256 hashes only; at most one live code per user; 24 hours at most.
-- 4. password_pending: the short-lived, single-purpose tokens between steps (after a correct
--    password; during setup), as SHA-256 hashes only, bound to one user.
-- The `evals` account can never have any of these. Users are never deleted (their rows stay
-- referenced); a deactivated user's credential is deleted when the 24-hour grace ends.

CREATE TABLE password_credentials (
    handle              text PRIMARY KEY REFERENCES users ON DELETE RESTRICT
                        CHECK (handle <> 'evals'),
    password_hash       text CHECK (password_hash LIKE '$argon2id$%' AND length(password_hash) <= 512),
    totp_sealed         text CHECK (totp_sealed LIKE 'v1.%' AND length(totp_sealed) <= 512),
    totp_last_step      bigint,
    epoch               bigint NOT NULL DEFAULT 0 CHECK (epoch >= 0),
    failed_count        integer NOT NULL DEFAULT 0 CHECK (failed_count >= 0),
    failed_last_at      timestamptz,
    locked_until        timestamptz,
    setup_failed_count  integer NOT NULL DEFAULT 0 CHECK (setup_failed_count >= 0),
    setup_failed_last_at timestamptz,
    setup_locked_until  timestamptz,
    set_at              timestamptz,
    updated_at          timestamptz NOT NULL,
    -- A password never exists without its second step, and the other way round.
    CHECK ((password_hash IS NULL) = (totp_sealed IS NULL)),
    CHECK ((password_hash IS NULL) = (set_at IS NULL))
);

CREATE TABLE password_recovery_codes (
    handle      text NOT NULL REFERENCES users ON DELETE RESTRICT CHECK (handle <> 'evals'),
    code_hash   text NOT NULL CHECK (code_hash ~ '^[0-9a-f]{64}$'),
    created_at  timestamptz NOT NULL,
    used_at     timestamptz,
    PRIMARY KEY (handle, code_hash)
);

CREATE TABLE password_setup_codes (
    id            bigserial PRIMARY KEY,
    handle        text NOT NULL REFERENCES users ON DELETE RESTRICT CHECK (handle <> 'evals'),
    code_hash     text NOT NULL UNIQUE CHECK (code_hash ~ '^[0-9a-f]{64}$'),
    kind          text NOT NULL CHECK (kind IN ('setup', 'reset')),
    issued_at     timestamptz NOT NULL,
    issued_by     text NOT NULL,
    issued_client text NOT NULL CHECK (issued_client IN ('web', 'host')),
    expires_at    timestamptz NOT NULL CHECK (expires_at > issued_at
                                              AND expires_at <= issued_at + interval '24 hours'),
    used_at       timestamptz,
    revoked_at    timestamptz,
    CHECK (used_at IS NULL OR revoked_at IS NULL)
);
-- A new code replaces the old one: at most one live code per user.
CREATE UNIQUE INDEX password_one_live_setup_code ON password_setup_codes (handle)
    WHERE used_at IS NULL AND revoked_at IS NULL;

CREATE TABLE password_pending (
    token_hash     text PRIMARY KEY CHECK (token_hash ~ '^[0-9a-f]{64}$'),
    handle         text NOT NULL REFERENCES users ON DELETE RESTRICT CHECK (handle <> 'evals'),
    purpose        text NOT NULL CHECK (purpose IN ('sign_in', 'setup')),
    setup_code_id  bigint REFERENCES password_setup_codes ON DELETE CASCADE,
    epoch          bigint NOT NULL,
    created_at     timestamptz NOT NULL,
    expires_at     timestamptz NOT NULL CHECK (expires_at > created_at
                                               AND expires_at <= created_at + interval '15 minutes'),
    attempts       integer NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    staged_hash    text CHECK (staged_hash LIKE '$argon2id$%' AND length(staged_hash) <= 512),
    staged_totp    text CHECK (staged_totp LIKE 'v1.%' AND length(staged_totp) <= 512),
    -- A setup token belongs to one setup code; a sign-in token stages nothing.
    CHECK ((purpose = 'setup') = (setup_code_id IS NOT NULL)),
    CHECK (purpose = 'setup' OR (staged_hash IS NULL AND staged_totp IS NULL))
);
CREATE INDEX password_pending_handle ON password_pending (handle);
CREATE INDEX password_pending_expires ON password_pending (expires_at);

-- role_changes records issuing a setup or reset code and the credential it sets
-- (`issue_password_code`, `set_password`), as it records every other change to a person.
ALTER TABLE role_changes DROP CONSTRAINT IF EXISTS role_changes_change_check;
ALTER TABLE role_changes ADD CONSTRAINT role_changes_change_check
    CHECK (change IN ('create', 'grant', 'revoke', 'deactivate', 'reactivate', 'link', 'unlink',
                      'import', 'assign', 'unassign', 'set_field', 'confirm_sign_in_name',
                      'suspend_credentials', 'delete_credentials', 'exception_on',
                      'exception_off', 'issue_password_code', 'set_password'));

-- No TRUNCATE of the credential tables: a mistake there would sign everyone out of their
-- second step at once. Rows go one by one (a reset, the grace's end, the pruner).
CREATE TRIGGER password_credentials_no_truncate BEFORE TRUNCATE ON password_credentials
    FOR EACH STATEMENT EXECUTE FUNCTION users_keep_rows();
CREATE TRIGGER password_recovery_codes_no_truncate BEFORE TRUNCATE ON password_recovery_codes
    FOR EACH STATEMENT EXECUTE FUNCTION users_keep_rows();
