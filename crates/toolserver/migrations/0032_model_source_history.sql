-- Slice H12 (docs/openshell-any-host-spec.md §3.6, "A first switch to a hosted entry needs an
-- acknowledgement", r7 X3): the sources this database has served, and the acknowledgements of
-- switches to a hosted entry. Both are permanent, like held_real_data: no code path clears
-- them, the triggers below refuse a DELETE or a TRUNCATE (and any UPDATE of the history but
-- its last_at), and a restore carries them with the rest of the data.
--
-- Numbered 0032, not 0030: the MCP servers and the prover builds each add a 0030 in parallel.

CREATE TABLE model_source_history (
    source text PRIMARY KEY CHECK (length(source) BETWEEN 1 AND 80),
    -- `local` or a `hosted:<name>` other than the test-only one; `stub`, `hosted-test` and
    -- `hosted:stack-test` are recorded but never count.
    real boolean NOT NULL,
    first_at timestamptz NOT NULL,
    last_at timestamptz NOT NULL
);

CREATE TABLE model_source_acknowledgements (
    id bigserial PRIMARY KEY,
    to_source text NOT NULL CHECK (to_source LIKE 'hosted:%'),
    by_user text NOT NULL CHECK (length(by_user) BETWEEN 1 AND 64),
    reason text NOT NULL CHECK (length(reason) BETWEEN 1 AND 500),
    at timestamptz NOT NULL
);

CREATE FUNCTION model_source_records_are_permanent() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'UPDATE' THEN
        IF TG_TABLE_NAME = 'model_source_history' THEN
            IF NEW.source = OLD.source AND NEW.real = OLD.real
               AND NEW.first_at = OLD.first_at AND NEW.last_at >= OLD.last_at THEN
                RETURN NEW;
            END IF;
        END IF;
    END IF;
    RAISE EXCEPTION 'the model-source history and its acknowledgements are permanent';
END;
$$;

CREATE TRIGGER model_source_history_no_change
    BEFORE UPDATE OR DELETE ON model_source_history
    FOR EACH ROW EXECUTE FUNCTION model_source_records_are_permanent();

CREATE TRIGGER model_source_history_no_truncate
    BEFORE TRUNCATE ON model_source_history
    FOR EACH STATEMENT EXECUTE FUNCTION model_source_records_are_permanent();

CREATE TRIGGER model_source_acknowledgements_no_change
    BEFORE UPDATE OR DELETE ON model_source_acknowledgements
    FOR EACH ROW EXECUTE FUNCTION model_source_records_are_permanent();

CREATE TRIGGER model_source_acknowledgements_no_truncate
    BEFORE TRUNCATE ON model_source_acknowledgements
    FOR EACH STATEMENT EXECUTE FUNCTION model_source_records_are_permanent();
