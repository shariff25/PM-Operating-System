-- The bot the ask box's picker last asked (docs/bot-marketplace-spec.md §5): a persona key, NULL
-- for Nudge. A preference only, never a decision: GET /api/me falls back to Nudge when the bot
-- is no longer on the PM's team, and the tool server checks every ask again at claim.
ALTER TABLE web_user_settings ADD COLUMN ask_persona text
    CHECK (ask_persona IS NULL OR (ask_persona ~ '^[a-z0-9]+(-[a-z0-9]+)*$' AND length(ask_persona) <= 32));
