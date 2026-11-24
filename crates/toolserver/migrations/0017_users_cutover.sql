-- Slice U3 of the user database (docs/user-database-spec.md §6 "Deactivation"): the tool server
-- decides from the users tables, and a deactivation is reversible for 24 hours.
--
-- 1. A deactivated person's linked accounts are suspended at once, in the deactivation's own
--    transaction: `suspended_at` is set, and the tool server never unseals a suspended
--    credential (vault.rs). A reactivation within the grace clears it; after the grace the
--    sweeper deletes the rows.
-- 2. A deactivated person's pending drafts are held (a decision is refused while the owner
--    isn't active) and, after the grace, closed by the system as `rejected` with the ledger
--    reason `owner_deactivated`. Only the tool server writes that reason; no client may give
--    it with a decision.

ALTER TABLE credentials ADD COLUMN suspended_at timestamptz;

ALTER TABLE ledger DROP CONSTRAINT IF EXISTS ledger_reason_check;
ALTER TABLE ledger ADD CONSTRAINT ledger_reason_check
    CHECK (reason IN ('wrong_facts', 'wrong_target', 'tone', 'incomplete', 'should_not_draft',
                      'other', 'owner_deactivated'));
