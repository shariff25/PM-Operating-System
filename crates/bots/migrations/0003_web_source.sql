-- pmos-bots: the web app's tasks have source `web`.

ALTER TABLE bots_tasks DROP CONSTRAINT bots_tasks_source_check;
ALTER TABLE bots_tasks
    ADD CONSTRAINT bots_tasks_source_check CHECK (source IN ('slack', 'web', 'routine', 'cli', 'eval'));
