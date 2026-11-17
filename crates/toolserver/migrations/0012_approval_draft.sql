-- A person's saved edit of a pending draft (`PUT /internal/approvals/{id}/draft`, the web
-- app's Save). The edited arguments replace `args`, so every surface shows them and an
-- approve anywhere writes them; `drafted_args` keeps the bot's own arguments for the ledger's
-- edit distance (it is never served), and `edited_at` says when the edit was last saved.
-- Both are cleared with the text when a Slack draft expires or loses its text.
ALTER TABLE approvals ADD COLUMN drafted_args JSONB;
ALTER TABLE approvals ADD COLUMN edited_at TIMESTAMPTZ;
