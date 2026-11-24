-- MCP servers, slice M8 (docs/mcp-servers-spec.md §2.6, §2.9): which backend served an approval
-- item's write, and what left PM OS. Both on the item (approval items are never deleted); the
-- ledger reads `served_by` from its item, so the ledger table is unchanged.
--
-- `served_by`: `builtin`, `fake`, or the MCP server record's id, written when the item is drafted,
-- so the side-by-side period can compare outcomes by backend. NULL for items from before (all were
-- served by a built-in connector or its fake).
-- `upstream_args_hash`: for a write sent to an MCP server, the SHA-256 of the canonical JSON of
-- the upstream arguments (after mapping, the marker and the escaping), written just before the
-- call. NULL for a built-in connector's write. A hash only, never the text, so the 24-hour removal
-- of Slack text (SLK-8) is unaffected.

ALTER TABLE approvals
    ADD COLUMN served_by          text CHECK (char_length(served_by) <= 64),
    ADD COLUMN upstream_args_hash text CHECK (upstream_args_hash ~ '^[0-9a-f]{64}$');
