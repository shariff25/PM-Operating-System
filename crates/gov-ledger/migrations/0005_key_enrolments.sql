-- What an Authority's chain says of each gate key id, kept as its records are taken in, so
-- that a key offered to the Ledger is one lookup and not a reading of whole chains (review
-- finding GR-124). A row is written once; `ended` is set when the gate is disabled or the key
-- revoked, and never unset.
CREATE TABLE key_enrolments (
    tenant    TEXT NOT NULL,
    env       TEXT NOT NULL,
    kid       TEXT NOT NULL,
    authority TEXT NOT NULL,
    gate      TEXT NOT NULL,
    digest    TEXT NOT NULL,
    ended     BOOLEAN NOT NULL DEFAULT FALSE,
    PRIMARY KEY (tenant, env, kid)
);
