-- Self-service link ending, request-not-action shape
-- (06-future-extensions.md §2): a submissive can ask to end the link,
-- the Keyholder has to act (or a 7-day timeout escalates it), rather
-- than a unilateral submissive click. Deliberately not a new `status`
-- value — a pending request doesn't change what's operative today
-- (tasks, confinement, verification all continue), it's metadata
-- *about* the relationship. Tier 2 (the admin force-end-link escape
-- hatch) already exists (`domain::links::force_end`); this is Tier 1.

ALTER TABLE keyholder_submissive_links ADD COLUMN end_requested_at INTEGER;
ALTER TABLE keyholder_submissive_links ADD COLUMN end_requested_by_user_id TEXT;
ALTER TABLE keyholder_submissive_links ADD COLUMN end_request_reason TEXT;
ALTER TABLE keyholder_submissive_links ADD COLUMN end_request_escalated_at INTEGER;
