-- A gate's key is retired when the Authority's chain says the gate is disabled or the key
-- revoked. A retired key stays, so that receipts already taken under it still verify; no
-- further receipt is taken under it.
ALTER TABLE gate_keys ADD COLUMN retired BOOLEAN NOT NULL DEFAULT FALSE;
