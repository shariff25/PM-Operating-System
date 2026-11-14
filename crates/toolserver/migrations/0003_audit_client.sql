-- Who made each call: a sandbox over MCP, the orchestrator, or the Slack bot. Each internal
-- service has its own token, so the audit log can say which one acted.
ALTER TABLE audit_events ADD COLUMN client text NOT NULL DEFAULT 'mcp'
    CHECK (client IN ('mcp', 'orchestrator', 'bot'));
