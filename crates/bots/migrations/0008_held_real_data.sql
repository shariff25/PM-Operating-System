-- pmos-bots: the permanent "this database has held real data" marker (slice H4,
-- docs/openshell-any-host-spec.md §3.6, "hosted-test, and fake-data mode"). The runtime writes
-- the one row the first time it starts with PMOS_CONNECTORS=live, whatever the model source, and
-- refuses to start under PMOS_MODEL_SOURCE=hosted-test (fake data only) while it is there. No
-- code path clears it: the triggers below refuse an UPDATE, a DELETE or a TRUNCATE, so only a
-- fresh database (or dropping the table by hand, as the database's owner) removes it. A restore
-- carries it with the rest of the data.

CREATE TABLE bots_held_real_data (
    id boolean PRIMARY KEY DEFAULT true CHECK (id),
    first_at timestamptz NOT NULL DEFAULT now()
);

CREATE FUNCTION bots_held_real_data_is_permanent() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'the held_real_data marker is permanent';
END;
$$;

CREATE TRIGGER bots_held_real_data_no_change
    BEFORE UPDATE OR DELETE ON bots_held_real_data
    FOR EACH ROW EXECUTE FUNCTION bots_held_real_data_is_permanent();

CREATE TRIGGER bots_held_real_data_no_truncate
    BEFORE TRUNCATE ON bots_held_real_data
    FOR EACH STATEMENT EXECUTE FUNCTION bots_held_real_data_is_permanent();
