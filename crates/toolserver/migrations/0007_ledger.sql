-- The feedback ledger (docs/improvement-loop-spec.md §1): one row per approval item, written
-- in the same transaction that creates, decides or completes the item. Labels and metadata
-- only: never the draft's text, never Slack text.
--
-- `expired` is not stored: a `pending` row never decided and older than
-- PMOS_LEDGER_EXPIRE_DAYS is reported as `expired` when read. `edited` records whether the
-- decision changed the item's arguments, so a finished write reads `edited`, not `approved`.
CREATE TABLE ledger (
    id            text PRIMARY KEY REFERENCES approvals (id),
    run_id        text NOT NULL,
    user_handle   text NOT NULL,
    bot           text,
    skill         text,
    tool          text NOT NULL,
    content_sha   text,
    model_alias   text,
    outcome       text NOT NULL
        CHECK (outcome IN ('pending', 'approved', 'edited', 'rejected', 'failed')),
    edited        boolean NOT NULL DEFAULT false,
    edit_distance double precision CHECK (edit_distance >= 0 AND edit_distance <= 1),
    reason        text CHECK (reason IN ('wrong_facts', 'wrong_target', 'tone', 'incomplete',
                                         'should_not_draft', 'other')),
    created_at    timestamptz NOT NULL,
    decided_at    timestamptz,
    decided_by    text,
    CHECK (reason IS NULL OR outcome = 'rejected')
);
-- Reads filter and order by the row's last change: its decision, else its creation.
CREATE INDEX ledger_activity ON ledger ((COALESCE(decided_at, created_at)) DESC, id);

-- Items made before the ledger get a row each, so none is missing. What they were drafted
-- from, and how much an approval changed them, wasn't recorded, so those stay null.
INSERT INTO ledger
    (id, run_id, user_handle, bot, skill, tool, outcome, created_at, decided_at, decided_by)
SELECT id, run_id, user_handle, bot, skill, tool,
       CASE status WHEN 'executing' THEN 'pending' ELSE status END,
       created_at, decided_at, decided_by
FROM approvals;
