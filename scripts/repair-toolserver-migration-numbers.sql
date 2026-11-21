-- Renumber two tool-server migrations in a database migrated by the `claude/web-app` branch
-- before it merged `main`'s security fixes (docs/runbooks/upgrade.md, "Tool-server migrations
-- renumbered"). That branch applied `0009_audit_source_titles` and `0010_audit_admin_reads`;
-- they are now `0010` and `0011`, with the same SQL, and `main`'s `0009_note_sensitivity`
-- comes before them.
--
-- DEV AND TEST STACKS OF THAT BRANCH ONLY. The reset in the runbook is the preferred path.
--
-- Guarded: it changes nothing unless the database is in exactly that state: version 9 is
-- `audit source titles` and version 10 `audit admin reads`, both applied successfully, each
-- with the checksum of its file, and nothing after 10. Anything else (already repaired, never
-- affected, a different history) is refused and rolled back. All in one transaction.
--
-- Run it with psql against the tool server's database, with ON_ERROR_STOP:
--   psql -v ON_ERROR_STOP=1 -d pmos_toolserver -f scripts/repair-toolserver-migration-numbers.sql
-- then start the tool server, which applies `0009_note_sensitivity`.

BEGIN;

LOCK TABLE _sqlx_migrations IN ACCESS EXCLUSIVE MODE;

DO $$
DECLARE
    -- SHA-384 of crates/toolserver/migrations/0010_audit_source_titles.sql (the old 0009)
    -- and 0011_audit_admin_reads.sql (the old 0010): sqlx's checksums.
    titles_sum CONSTANT bytea := decode('58493139071ea0fdee8a1f2ab45c154599db22f7d2552097702f916d34874d6bc9f275bc9cc7b4a3befde199eebd4fc3', 'hex');
    reads_sum CONSTANT bytea := decode('9c8b228b8da02f42f6cc464966e8033edafbf2ced66036a1f2bbcc79c6adfbfd912ab78f3d43c36c1df9dfe0bc04408a', 'hex');
    v9_ok boolean;
    v10_ok boolean;
    later bigint;
BEGIN
    SELECT count(*) = 1 INTO v9_ok FROM _sqlx_migrations
     WHERE version = 9 AND description = 'audit source titles' AND success
       AND checksum = titles_sum;
    SELECT count(*) = 1 INTO v10_ok FROM _sqlx_migrations
     WHERE version = 10 AND description = 'audit admin reads' AND success
       AND checksum = reads_sum;
    SELECT count(*) INTO later FROM _sqlx_migrations WHERE version > 10;
    IF NOT v9_ok OR NOT v10_ok OR later <> 0 THEN
        RAISE EXCEPTION 'not the expected state (version 9 matches: %, version 10 matches: %, later versions: %): nothing changed. Use the reset in docs/runbooks/upgrade.md.',
            v9_ok, v10_ok, later;
    END IF;
    -- 10 first, so the primary key never collides.
    UPDATE _sqlx_migrations SET version = 11 WHERE version = 10;
    UPDATE _sqlx_migrations SET version = 10 WHERE version = 9;
    RAISE NOTICE 'renumbered: audit source titles is 10, audit admin reads is 11';
END
$$;

COMMIT;
