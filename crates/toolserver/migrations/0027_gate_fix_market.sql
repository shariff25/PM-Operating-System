-- The security gate's marketplace findings on 8e8f8fe (docs/bot-marketplace-spec.md §13, "Gate
-- fixes after the review of 8e8f8fe (marketplace)").

-- A persona's draft notice (Echo's "Draft for internal review. Not approved for external use.")
-- is stored on the item when it is drafted, so a saved edit and the approved write put it back
-- at the top of the page even once the persona's file is gone or no longer says it. Copied from
-- the persona when the item is created and never changed; NULL for every other item.
ALTER TABLE approvals ADD COLUMN draft_notice text
    CHECK (draft_notice IS NULL OR (length(draft_notice) BETWEEN 1 AND 160));

CREATE FUNCTION approvals_notice_fixed() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.draft_notice IS DISTINCT FROM OLD.draft_notice THEN
        RAISE EXCEPTION 'approvals.draft_notice is set when the item is drafted and never changes'
            USING ERRCODE = 'insufficient_privilege';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER approvals_notice_fixed BEFORE UPDATE OF draft_notice ON approvals
    FOR EACH ROW EXECUTE FUNCTION approvals_notice_fixed();

DO $$
BEGIN
    EXECUTE format('ALTER FUNCTION approvals_notice_fixed() SET search_path = %I, pg_temp', current_schema());
END
$$;
