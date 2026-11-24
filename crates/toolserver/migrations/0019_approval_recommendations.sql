-- Slice U6a of the user database (docs/user-database-spec.md §8): a Reviewer's recommendation
-- on a pending draft of a PM assigned to them (migration 0018's reviewer_assignments). A
-- recommendation is advice only: it never changes an approval's status, and nothing here is
-- read by a decision except to word the decision's audit detail.
--
-- 1. `approvals.revision`: the version of a draft's text, bumped whenever its arguments change
--    (a saved edit, `PUT /internal/approvals/{id}/draft`, or an edit decision) to arguments
--    with another hash. The B1 fix (migration 0012) saved edits on the item but added no
--    revision, so the spec's fallback applies: U6 adds it. Existing items start at 1.
-- 2. `approval_recommendations`: one row per recommendation, bound to the revision and the
--    args hash (SHA-256 of the arguments' canonical JSON) the reviewer read. One live row per
--    reviewer per draft: a new one withdraws the old (`withdrawn_at`), and the old note is
--    cleared then. A row whose revision or hash differs from the item's now is shown "on an
--    earlier version".
-- 3. The note is untrusted plain text from a person: at most 1000 characters, no control
--    characters but the line feed (the tool server cleans it on write; this is the backstop).
--    It is removed (`note_removed_at`) when its Slack draft's text is removed, by the same
--    sweep, under the same 24-hour rule (PMOS_SLACK_DRAFT_TTL_HOURS).
-- 4. Rows go with their item (ON DELETE CASCADE) and never with a user (users are deactivated,
--    not deleted). The audit events a recommendation writes are ordinary events, purged at 90
--    days with the rest of the log (migration 0013); nothing here touches that purge.

ALTER TABLE approvals ADD COLUMN revision integer NOT NULL DEFAULT 1 CHECK (revision >= 1);

CREATE TABLE approval_recommendations (
    id              text PRIMARY KEY,
    approval_id     text NOT NULL REFERENCES approvals ON DELETE CASCADE,
    reviewer        text NOT NULL REFERENCES users ON DELETE RESTRICT,
    verdict         text NOT NULL CHECK (verdict IN ('approve', 'changes')),
    note            text CHECK (char_length(note) BETWEEN 1 AND 1000
                                AND note !~ '[\x01-\x09\x0b-\x1f\x7f]'),
    note_removed_at timestamptz,
    draft_revision  integer NOT NULL CHECK (draft_revision >= 1),
    draft_sha256    text NOT NULL CHECK (draft_sha256 ~ '^[0-9a-f]{64}$'),
    created_at      timestamptz NOT NULL,
    withdrawn_at    timestamptz,
    CHECK (note IS NULL OR note_removed_at IS NULL)
);
CREATE UNIQUE INDEX approval_recommendations_live
    ON approval_recommendations (approval_id, reviewer) WHERE withdrawn_at IS NULL;
CREATE INDEX approval_recommendations_reviewer ON approval_recommendations (reviewer);
