-- Per-user OAuth credentials. Tokens are sealed with AES-256-GCM by the tool server before
-- they reach this table (see src/vault.rs); the database never sees plaintext tokens.
CREATE TABLE credentials (
    user_handle   text NOT NULL,
    provider      text NOT NULL CHECK (provider IN ('atlassian', 'slack')),
    key_id        text NOT NULL,
    access_token  text NOT NULL,
    refresh_token text,
    expires_at    timestamptz,
    scopes        text NOT NULL DEFAULT '',
    cloud_id      text,
    site_url      text,
    external_id   text,
    updated_at    timestamptz NOT NULL,
    PRIMARY KEY (user_handle, provider)
);
