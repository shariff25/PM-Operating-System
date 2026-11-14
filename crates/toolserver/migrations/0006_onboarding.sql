-- Onboarding and personal memory in GitHub (docs/onboarding-spec.md). The spec names this
-- migration 0005; 0005 was already taken by bot sessions.

-- GitHub App user tokens, sealed like the others.
ALTER TABLE credentials DROP CONSTRAINT credentials_provider_check;
ALTER TABLE credentials ADD CONSTRAINT credentials_provider_check
    CHECK (provider IN ('atlassian', 'slack', 'github'));

-- Which repo holds each PM's memory, and when it was last checked to be private, in the org,
-- not archived and pushable by the PM. The repo itself is the PM's.
CREATE TABLE memory_repos (
    user_handle text PRIMARY KEY,
    repo        text NOT NULL,
    verified_at timestamptz NOT NULL
);

-- Requests to join PM OS, decided by a platform owner in Slack. One pending request per
-- Slack id at a time.
CREATE TABLE member_requests (
    id              text PRIMARY KEY,
    slack_id        text NOT NULL,
    email           text NOT NULL,
    name            text NOT NULL,
    proposed_handle text NOT NULL,
    status          text NOT NULL CHECK (status IN ('pending', 'approved', 'rejected', 'failed')),
    requested_at    timestamptz NOT NULL,
    decided_by      text,
    decided_at      timestamptz,
    handle          text,
    commit_sha      text,
    error           text
);
CREATE UNIQUE INDEX member_requests_one_pending ON member_requests (slack_id)
    WHERE status = 'pending';
CREATE INDEX member_requests_status ON member_requests (status, requested_at);
