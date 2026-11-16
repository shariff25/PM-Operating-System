-- pmos-bots: conversations. A follow-up in the same Slack thread (or the same web
-- conversation) is given the earlier turns of that thread (docs/bot-runtime-spec.md §3).
--
-- `conversation` is the id a client names (the web app); `thread_key` is the runtime's own
-- key for the thread, `c:<conversation>` or `s:<channel>:<thread_ts>`, set only for
-- interactive tasks, so routines and evals never share history.

ALTER TABLE bots_tasks
    ADD COLUMN conversation text,
    ADD COLUMN thread_key text;

UPDATE bots_tasks
    SET thread_key = 's:' || (slack ->> 'channel') || ':' || (slack ->> 'thread_ts')
    WHERE slack IS NOT NULL AND priority = 'interactive';

CREATE INDEX bots_tasks_thread ON bots_tasks (bot, requested_by, thread_key, created_at DESC)
    WHERE thread_key IS NOT NULL;
