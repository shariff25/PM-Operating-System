-- The Slack reply assistant (docs/slack-reply-spec.md §3).
--
-- A Slack draft (`slack.post_reply`, `slack.send_message`) still `pending` after
-- PMOS_SLACK_DRAFT_TTL_HOURS becomes `expired`, and every string argument but `channel` and
-- `thread_ts` is replaced with "[expired]"; the args hash is kept. A decided Slack draft
-- loses its text the same time after its decision. `redacted_at` records when the text went.
ALTER TABLE approvals DROP CONSTRAINT approvals_status_check;
ALTER TABLE approvals ADD CONSTRAINT approvals_status_check
    CHECK (status IN ('pending', 'executing', 'approved', 'rejected', 'failed', 'expired'));
ALTER TABLE approvals ADD COLUMN redacted_at timestamptz;

-- The highest data class the drafting session had read when the item was created. The
-- audience check blocks a `restricted` draft from reaching anyone outside team.yaml. Null
-- for items made before this migration, which the check treats as restricted.
ALTER TABLE approvals ADD COLUMN sources_sensitivity text
    CHECK (sources_sensitivity IN ('public', 'internal', 'restricted'));

-- What each session has read so far, raised as it reads.
ALTER TABLE sessions ADD COLUMN sources_sensitivity text NOT NULL DEFAULT 'public'
    CHECK (sources_sensitivity IN ('public', 'internal', 'restricted'));

-- One draft per thread: a user's recent replies, by thread.
CREATE INDEX approvals_slack_thread
    ON approvals (user_handle, (args ->> 'channel'), (args ->> 'thread_ts'), created_at)
    WHERE tool = 'slack.post_reply';
-- The sweeper: undecided Slack drafts, and decided ones that still hold their text.
CREATE INDEX approvals_slack_pending ON approvals (created_at)
    WHERE status = 'pending' AND tool IN ('slack.post_reply', 'slack.send_message');
CREATE INDEX approvals_slack_unredacted ON approvals (decided_at)
    WHERE redacted_at IS NULL AND tool IN ('slack.post_reply', 'slack.send_message');

-- An expired Slack draft is stored as `expired`. Other tools' pending rows are still
-- reported as `expired` when read, after PMOS_LEDGER_EXPIRE_DAYS.
ALTER TABLE ledger DROP CONSTRAINT ledger_outcome_check;
ALTER TABLE ledger ADD CONSTRAINT ledger_outcome_check
    CHECK (outcome IN ('pending', 'approved', 'edited', 'rejected', 'failed', 'expired'));
