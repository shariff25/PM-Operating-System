-- Reviewer recommendations on approval items. A Reviewer recommends; only the item's own user
-- decides. A recommendation never changes the item. One per reviewer per item (a new one
-- replaces it). No free text: `reason` is one of the ledger's fixed reasons.
CREATE TABLE approval_recommendations (
    approval_id text        NOT NULL REFERENCES approvals (id),
    reviewer    text        NOT NULL,
    recommend   text        NOT NULL CHECK (recommend IN ('approve', 'reject')),
    reason      text        CHECK (reason IN ('wrong_facts', 'wrong_target', 'tone',
                                              'incomplete', 'should_not_draft', 'other')),
    created_at  timestamptz NOT NULL,
    PRIMARY KEY (approval_id, reviewer)
);
