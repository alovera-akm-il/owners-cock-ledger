-- Per-link Keyholder configuration (01-data-model.md §3,
-- 03-api-design.md §2): whether the submissive may self-report a
-- confinement-session start/stop (default off — the Keyholder is the
-- system of record for lock/unlock events unless they opt a specific
-- submissive in), and whether the submissive can read the Keyholder's
-- catalog at all (default on — read-only transparency about what's
-- possible, 02-roles-and-permissions.md §3).
ALTER TABLE keyholder_submissive_links ADD COLUMN self_report_allowed INTEGER NOT NULL DEFAULT 0;
ALTER TABLE keyholder_submissive_links ADD COLUMN catalog_visible_to_submissive INTEGER NOT NULL DEFAULT 1;
