-- The admin and auditor reads (`GET /internal/admin/*`, docs/web-api.md, Admin and auditor).
--
-- 1. The services that act for a person (`pmos-web`, the console) have recorded events with
--    `client` = 'web' or 'console' since they got their own tokens, but the column's check
--    still allowed only 'mcp', 'orchestrator' and 'bot', so Postgres refused those events.
--    Widen it to every client the tool server has.
-- 2. The whole-log search reads newest first by (at, id): index that order.
--
-- Neither statement rewrites a row, and the append-only triggers on audit_events are untouched.
ALTER TABLE audit_events DROP CONSTRAINT IF EXISTS audit_events_client_check;
ALTER TABLE audit_events ADD CONSTRAINT audit_events_client_check
    CHECK (client IN ('mcp', 'orchestrator', 'bot', 'web', 'console'));

CREATE INDEX audit_events_at ON audit_events (at DESC, id DESC);
