-- Whether the web app draws Nudge, its mascot ("Show Nudge" in the user menu). Off hides the
-- character only: hints and empty states keep their text, and `nudges` is unchanged.
ALTER TABLE web_user_settings ADD COLUMN mascot boolean NOT NULL DEFAULT true;
