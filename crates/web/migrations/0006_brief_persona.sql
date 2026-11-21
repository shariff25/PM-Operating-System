-- Who wrote the PM's latest daily brief (docs/bot-marketplace-spec.md §8, "Chief and the daily
-- brief"; slice R2): a marketplace bot's key, NULL for Nudge. Sent by the Slack service with the
-- brief's block (PUT /internal/brief/{handle}) from the runtime's notification; shown on Today as
-- the brief's attribution, never used to decide anything.
ALTER TABLE web_brief_days ADD COLUMN persona text
    CHECK (persona IS NULL OR (persona ~ '^[a-z0-9]+(-[a-z0-9]+)*$' AND length(persona) <= 32));
