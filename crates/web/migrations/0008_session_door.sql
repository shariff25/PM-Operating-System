-- Slice H10 (docs/openshell-any-host-spec.md §3.8, "The admin listener's sign-in"): each session
-- records the front door it was made on, the browser-facing listener (`public`) or the admin-only
-- listener (`admin`), and each door accepts only its own. Every row made before this migration is
-- a public-door session: the default, so none of them can be used on the admin door.
ALTER TABLE web_sessions
    ADD COLUMN door text NOT NULL DEFAULT 'public' CHECK (door IN ('public', 'admin'));
