-- An approved write is `executing` until its outcome is stored, so a crash mid-write leaves a
-- visible item for the start-up reconciler instead of one that looks done.
ALTER TABLE approvals DROP CONSTRAINT approvals_status_check;
ALTER TABLE approvals ADD CONSTRAINT approvals_status_check
    CHECK (status IN ('pending', 'executing', 'approved', 'rejected', 'failed'));
CREATE INDEX approvals_executing ON approvals (decided_at) WHERE status = 'executing';

-- The row trigger stops UPDATE and DELETE; TRUNCATE needs a statement trigger.
CREATE TRIGGER audit_events_no_truncate
    BEFORE TRUNCATE ON audit_events
    FOR EACH STATEMENT EXECUTE FUNCTION audit_events_append_only();
