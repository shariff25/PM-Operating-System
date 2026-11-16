-- pmos-bots: a notification names the routine that created its task, when one did
-- (docs/bot-runtime-spec.md §5, `GET /v1/notifications`), so the Slack service can tell a
-- delivered routine run (the daily brief) from any other finished task. Rows written before
-- this column existed read as having no routine.

ALTER TABLE bots_notifications ADD COLUMN routine_id text;
