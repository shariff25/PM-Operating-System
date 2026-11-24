-- OpenShell's policy prover on the host (docs/openshell-prover-and-sandbox-audit-spec.md §A.5,
-- slice A3): one row per bot each time `pmos-admin policy prove` runs, written by the host command
-- `pmos-toolserver policy-proof record` (with PMOS_HOST_USER) together with an audit event
-- `policy.prove` in the same transaction.
--
-- Append-only: the triggers refuse UPDATE, DELETE and TRUNCATE for everyone. Not purged in v1 (the
-- spec's open question 8: purge at 90 days, or keep only the newest per bot, is the owner's); the
-- `policy.prove` audit events are purged at 90 days with the rest of the audit log.
-- (Number 0030 on this branch; another branch adds tool-server migrations too, so the conductor
-- may renumber it on merge.)

CREATE TABLE policy_proofs (
    id               bigserial PRIMARY KEY,
    at               timestamptz NOT NULL,
    bot              text NOT NULL CHECK (bot ~ '^[a-z0-9]([a-z0-9-]{0,30}[a-z0-9])?$' AND bot NOT LIKE '%--%'),
    status           text NOT NULL CHECK (status IN ('proved', 'exceeds', 'unsupported',
                         'inconclusive', 'error', 'not_available', 'version_mismatch')),
    reason_code      text CHECK (char_length(reason_code) <= 256),
    reason           text CHECK (char_length(reason) <= 256),
    coverage         text[] NOT NULL DEFAULT '{}',
    counterexample   text CHECK (char_length(counterexample) <= 256),
    prover_version   text CHECK (char_length(prover_version) <= 64),
    prover_sha256    text CHECK (prover_sha256 ~ '^[0-9a-f]{64}$'),
    boundary_sha256  text CHECK (boundary_sha256 ~ '^[0-9a-f]{64}$'),
    candidate_sha256 text CHECK (candidate_sha256 ~ '^[0-9a-f]{64}$'),
    host_user        text NOT NULL CHECK (host_user ~ '^[a-z_][a-z0-9_.-]{0,31}$'),
    -- Only `proved` may claim full coverage; a proved row without the five domains can't exist.
    CHECK (status <> 'proved' OR coverage @> ARRAY['filesystem', 'network_l4', 'network_rest',
                                                    'process', 'landlock']::text[])
);

-- The newest proof per bot (`GET /internal/admin/policy/proofs`).
CREATE INDEX policy_proofs_bot ON policy_proofs (bot, at DESC, id DESC);

CREATE FUNCTION policy_proofs_append_only() RETURNS trigger AS $$
BEGIN
    RAISE EXCEPTION 'policy_proofs is append-only' USING ERRCODE = 'insufficient_privilege';
END
$$ LANGUAGE plpgsql;

CREATE TRIGGER policy_proofs_append_only
    BEFORE UPDATE OR DELETE ON policy_proofs
    FOR EACH ROW EXECUTE FUNCTION policy_proofs_append_only();
CREATE TRIGGER policy_proofs_no_truncate
    BEFORE TRUNCATE ON policy_proofs
    FOR EACH STATEMENT EXECUTE FUNCTION policy_proofs_append_only();
