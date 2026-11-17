-- A Slack draft's channel as `conversations.info` last described it (docs/toolserver.md,
-- "Slack draft targets"): `{"channel": "C…", "kind": "channel" | "private_channel" | "im" |
-- "mpim", "name"?: "launch-acme", "peer"?: "U…", "checked_at": "…"}`. Recorded when the draft
-- is made and refreshed at every audience check, so a renamed channel reads its new name
-- after the next check. Names are metadata, not message text (no Slack text is ever stored
-- here); they come from Slack, so only a name shaped like a channel name is kept. Null for
-- other items, and for a draft whose channel couldn't be read.
ALTER TABLE approvals ADD COLUMN target_conversation JSONB;
