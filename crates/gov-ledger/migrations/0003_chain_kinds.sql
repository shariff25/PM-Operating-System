-- A chain holds one kind of document: a gate's receipts, or an Authority's admin records
-- (spec §7.6, review finding GR-108). A key is enrolled for one kind, and every key of a
-- chain is of that chain's kind: a key enrolled for admin records is never accepted on a
-- receipt, nor a gate's key on an admin record.
ALTER TABLE gate_keys ADD COLUMN kind TEXT NOT NULL DEFAULT 'receipt'
    CHECK (kind IN ('receipt', 'admin'));
