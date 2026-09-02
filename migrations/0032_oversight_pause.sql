-- Oversight pause (06-future-extensions.md §13): a bulk pause on the
-- link itself, one level up from the existing per-session confinement
-- pause — freezes task/punishment deadline auto-fail and new
-- verification code issuance for a Keyholder who has gone genuinely
-- unreachable, instead of extending every open deadline by hand.
ALTER TABLE keyholder_submissive_links ADD COLUMN oversight_paused_at INTEGER;
ALTER TABLE keyholder_submissive_links ADD COLUMN oversight_pause_message TEXT;
