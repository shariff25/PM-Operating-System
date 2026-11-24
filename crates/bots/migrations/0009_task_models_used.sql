-- pmos-bots: which labelled models answered a task's model calls, as its worker reported them
-- when it finished (docs/model-providers-spec.md §4.9 and §4.10, slice MP7): labels and model
-- hashes only, never a model id. `fallback_used` is true when any call was answered by a
-- fallback model of its role's route, so a benchmarked bot's answer can be marked. Rows from
-- before this column have none.

ALTER TABLE bots_tasks
    ADD COLUMN models_used jsonb,
    ADD COLUMN fallback_used boolean NOT NULL DEFAULT false;

-- The Slack feed's item for a task answered (in part) by a fallback model says so, so the
-- Slack service can mark the reply and each draft card.
ALTER TABLE bots_notifications
    ADD COLUMN fallback_used boolean NOT NULL DEFAULT false;
