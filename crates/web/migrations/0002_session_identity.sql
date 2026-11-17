-- An Entra ID sign-in records the token's `oid` (the person's object id in the tenant) and
-- `tid` (the tenant) on the session, so a member whose team.yaml entry has `entra-oid` is
-- checked against it on every request. Development sign-in leaves both null.
ALTER TABLE web_sessions ADD COLUMN entra_oid text;
ALTER TABLE web_sessions ADD COLUMN entra_tid text;
