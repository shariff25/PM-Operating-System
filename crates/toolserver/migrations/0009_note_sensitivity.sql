-- A note carries the sensitivity of what it was built from (security fixes, S6): the highest
-- data class the writing session had read. Reading the note raises the reading session to
-- it. Null for notes written before this migration, which read as `internal`.
ALTER TABLE memory_notes ADD COLUMN sensitivity text
    CHECK (sensitivity IN ('public', 'internal', 'restricted'));
