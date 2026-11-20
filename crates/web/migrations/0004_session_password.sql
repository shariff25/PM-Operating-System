-- A password sign-in (docs/password-sign-in.md) records the password credential's epoch on
-- the session; the session ends when the user database's epoch moves (a reset, a password
-- change). Every other sign-in leaves it null.
ALTER TABLE web_sessions ADD COLUMN password_epoch bigint;
