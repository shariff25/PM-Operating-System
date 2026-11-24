-- Per-PM OAuth tokens for MCP servers (docs/mcp-servers-spec.md §2.7, slice M4). One row per
-- person (or team bot identity) and server record id. Tokens are sealed with AES-256-GCM by the
-- tool server before they reach this table, bound to (user, `mcp:<server>`, field)
-- (src/mcp_client/oauth.rs); the database never sees a plaintext token. `account` is the email
-- the server's identity check returned, matched against the user database before sealing.
CREATE TABLE mcp_credentials (
    user_handle   text NOT NULL,
    server_id     text NOT NULL CHECK (server_id ~ '^[a-z][a-z0-9-]{0,39}$'),
    key_id        text NOT NULL,
    access_token  text NOT NULL,
    refresh_token text,
    expires_at    timestamptz,
    scopes        text NOT NULL DEFAULT '',
    account       text,
    updated_at    timestamptz NOT NULL,
    PRIMARY KEY (user_handle, server_id)
);
