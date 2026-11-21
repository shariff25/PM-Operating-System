-- The reviewer's changes to the bot marketplace (docs/bot-marketplace-spec.md §13, "M1 is
-- built"). 0025 is reserved for another slice.

-- 1. The 16-row cap holds under concurrent writers. The deferred trigger counted a person's
-- rows with only its own transaction's view, so two transactions adding the 16th and 17th row
-- at once could each count 16 and both commit. It now takes the same per-person advisory lock
-- the tool server's team change takes (crate::team, `hashtext('pmos.user_bots')` and the
-- handle) before it counts, held to the end of the transaction: a second writer waits for the
-- first to commit, and its count (a later statement, so a fresh snapshot under READ
-- COMMITTED) sees the first one's rows.
CREATE OR REPLACE FUNCTION user_bots_cap() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    PERFORM pg_advisory_xact_lock(hashtext('pmos.user_bots'), hashtext(NEW.handle));
    IF (SELECT count(*) FROM user_bots WHERE handle = NEW.handle) > 16 THEN
        RAISE EXCEPTION 'user_bots: at most 16 bots per person'
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;

-- 2. A withdrawn bot's pending drafts are held: not approvable until the persona is offered
-- again or an Admin releases the item. The release is recorded here, once per item, with who
-- and when; it is the only thing written (the item itself is unchanged, and the decision still
-- belongs to its owner). Rows go with their item.
CREATE TABLE approval_hold_releases (
    approval_id text PRIMARY KEY REFERENCES approvals ON DELETE CASCADE,
    released_by text NOT NULL,
    released_at timestamptz NOT NULL
);

-- `CREATE OR REPLACE` keeps the path 0024 set; set it again in case.
DO $$
BEGIN
    EXECUTE format('ALTER FUNCTION user_bots_cap() SET search_path = %I, pg_temp', current_schema());
END
$$;
