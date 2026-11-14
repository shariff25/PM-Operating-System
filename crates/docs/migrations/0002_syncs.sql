-- One row per finished sync, so a restart knows when the index was last refreshed and
-- doesn't re-crawl the portal on every boot.
CREATE TABLE docs_syncs (
    id          bigserial PRIMARY KEY,
    finished_at timestamptz NOT NULL,
    ok          boolean NOT NULL,
    report      jsonb NOT NULL
);
CREATE INDEX docs_syncs_ok ON docs_syncs (finished_at DESC) WHERE ok;
