-- A short title for what a read returned (docs/toolserver.md, What an item is shown with):
-- a Jira issue's summary, a Confluence page's title, a docs page's title, a context file's
-- title, keyed by the event's `sources` entry, such as {"jira:PAY-212": "Grace period"}.
-- At most 120 characters each, control characters removed. Never for Slack, whose text may
-- be kept only 24 hours: Slack sources stay channel and message ids. No body, snippet or
-- excerpt is ever recorded.
--
-- Nullable and with no default, so existing rows are not rewritten (they read as NULL: no
-- titles) and the append-only triggers on audit_events are untouched.
ALTER TABLE audit_events ADD COLUMN source_titles jsonb;
