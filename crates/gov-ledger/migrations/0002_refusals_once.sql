-- A refusal is kept once, however often the same envelope is refused for the same reason
-- (review finding GR-74): a gate whose chain is stuck resends its batch every round, and
-- anyone who recorded such a shipment could replay it. `seen` counts the arrivals and
-- `last_at` is the latest.
ALTER TABLE refusals ADD COLUMN seen BIGINT NOT NULL DEFAULT 1 CHECK (seen >= 1);
ALTER TABLE refusals ADD COLUMN last_at BIGINT;
UPDATE refusals SET last_at = at;
ALTER TABLE refusals ALTER COLUMN last_at SET NOT NULL;

WITH kept AS (
    SELECT tenant, env, envelope_hash, code, MIN(id) AS id, COUNT(*) AS seen, MAX(at) AS last_at
    FROM refusals GROUP BY tenant, env, envelope_hash, code
)
UPDATE refusals r SET seen = kept.seen, last_at = kept.last_at
FROM kept WHERE r.tenant = kept.tenant AND r.env = kept.env AND r.id = kept.id;

DELETE FROM refusals r USING refusals first
WHERE r.tenant = first.tenant AND r.env = first.env
  AND r.envelope_hash = first.envelope_hash AND r.code = first.code AND r.id > first.id;

CREATE UNIQUE INDEX refusals_once ON refusals (tenant, env, envelope_hash, code);
