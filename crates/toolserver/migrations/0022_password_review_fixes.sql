-- Password sign-in, after the independent review of 2026-11-20 (docs/password-sign-in.md, "What
-- changed after review"). 0021 belongs to the user database's fixes.
--
-- 1. The wait after failed guesses is per person AND per address (PM1): someone guessing from
--    one address no longer holds the person out of every other one. password_source_counters
--    keeps, per person and per address, the two counters (sign-in, setup code) and when the
--    person last fully signed in from there (a "known" address, exempt from the account-wide
--    ceiling). The address is stored only as the SHA-256 of pmos-web's normalized text
--    (`password::limits::source_key`), never as an address. The counters on
--    password_credentials stay, as account-wide counts only (locked_until and
--    setup_locked_until are no longer set).
-- 2. Who issued the code a credential was set with (PM3): `issued_via`. A web-issued
--    credential doesn't work for a person holding Admin or Auditor. Credentials set before
--    this migration get the client of the setup code they were set with, when it is still on
--    record; otherwise NULL, which counts as web-issued.
-- 3. A reset is never silent (PM2): when a reset code is issued for a person who has a
--    credential, who issued it and when is kept on their credential until they dismiss it.

ALTER TABLE password_credentials
    ADD COLUMN issued_via text CHECK (issued_via IN ('web', 'host')),
    ADD COLUMN reset_notice_by text CHECK (length(reset_notice_by) BETWEEN 1 AND 64),
    ADD COLUMN reset_notice_via text CHECK (reset_notice_via IN ('web', 'host')),
    ADD COLUMN reset_notice_host_user text CHECK (length(reset_notice_host_user) <= 64),
    ADD COLUMN reset_notice_at timestamptz,
    ADD CONSTRAINT password_reset_notice_whole CHECK (
        (reset_notice_by IS NULL) = (reset_notice_at IS NULL)
        AND (reset_notice_by IS NULL) = (reset_notice_via IS NULL)
        AND (reset_notice_host_user IS NULL OR reset_notice_via = 'host'));

UPDATE password_credentials c
   SET issued_via = (SELECT s.issued_client FROM password_setup_codes s
                      WHERE s.handle = c.handle AND s.used_at IS NOT NULL
                      ORDER BY s.used_at DESC, s.id DESC LIMIT 1)
 WHERE c.password_hash IS NOT NULL;

CREATE TABLE password_source_counters (
    handle               text NOT NULL REFERENCES users ON DELETE RESTRICT
                         CHECK (handle <> 'evals'),
    source_key           text NOT NULL CHECK (source_key ~ '^[0-9a-f]{64}$'),
    sign_in_count        integer NOT NULL DEFAULT 0 CHECK (sign_in_count >= 0),
    sign_in_last_at      timestamptz,
    sign_in_locked_until timestamptz,
    setup_count          integer NOT NULL DEFAULT 0 CHECK (setup_count >= 0),
    setup_last_at        timestamptz,
    setup_locked_until   timestamptz,
    last_success_at      timestamptz,
    updated_at           timestamptz NOT NULL,
    PRIMARY KEY (handle, source_key)
);
CREATE INDEX password_source_counters_updated ON password_source_counters (updated_at);
