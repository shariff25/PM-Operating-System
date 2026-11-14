-- Bot sessions (docs/bot-runtime-spec.md §2): a session runs either one skill or one named
-- bot, never both. Approval items and audit events record which, so an item can be checked
-- against its bot's role and action settings when it is decided.
ALTER TABLE sessions ALTER COLUMN skill DROP NOT NULL;
ALTER TABLE sessions ADD COLUMN bot text;
ALTER TABLE sessions ADD CONSTRAINT sessions_skill_or_bot
    CHECK ((skill IS NULL) <> (bot IS NULL));

ALTER TABLE approvals ALTER COLUMN skill DROP NOT NULL;
ALTER TABLE approvals ADD COLUMN bot text;
ALTER TABLE approvals ADD CONSTRAINT approvals_skill_or_bot
    CHECK ((skill IS NULL) <> (bot IS NULL));

-- Schema changes don't fire the append-only row triggers; existing events keep their skill.
ALTER TABLE audit_events ALTER COLUMN skill DROP NOT NULL;
ALTER TABLE audit_events ADD COLUMN bot text;
