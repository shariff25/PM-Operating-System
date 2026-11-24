-- pmos-bots: the marketplace bot (persona) a task, routine or notification is for
-- (docs/bot-marketplace-spec.md §5 and §6). NULL is Nudge, the always-on generalist, so every
-- row from before this migration keeps meaning what it meant. `nudge` itself is never stored:
-- a request that names it is stored as NULL, and the check refuses it in case code forgets.

ALTER TABLE bots_tasks
    ADD COLUMN persona text
        CHECK (persona ~ '^[a-z0-9]+(-[a-z0-9]+)*$' AND length(persona) <= 32
               AND persona <> 'nudge');

ALTER TABLE bots_routines
    ADD COLUMN persona text
        CHECK (persona ~ '^[a-z0-9]+(-[a-z0-9]+)*$' AND length(persona) <= 32
               AND persona <> 'nudge');

-- So the Slack service can say which bot is speaking ("Chief: …").
ALTER TABLE bots_notifications
    ADD COLUMN persona text
        CHECK (persona ~ '^[a-z0-9]+(-[a-z0-9]+)*$' AND length(persona) <= 32
               AND persona <> 'nudge');

-- A conversation's earlier turns are those of the same bot, requester, thread and persona.
DROP INDEX IF EXISTS bots_tasks_thread;
CREATE INDEX bots_tasks_thread ON bots_tasks (bot, requested_by, thread_key, persona, created_at DESC)
    WHERE thread_key IS NOT NULL;
