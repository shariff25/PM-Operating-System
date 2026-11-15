-- pmos-docs: the docs index. Rebuilt from the configured docs site by the sync job;
-- the site stays the source of truth.
CREATE TABLE docs_pages (
    url         text PRIMARY KEY,
    slug        text NOT NULL,
    title       text NOT NULL,
    section     text NOT NULL CHECK (section IN ('docs', 'api')),
    product     text NOT NULL,
    description text,
    headings    text NOT NULL DEFAULT '',
    body        text NOT NULL,
    updated     timestamptz,
    etag        text,
    fetched_at  timestamptz NOT NULL,
    tsv         tsvector GENERATED ALWAYS AS (
                    setweight(to_tsvector('english', title), 'A') ||
                    setweight(to_tsvector('english', coalesce(description, '') || ' ' || headings), 'B') ||
                    setweight(to_tsvector('english', body), 'C')
                ) STORED
);
CREATE INDEX docs_pages_tsv ON docs_pages USING gin (tsv);
CREATE INDEX docs_pages_product ON docs_pages (product);
