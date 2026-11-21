-- The bot marketplace (docs/bot-marketplace-spec.md §5, slice B1): the marketplace bots
-- ("personas") on each PM's team, and which persona a session runs as, drafted an item or made
-- a call.
--
-- Numbering: the spec names this file 0023; 0023 was taken by a security-fix branch in flight,
-- so the team is 0024.
--
-- Nudge is always on every team and is never a row. A row whose persona is no longer offered
-- (or no longer in the catalog) is kept and inert: it grants nothing until the persona is
-- offered again. History is the audit log (`team.change` events), not a second table.
CREATE TABLE user_bots (
    handle    text NOT NULL REFERENCES users ON DELETE RESTRICT,
    persona   text NOT NULL CHECK (persona ~ '^[a-z0-9]+(-[a-z0-9]+)*$'
                                   AND length(persona) <= 32 AND persona <> 'nudge'),
    added_at  timestamptz NOT NULL,
    added_via text NOT NULL CHECK (added_via IN ('onboarding', 'bots_page', 'picker', 'seed')),
    PRIMARY KEY (handle, persona)
);

-- At most 16 rows per person, in the database as well as in code.
CREATE FUNCTION user_bots_cap() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF (SELECT count(*) FROM user_bots WHERE handle = NEW.handle) > 16 THEN
        RAISE EXCEPTION 'user_bots: at most 16 bots per person'
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;
CREATE CONSTRAINT TRIGGER user_bots_cap AFTER INSERT ON user_bots
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION user_bots_cap();

-- Which persona a session runs as, which persona drafted an item, which persona made a call.
-- NULL is Nudge (and every row from before this migration). Schema changes don't fire the
-- audit log's append-only row trigger; existing events keep NULL.
ALTER TABLE sessions     ADD COLUMN persona text CHECK (persona ~ '^[a-z0-9]+(-[a-z0-9]+)*$');
ALTER TABLE approvals    ADD COLUMN persona text CHECK (persona ~ '^[a-z0-9]+(-[a-z0-9]+)*$');
ALTER TABLE audit_events ADD COLUMN persona text CHECK (persona ~ '^[a-z0-9]+(-[a-z0-9]+)*$');
ALTER TABLE sessions ADD CONSTRAINT sessions_persona_needs_bot
    CHECK (persona IS NULL OR bot IS NOT NULL);

-- `approvals.persona` is copied from the drafting session when the item is created and never
-- changes: no route writes it, and the database refuses an update that would.
CREATE FUNCTION approvals_persona_fixed() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.persona IS DISTINCT FROM OLD.persona THEN
        RAISE EXCEPTION 'approvals.persona is set when the item is drafted and never changes'
            USING ERRCODE = 'insufficient_privilege';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER approvals_persona_fixed BEFORE UPDATE OF persona ON approvals
    FOR EACH ROW EXECUTE FUNCTION approvals_persona_fixed();

-- The functions run with this schema first on their path, as 0021's do (the spec's
-- `pg_catalog, public` would miss a schema other than `public`, as each test's is).
DO $$
BEGIN
    EXECUTE format('ALTER FUNCTION user_bots_cap() SET search_path = %I, pg_temp', current_schema());
    EXECUTE format('ALTER FUNCTION approvals_persona_fixed() SET search_path = %I, pg_temp', current_schema());
END
$$;
