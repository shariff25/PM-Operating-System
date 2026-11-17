-- pmos-bots: the highest data class a task's tool-server sessions read, recorded when its
-- worker finishes it (docs/bot-runtime-spec.md §3, Conversations). A follow-up in the same
-- conversation replays this task's answer, so its session starts at least here. Rows from
-- before this column (and tasks recovered after a lost lease) have none, which a follow-up
-- reads as `restricted`.

ALTER TABLE bots_tasks
    ADD COLUMN sources_sensitivity text
        CHECK (sources_sensitivity IN ('public', 'internal', 'restricted'));
